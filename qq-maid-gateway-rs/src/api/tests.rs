use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    thread,
    time::Duration,
};

use super::*;
use crate::{markdown::MarkdownPayload, media::ImagePayload, render::OutboundMessage};

#[test]
fn extracts_sent_message_id_from_common_response_shapes() {
    assert_eq!(
        extract_sent_message_id(r#"{"id":"msg-1"}"#).as_deref(),
        Some("msg-1")
    );
    assert_eq!(
        extract_sent_message_id(r#"{"data":{"message_id":"msg-2"}}"#).as_deref(),
        Some("msg-2")
    );
    assert_eq!(
        extract_sent_message_id(r#"{"d":{"msg_id":"msg-3"}}"#).as_deref(),
        Some("msg-3")
    );
    assert_eq!(
        extract_sent_message_id(r#"{"message":{"id":"msg-4"}}"#).as_deref(),
        Some("msg-4")
    );
    assert_eq!(extract_sent_message_id(r#"{"ok":true}"#), None);
}

#[test]
fn extracts_message_id_and_refidx_without_mixing_semantics() {
    let ids = extract_sent_message_ids(r#"{"id":"bot-msg-1","msg_idx":"REFIDX_bot_1"}"#);
    assert_eq!(
        ids,
        SendMessageIds {
            message_id: Some("bot-msg-1".to_owned()),
            ref_index_id: Some("REFIDX_bot_1".to_owned()),
        }
    );

    let nested = extract_sent_message_ids(
        r#"{"data":{"message_id":"bot-msg-2","ref_msg_idx":"REFIDX_bot_2"}}"#,
    );
    assert_eq!(nested.message_id.as_deref(), Some("bot-msg-2"));
    assert_eq!(nested.ref_index_id.as_deref(), Some("REFIDX_bot_2"));

    let official =
        extract_sent_message_ids(r#"{"id":"bot-msg-3","ext_info":{"ref_idx":"REFIDX_bot_3"}}"#);
    assert_eq!(official.message_id.as_deref(), Some("bot-msg-3"));
    assert_eq!(official.ref_index_id.as_deref(), Some("REFIDX_bot_3"));
}

#[test]
fn c2c_text_payload_matches_qq_shape() {
    let payload = build_c2c_text_payload("hello", Some("msg-1"), 7);

    assert_eq!(payload["content"], "hello");
    assert_eq!(payload["msg_type"], 0);
    assert_eq!(payload["msg_id"], "msg-1");
    assert_eq!(payload["msg_seq"], 7);
}

#[test]
fn c2c_typing_payload_uses_native_typing_message_type() {
    let payload = build_c2c_typing_payload(Some("msg-1"), 8);

    assert_eq!(payload["msg_type"], 6);
    assert_eq!(payload["input_notify"]["input_type"], 1);
    assert_eq!(payload["input_notify"]["input_second"], 60);
    assert_eq!(payload["msg_id"], "msg-1");
    assert_eq!(payload["msg_seq"], 8);
    assert!(payload.get("content").is_none());
    assert!(payload.get("markdown").is_none());
    assert!(payload.get("stream").is_none());
}

#[test]
fn official_c2c_stream_payload_uses_cumulative_replace_shape() {
    let first_state = C2cStreamTransportState::new();
    let first = build_c2c_stream_payload("你好", "msg-1", 6, &first_state, 1);
    assert_eq!(first["input_mode"], "replace");
    assert_eq!(first["input_state"], 1);
    assert_eq!(first["content_type"], "markdown");
    assert_eq!(first["content_raw"], "你好");
    assert_eq!(first["event_id"], "msg-1");
    assert_eq!(first["msg_id"], "msg-1");
    assert_eq!(first["msg_seq"], 6);
    assert_eq!(first["index"], 0);
    assert!(first.get("stream_msg_id").is_none());
    assert!(first.get("markdown").is_none());
    assert!(first.get("stream").is_none());
    assert!(first.get("msg_type").is_none());

    let middle_state = C2cStreamTransportState {
        stream_msg_id: Some("stream-1".to_owned()),
        msg_seq: Some(6),
        index: 1,
    };
    let middle = build_c2c_stream_payload("你好，这是", "msg-1", 6, &middle_state, 1);
    assert_eq!(middle["content_raw"], "你好，这是");
    assert_eq!(middle["stream_msg_id"], "stream-1");
    assert_eq!(middle["msg_seq"], 6);
    assert_eq!(middle["index"], 1);

    let final_payload = build_c2c_stream_payload(
        "你好，这是最终内容",
        "msg-1",
        6,
        &C2cStreamTransportState {
            stream_msg_id: Some("stream-1".to_owned()),
            msg_seq: Some(6),
            index: 2,
        },
        10,
    );
    assert_eq!(final_payload["input_state"], 10);
    assert_eq!(final_payload["content_raw"], "你好，这是最终内容");
    assert_eq!(final_payload["stream_msg_id"], "stream-1");
    assert_eq!(final_payload["index"], 2);
}

fn read_http_request(stream: &mut TcpStream) -> (String, String) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "test HTTP server received an incomplete request");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.strip_prefix("content-length:")
                .or_else(|| line.strip_prefix("Content-Length:"))
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "test HTTP server received an incomplete body");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let body = String::from_utf8(bytes[header_end..header_end + content_length].to_vec()).unwrap();
    (path, body)
}

#[tokio::test]
async fn c2c_stream_client_posts_official_endpoint_and_commits_only_successful_cursor() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let responses = [
            r#"{"id":"stream-1","timestamp":1}"#,
            r#"{"id":"reply-2","timestamp":2}"#,
            r#"{"id":"reply-3","timestamp":3,"ext_info":{"ref_idx":"REFIDX_3"}}"#,
        ];
        let mut requests = Vec::new();
        for response_body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState::new();
    let first = client
        .send_c2c_stream_message("user-openid", Some("source-msg"), "你好", &mut state, 1)
        .await
        .unwrap();
    let second = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "你好，这是",
            &mut state,
            1,
        )
        .await
        .unwrap();
    let complete = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "你好，这是最终",
            &mut state,
            10,
        )
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(first.message_id, "stream-1");
    assert_eq!(second.message_id, "reply-2");
    assert_eq!(complete.ref_index_id.as_deref(), Some("REFIDX_3"));
    assert_eq!(state.stream_msg_id.as_deref(), Some("stream-1"));
    assert_eq!(state.msg_seq, Some(1));
    assert_eq!(state.index, 3);

    let bodies = requests
        .into_iter()
        .map(|(path, body)| {
            assert_eq!(path, "/v2/users/user-openid/stream_messages");
            serde_json::from_str::<Value>(&body).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["input_mode"], "replace");
    assert_eq!(bodies[0]["input_state"], 1);
    assert_eq!(bodies[0]["content_raw"], "你好");
    assert!(bodies[0].get("stream_msg_id").is_none());
    assert_eq!(bodies[0]["index"], 0);
    assert_eq!(bodies[1]["content_raw"], "你好，这是");
    assert_eq!(bodies[1]["stream_msg_id"], "stream-1");
    assert_eq!(bodies[1]["msg_seq"], bodies[0]["msg_seq"]);
    assert_eq!(bodies[1]["index"], 1);
    assert_eq!(bodies[2]["input_state"], 10);
    assert_eq!(bodies[2]["content_raw"], "你好，这是最终");
    assert_eq!(bodies[2]["stream_msg_id"], "stream-1");
    assert_eq!(bodies[2]["index"], 2);
}

#[tokio::test]
async fn c2c_stream_client_uses_next_index_after_non_retryable_http_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let responses = [
            (
                "400 Bad Request",
                r#"{"err_code":40054014,"message":"stream content too long"}"#,
            ),
            ("200 OK", r#"{"id":"complete-reply"}"#),
        ];
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState {
        stream_msg_id: Some("stream-1".to_owned()),
        msg_seq: Some(9),
        index: 4,
    };
    let failed = client
        .send_c2c_stream_message("user-openid", Some("source-msg"), "累计正文", &mut state, 1)
        .await;
    let complete = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "累计正文",
            &mut state,
            10,
        )
        .await;
    let requests = server.join().unwrap();

    assert!(matches!(failed, Err(ApiError::Status { .. })));
    assert_eq!(complete.unwrap().message_id, "complete-reply");
    assert_eq!(state.stream_msg_id.as_deref(), Some("stream-1"));
    assert_eq!(state.msg_seq, Some(9));
    assert_eq!(state.index, 6);

    let bodies = requests
        .into_iter()
        .map(|(path, body)| {
            assert_eq!(path, "/v2/users/user-openid/stream_messages");
            serde_json::from_str::<Value>(&body).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["index"], 4);
    assert_eq!(bodies[1]["index"], 5);
    assert_eq!(bodies[0]["msg_seq"], bodies[1]["msg_seq"]);
    assert_eq!(bodies[0]["input_state"], 1);
    assert_eq!(bodies[1]["input_state"], 10);
}

#[tokio::test]
async fn c2c_stream_client_uses_next_index_after_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut failed_stream, _) = listener.accept().unwrap();
        let failed_request = read_http_request(&mut failed_stream);
        drop(failed_stream);

        let (mut complete_stream, _) = listener.accept().unwrap();
        let complete_request = read_http_request(&mut complete_stream);
        let body = r#"{"id":"complete-after-network-error"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
        );
        complete_stream.write_all(response.as_bytes()).unwrap();
        vec![failed_request, complete_request]
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState {
        stream_msg_id: Some("stream-1".to_owned()),
        msg_seq: Some(9),
        index: 4,
    };
    let failed = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "网络错误前缀",
            &mut state,
            1,
        )
        .await;
    let complete = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "网络错误前缀",
            &mut state,
            10,
        )
        .await;
    let requests = server.join().unwrap();

    assert!(matches!(failed, Err(ApiError::Http(_))));
    assert_eq!(complete.unwrap().message_id, "complete-after-network-error");
    assert_eq!(state.stream_msg_id.as_deref(), Some("stream-1"));
    assert_eq!(state.msg_seq, Some(9));
    assert_eq!(state.index, 6);

    let bodies = requests
        .into_iter()
        .map(|(_, body)| serde_json::from_str::<Value>(&body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["index"], 4);
    assert_eq!(bodies[1]["index"], 5);
    assert_eq!(bodies[0]["msg_seq"], bodies[1]["msg_seq"]);
    assert_eq!(bodies[1]["input_state"], 10);
}

#[tokio::test]
async fn c2c_stream_client_retries_http_429_with_a_new_index() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let responses = [
            (
                "429 Too Many Requests",
                r#"{"code":429,"message":"rate limited"}"#,
            ),
            ("200 OK", r#"{"id":"stream-after-429"}"#),
        ];
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState::new();
    let result = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "限流后重试",
            &mut state,
            1,
        )
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(result.message_id, "stream-after-429");
    assert_eq!(state.stream_msg_id.as_deref(), Some("stream-after-429"));
    assert_eq!(state.msg_seq, Some(1));
    assert_eq!(state.index, 2);

    let bodies = requests
        .into_iter()
        .map(|(_, body)| serde_json::from_str::<Value>(&body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["index"], 0);
    assert_eq!(bodies[1]["index"], 1);
    assert_eq!(bodies[0]["msg_seq"], bodies[1]["msg_seq"]);
}

#[tokio::test]
async fn c2c_stream_client_retries_qq_50002_with_a_new_index() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let responses = [
            (
                "400 Bad Request",
                r#"{"err_code":50002,"message":"rate limited"}"#,
            ),
            (
                "200 OK",
                r#"{"id":"stream-after-50002","ext_info":{"ref_idx":"REFIDX_50002"}}"#,
            ),
        ];
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState::new();
    let result = client
        .send_c2c_stream_message(
            "user-openid",
            Some("source-msg"),
            "QQ 限流后重试",
            &mut state,
            1,
        )
        .await
        .unwrap();
    let requests = server.join().unwrap();

    assert_eq!(result.message_id, "stream-after-50002");
    assert_eq!(result.ref_index_id.as_deref(), Some("REFIDX_50002"));
    assert_eq!(state.stream_msg_id.as_deref(), Some("stream-after-50002"));
    assert_eq!(state.msg_seq, Some(1));
    assert_eq!(state.index, 2);

    let bodies = requests
        .into_iter()
        .map(|(_, body)| serde_json::from_str::<Value>(&body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["index"], 0);
    assert_eq!(bodies[1]["index"], 1);
    assert_eq!(bodies[0]["msg_seq"], bodies[1]["msg_seq"]);
}

#[tokio::test]
async fn c2c_stream_client_stops_after_limited_rate_limit_retries() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let body = r#"{"err_code":50002,"message":"rate limited"}"#;
        let mut requests = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });

    let auth = crate::auth::AccessTokenManager::new_with_cached_token_for_test(
        qq_maid_common::http_client::client(),
        "app-id",
        "app-secret",
        Duration::from_secs(5),
        "token",
        Duration::from_secs(60),
    );
    let client = QqApiClient::new(
        qq_maid_common::http_client::client(),
        format!("http://{address}"),
        auth,
    );
    let mut state = C2cStreamTransportState::new();
    let result = client
        .send_c2c_stream_message("user-openid", Some("source-msg"), "限流终止", &mut state, 1)
        .await;
    let requests = server.join().unwrap();

    assert!(matches!(result, Err(ApiError::Status { .. })));
    assert_eq!(state.index, 4);
    assert_eq!(state.msg_seq, Some(1));
    assert!(state.stream_msg_id.is_none());
    let indexes = requests
        .into_iter()
        .map(|(_, body)| {
            serde_json::from_str::<Value>(&body).unwrap()["index"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(indexes, vec![0, 1, 2, 3]);
}

#[test]
fn official_stream_response_extracts_ref_idx_only_from_ext_info() {
    assert_eq!(
        extract_c2c_stream_response(
            r#"{"id":"reply-1","timestamp":1700000000,"ext_info":{"ref_idx":"REFIDX_1"}}"#
        ),
        Some(("reply-1".to_owned(), Some("REFIDX_1".to_owned())))
    );
    assert_eq!(
        extract_c2c_stream_response(r#"{"id":"reply-2","msg_id":"wrong"}"#),
        Some(("reply-2".to_owned(), None))
    );
    assert_eq!(extract_c2c_stream_response(r#"{"msg_id":"wrong"}"#), None);
}

#[test]
fn qq_error_fields_accept_err_code_and_redact_sensitive_text() {
    let body = serde_json::json!({
        "err_code": 40054014,
        "message": "stream content too long; OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456"
    })
    .to_string();
    let (code, message) = qq_api_error_fields(&body);
    assert_eq!(code.as_deref(), Some("40054014"));
    assert!(message.unwrap().contains("OPENAI_API_KEY=<redacted>"));
}

#[derive(Debug, Default)]
struct MockSender {
    calls: Mutex<Vec<String>>,
}

impl MockSender {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl OutboundSender for MockSender {
    fn send_text<'a>(&'a self, _target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("text:{text}"));
            Ok(SendMessageIds::none())
        })
    }

    fn send_markdown<'a>(
        &'a self,
        _target: &'a C2cReplyTarget,
        _markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("markdown".to_owned());
            Err(ApiError::Unsupported("markdown"))
        })
    }

    fn send_image<'a>(
        &'a self,
        _target: &'a C2cReplyTarget,
        _image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("image".to_owned());
            Err(ApiError::Unsupported("image"))
        })
    }
}

impl GroupOutboundSender for MockSender {
    fn send_text<'a>(&'a self, _target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("group-text:{text}"));
            Ok(SendMessageIds::none())
        })
    }

    fn send_markdown<'a>(
        &'a self,
        _target: &'a GroupReplyTarget,
        _markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("group-markdown".to_owned());
            Err(ApiError::Unsupported("markdown"))
        })
    }
}

fn target() -> C2cReplyTarget {
    C2cReplyTarget {
        user_openid: "user-1".to_owned(),
        msg_id: Some("msg-1".to_owned()),
    }
}

fn group_target() -> GroupReplyTarget {
    GroupReplyTarget {
        group_openid: "group-1".to_owned(),
        msg_id: Some("msg-1".to_owned()),
    }
}

#[tokio::test]
async fn send_failure_falls_back_to_text() {
    let cases = [
        (
            OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("# hello"),
                fallback_text: "hello".to_owned(),
            },
            vec!["markdown", "text:hello"],
        ),
        (
            OutboundMessage::Image {
                image: ImagePayload::new("file-info"),
                fallback_text: "image fallback".to_owned(),
            },
            vec!["image", "text:image fallback"],
        ),
    ];

    for (outbound, expected_calls) in cases {
        let sender = MockSender::default();
        send_outbound_with_fallback(&sender, &target(), &outbound)
            .await
            .unwrap();
        assert_eq!(sender.calls(), expected_calls);
    }
}

#[tokio::test]
async fn group_markdown_send_failure_falls_back_to_text() {
    let sender = MockSender::default();
    let outbound = OutboundMessage::Markdown {
        markdown: MarkdownPayload::new("# hello"),
        fallback_text: "hello".to_owned(),
    };
    send_group_outbound_with_fallback(&sender, &group_target(), &outbound)
        .await
        .unwrap();
    assert_eq!(sender.calls(), vec!["group-markdown", "group-text:hello"]);
}
