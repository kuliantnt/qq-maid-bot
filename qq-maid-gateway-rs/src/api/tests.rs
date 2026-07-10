use std::sync::Mutex;

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
    assert_eq!(
        extract_sent_message_id(r#"{"id":"bot-msg-1","msg_idx":"REFIDX_bot_1"}"#).as_deref(),
        Some("bot-msg-1")
    );
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
fn c2c_markdown_stream_payload_matches_reference_shape() {
    let first_markdown = MarkdownPayload::new("**hello**");
    let first_payload = build_c2c_markdown_stream_payload(
        &first_markdown,
        Some("msg-1"),
        6,
        &C2cStreamState {
            stream_id: None,
            index: 0,
            ..C2cStreamState::new()
        },
        1,
        Some(false),
    );
    assert_eq!(first_payload["msg_type"], 2);
    assert_eq!(first_payload["markdown"]["content"], "**hello**");
    assert_eq!(first_payload["msg_id"], "msg-1");
    assert_eq!(first_payload["msg_seq"], 6);
    assert!(first_payload.get("content").is_none());
    assert!(first_payload["stream"]["id"].is_null());
    assert_eq!(first_payload["stream"]["index"], 0);
    assert_eq!(first_payload["stream"]["state"], 1);
    assert!(first_payload["stream"].get("done").is_none());
    assert!(first_payload["stream"].get("type").is_none());
    assert_eq!(first_payload["stream"]["reset"], false);

    let middle_markdown = MarkdownPayload::new(" delta");
    let middle_payload = build_c2c_markdown_stream_payload(
        &middle_markdown,
        Some("msg-1"),
        7,
        &C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 1,
            ..C2cStreamState::new()
        },
        1,
        Some(false),
    );

    // 被动回复 msg_id 和流式续接 id 分属两个协议字段，缺一都会导致 QQ 端退化或续接失败。
    assert_eq!(middle_payload["msg_type"], 2);
    assert_eq!(middle_payload["markdown"]["content"], " delta");
    assert!(middle_payload.get("content").is_none());
    assert_eq!(middle_payload["stream"]["id"], "stream-1");
    assert_eq!(middle_payload["stream"]["index"], 1);
    assert_eq!(middle_payload["stream"]["state"], 1);
    assert!(middle_payload["stream"].get("done").is_none());
    assert!(middle_payload["stream"].get("type").is_none());
    assert_eq!(middle_payload["stream"]["reset"], false);

    let middle_json = serde_json::to_string(&middle_payload).unwrap();
    assert!(middle_json.contains("\"state\":1"));
    assert!(!middle_json.contains("\"type\":1"));

    let final_markdown = MarkdownPayload::new("**hello** delta");
    let final_payload = build_c2c_markdown_stream_payload(
        &final_markdown,
        Some("msg-1"),
        8,
        &C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 2,
            ..C2cStreamState::new()
        },
        10,
        Some(false),
    );
    assert_eq!(final_payload["msg_type"], 2);
    assert_eq!(final_payload["markdown"]["content"], "**hello** delta");
    assert!(final_payload.get("content").is_none());
    assert_eq!(final_payload["stream"]["id"], "stream-1");
    assert_eq!(final_payload["stream"]["index"], 2);
    assert_eq!(final_payload["stream"]["state"], 10);
    assert_eq!(final_payload["stream"]["reset"], false);
    assert!(final_payload["stream"].get("done").is_none());
    assert!(final_payload["stream"].get("type").is_none());

    let final_json = serde_json::to_string(&final_payload).unwrap();
    assert!(final_json.contains("\"state\":10"));
    assert!(final_json.contains("\"id\":\"stream-1\""));
    assert!(final_json.contains("\"index\":2"));
    assert!(final_json.contains("\"reset\":false"));
    assert!(!final_json.contains("\"type\":10"));
    assert!(!final_json.contains("\"done\""));
    assert!(final_json.contains("\"markdown\":{"));
    assert!(final_json.contains("\"content\":\"**hello** delta\""));
    assert_ne!(middle_payload["msg_seq"], final_payload["msg_seq"]);
}

#[test]
fn c2c_stream_response_uses_typed_top_level_id_only() {
    assert_eq!(
        extract_c2c_text_stream_id(r#"{"id":"stream-1","code":0}"#).as_deref(),
        Some("stream-1")
    );
    assert_eq!(
        extract_c2c_text_stream_id(r#"{"data":{"id":"ordinary-message"}}"#),
        None
    );
    assert_eq!(extract_c2c_text_stream_id(r#"{"msg_id":"msg-1"}"#), None);
}

#[test]
fn stream_request_log_fields_report_index_commit_semantics() {
    let first_state = C2cStreamState {
        stream_id: None,
        index: 0,
        ..C2cStreamState::new()
    };
    let first_attempt = C2cStreamMsgSeqAttempt {
        key: C2cStreamMsgSeqKey {
            state: 1,
            stream_index: Some(0),
        },
        msg_seq: 11,
        previous_success_msg_seq: None,
    };
    assert_eq!(
        stream_request_log_fields(1, &first_state, first_attempt, true),
        StreamRequestLogFields {
            previous_success_index: None,
            next_index: 1,
            msg_seq: 11,
            previous_success_msg_seq: None,
            index_committed: true,
            msg_seq_committed: true,
        }
    );

    let middle_state = C2cStreamState {
        stream_id: Some("stream-1".to_owned()),
        index: 1,
        ..C2cStreamState::new()
    };
    let middle_attempt = C2cStreamMsgSeqAttempt {
        key: C2cStreamMsgSeqKey {
            state: 1,
            stream_index: Some(1),
        },
        msg_seq: 12,
        previous_success_msg_seq: Some(11),
    };
    assert_eq!(
        stream_request_log_fields(1, &middle_state, middle_attempt, false),
        StreamRequestLogFields {
            previous_success_index: Some(0),
            next_index: 1,
            msg_seq: 12,
            previous_success_msg_seq: Some(11),
            index_committed: false,
            msg_seq_committed: false,
        }
    );

    // 终包也携带连续 index；只有 QQ 明确接受后才提交 next_index。
    let final_state = C2cStreamState {
        stream_id: Some("stream-1".to_owned()),
        index: 2,
        ..C2cStreamState::new()
    };
    let final_attempt = C2cStreamMsgSeqAttempt {
        key: C2cStreamMsgSeqKey {
            state: 10,
            stream_index: None,
        },
        msg_seq: 13,
        previous_success_msg_seq: Some(12),
    };
    assert_eq!(
        stream_request_log_fields(10, &final_state, final_attempt, true),
        StreamRequestLogFields {
            previous_success_index: Some(1),
            msg_seq: 13,
            previous_success_msg_seq: Some(12),
            next_index: 3,
            index_committed: true,
            msg_seq_committed: true,
        }
    );
}

#[test]
fn stream_msg_seq_reuses_same_value_for_same_failed_request_retry() {
    let mut state = C2cStreamState {
        stream_id: Some("stream-1".to_owned()),
        index: 1,
        ..C2cStreamState::new()
    };
    let mut next = 40;

    let first = state.begin_msg_seq_attempt(1, || {
        next += 1;
        next
    });
    let retry = state.begin_msg_seq_attempt(1, || {
        next += 1;
        next
    });

    assert_eq!(first.msg_seq, 41);
    assert_eq!(retry.msg_seq, 41);
    assert_eq!(next, 41);
    assert_eq!(retry.previous_success_msg_seq, None);
}

#[test]
fn stream_final_msg_seq_does_not_reuse_previous_success_or_failed_middle() {
    let mut state = C2cStreamState {
        stream_id: Some("stream-1".to_owned()),
        index: 1,
        ..C2cStreamState::new()
    };
    let mut next = 50;

    let middle = state.begin_msg_seq_attempt(1, || {
        next += 1;
        next
    });
    state.commit_msg_seq_attempt(middle);
    state.index = 2;
    let failed_middle_retry_key = state.begin_msg_seq_attempt(1, || {
        next += 1;
        next
    });
    let final_attempt = state.begin_msg_seq_attempt(10, || {
        next += 1;
        next
    });

    assert_ne!(middle.msg_seq, final_attempt.msg_seq);
    assert_ne!(failed_middle_retry_key.msg_seq, final_attempt.msg_seq);
    assert_eq!(final_attempt.previous_success_msg_seq, Some(middle.msg_seq));
    assert!(final_attempt.key.stream_index.is_none());
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

/// 合并 2 个 send 回退测试为表驱动测试。
#[tokio::test]
async fn send_failure_falls_back_to_text() {
    struct Case {
        name: &'static str,
        outbound: OutboundMessage,
        expected_calls: &'static [&'static str],
    }

    let cases = [
        Case {
            name: "markdown_send_failure_falls_back_to_text",
            outbound: OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("# hello"),
                fallback_text: "hello".to_owned(),
            },
            expected_calls: &["markdown", "text:hello"],
        },
        Case {
            name: "image_send_failure_falls_back_to_text",
            outbound: OutboundMessage::Image {
                image: ImagePayload::new("file-info"),
                fallback_text: "image fallback".to_owned(),
            },
            expected_calls: &["image", "text:image fallback"],
        },
    ];

    for case in &cases {
        let sender = MockSender::default();
        send_outbound_with_fallback(&sender, &target(), &case.outbound)
            .await
            .unwrap_or_else(|e| panic!("case '{}' failed: {:?}", case.name, e));
        assert_eq!(
            sender.calls(),
            case.expected_calls,
            "case '{}' failed: calls mismatch",
            case.name
        );
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
