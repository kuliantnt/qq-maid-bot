use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use qq_maid_common::input_part::MessageMedia;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};

#[derive(Debug)]
struct MockChatState {
    bodies: Vec<String>,
    status: StatusCode,
    requests: Vec<Value>,
}

async fn mock_chat_handler(
    State(state): State<Arc<Mutex<MockChatState>>>,
    body: Body,
) -> impl IntoResponse {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    let mut state = state.lock().await;
    state.requests.push(request);
    let body = state.bodies.remove(0);
    (
        state.status,
        [(header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

async fn spawn_mock_chat(
    bodies: Vec<String>,
    status: StatusCode,
) -> (String, Arc<Mutex<MockChatState>>) {
    let state = Arc::new(Mutex::new(MockChatState {
        bodies,
        status,
        requests: Vec::new(),
    }));
    let app = Router::new()
        .route("/v1/chat/completions", post(mock_chat_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state)
}

fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

#[test]
fn chat_payload_keeps_byte_prefix_stable_between_compactions() {
    let stable = vec![
        ChatMessage::system("固定 system"),
        ChatMessage::system("摘要 revision 3"),
        ChatMessage::user("历史用户"),
        ChatMessage {
            role: ChatRole::Assistant,
            content: "历史助手".to_owned(),
            content_parts: Vec::new(),
        },
    ];
    let mut first_messages = stable.clone();
    first_messages.extend([
        ChatMessage::system("动态时间一"),
        ChatMessage::user("本轮一"),
    ]);
    let mut second_messages = stable;
    second_messages.extend([
        ChatMessage::user("本轮一"),
        ChatMessage {
            role: ChatRole::Assistant,
            content: "回复一".to_owned(),
            content_parts: Vec::new(),
        },
        ChatMessage::system("动态时间二"),
        ChatMessage::user("本轮二"),
    ]);

    let first = chat_completions_payload(&first_messages, "gpt-test", 1024, 1200, false).unwrap();
    let second = chat_completions_payload(&second_messages, "gpt-test", 1024, 1200, false).unwrap();
    let first_bytes = serde_json::to_vec(&first).unwrap();
    let second_bytes = serde_json::to_vec(&second).unwrap();
    let history_end = first_bytes
        .windows("历史助手".len())
        .position(|window| window == "历史助手".as_bytes())
        .unwrap()
        + "历史助手".len();

    assert!(common_prefix_len(&first_bytes, &second_bytes) >= history_end);

    let streaming =
        chat_completions_payload(&first_messages, "gpt-test", 1024, 1200, true).unwrap();
    assert_eq!(first["messages"], streaming["messages"]);
}

#[test]
fn chat_completions_payload_keeps_reply_context_before_image_parts() {
    let payload = chat_completions_payload(
        &[ChatMessage::user_with_parts(
            "[reply message_id=quoted-1]\n上一条\n[/reply]\n看图",
            vec![
                MessageInputPart::text("[reply message_id=quoted-1]\n上一条\n[/reply]\n"),
                MessageInputPart::text("看图"),
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/jpeg".to_owned()),
                    filename: Some("a.jpg".to_owned()),
                    url: Some("https://example.test/a.jpg".to_owned()),
                    ..Default::default()
                }),
            ],
        )],
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        false,
    )
    .unwrap();
    let content = payload["messages"][0]["content"].as_array().unwrap();

    assert_eq!(content[0]["type"], "text");
    assert_eq!(
        content[0]["text"],
        "[reply message_id=quoted-1]\n上一条\n[/reply]\n"
    );
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "看图");
    assert_eq!(content[2]["type"], "image_url");
    assert_eq!(content[2]["image_url"]["url"], "https://example.test/a.jpg");
}

#[test]
fn chat_completions_payload_rejects_file_url_image_part() {
    let err = chat_completions_payload(
        &[ChatMessage::user_with_parts(
            "看图",
            vec![
                MessageInputPart::text("看图"),
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/jpeg".to_owned()),
                    filename: Some("a.jpg".to_owned()),
                    url: Some("file://C:\\Users\\ThinkPad\\Pictures\\a.jpg".to_owned()),
                    ..Default::default()
                }),
            ],
        )],
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        false,
    )
    .unwrap_err();

    assert_eq!(err.code, "unsupported_input_part");
    assert!(err.message.contains("当前入口没有提供可读取图片内容"));
    assert!(!err.message.contains("C:\\Users"));
}

#[test]
fn chat_completions_payload_uses_local_path_as_data_url() {
    let path = std::env::temp_dir().join(format!(
        "qq-maid-chat-local-image-{}.jpg",
        std::process::id()
    ));
    std::fs::write(&path, b"fake-jpg").unwrap();

    let payload = chat_completions_payload(
        &[ChatMessage::user_with_parts(
            "看图",
            vec![MessageInputPart::image(MessageMedia {
                mime_type: Some("image/jpeg".to_owned()),
                filename: Some("a.jpg".to_owned()),
                local_path: Some(path.to_string_lossy().to_string()),
                ..Default::default()
            })],
        )],
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        false,
    )
    .unwrap();
    let image_url = payload["messages"][0]["content"][0]["image_url"]["url"]
        .as_str()
        .unwrap();

    assert!(image_url.starts_with("data:image/jpeg;base64,"));
    assert!(!image_url.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn chat_completions_payload_rejects_oversized_local_image() {
    let path = std::env::temp_dir().join(format!(
        "qq-maid-chat-local-image-too-large-{}.png",
        std::process::id()
    ));
    std::fs::write(&path, b"12345678").unwrap();

    let err = chat_completions_payload(
        &[ChatMessage::user_with_parts(
            "看图",
            vec![MessageInputPart::image(MessageMedia {
                mime_type: Some("image/png".to_owned()),
                filename: Some("a.png".to_owned()),
                local_path: Some(path.to_string_lossy().to_string()),
                ..Default::default()
            })],
        )],
        "gpt-test",
        4,
        1200,
        false,
    )
    .unwrap_err();

    assert_eq!(err.code, "unsupported_input_part");
    assert!(err.message.contains("图片太大了"));
    assert!(!err.message.contains(path.to_string_lossy().as_ref()));
}

#[test]
fn chat_completions_payload_ignores_generic_mime_when_path_is_png() {
    let path = std::env::temp_dir().join(format!(
        "qq-maid-chat-local-generic-mime-{}.png",
        std::process::id()
    ));
    std::fs::write(&path, b"fake-png").unwrap();

    let payload = chat_completions_payload(
        &[ChatMessage::user_with_parts(
            "看图",
            vec![MessageInputPart::image(MessageMedia {
                mime_type: Some("image".to_owned()),
                filename: Some("upload".to_owned()),
                local_path: Some(path.to_string_lossy().to_string()),
                ..Default::default()
            })],
        )],
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        false,
    )
    .unwrap();

    assert_eq!(
        payload["messages"][0]["content"][0]["image_url"]["url"].as_str(),
        Some("data:image/png;base64,ZmFrZS1wbmc=")
    );
}

#[tokio::test]
async fn non_stream_chat_completion_extracts_text_and_usage() {
    let (base_url, state) = spawn_mock_chat(
        vec![
            json!({
                "choices": [{"message": {"content": "ok"}}],
                "usage": {
                    "prompt_tokens": 2,
                    "completion_tokens": 3,
                    "total_tokens": 5,
                    "prompt_tokens_details": {"cached_tokens": 0}
                }
            })
            .to_string(),
        ],
        StatusCode::OK,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = non_stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "ok");
    assert_eq!(outcome.usage.unwrap().cached_input_tokens, Some(0));
    assert_eq!(
        state.lock().await.requests[0]["messages"][0]["content"][0]["type"],
        "text"
    );
}

#[tokio::test]
async fn stream_chat_completion_extracts_delta() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "你好");
    assert_eq!(outcome.usage.unwrap().total_tokens, Some(3));
}

#[tokio::test]
async fn stream_chat_completion_skips_null_and_non_body_chunks() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"可以\"}}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned();
    let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "可以");
    assert!(!outcome.reply.starts_with("null"));
    assert_eq!(outcome.usage.unwrap().total_tokens, Some(3));
}

#[tokio::test]
async fn stream_chat_completion_requires_done_after_delta() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n".to_owned();
    let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let err = stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert!(err.message.contains("[DONE]"));
}

#[tokio::test]
async fn stream_chat_completion_accepts_finish_reason_without_done() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}]}\n\n",
    )
    .to_owned();
    let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "你好");
}

#[tokio::test]
async fn empty_stream_retries_non_stream() {
    let (base_url, state) = spawn_mock_chat(
        vec![
            "data: [DONE]\n\n".to_owned(),
            json!({"choices": [{"message": {"content": "retry ok"}}]}).to_string(),
        ],
        StatusCode::OK,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = chat_completions_with_stream_fallback(
        true,
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "retry ok");
    assert_eq!(state.lock().await.requests.len(), 2);
}

#[tokio::test]
async fn chat_with_stream_fallback_retries_non_stream_after_stream_parse_error() {
    let (base_url, state) = spawn_mock_chat(
        vec![
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n",
                "data: {not-json}\n\n",
            )
            .to_owned(),
            json!({"choices": [{"message": {"content": "non stream ok"}}]}).to_string(),
        ],
        StatusCode::OK,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let outcome = chat_completions_with_stream_fallback(
        true,
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "non stream ok");
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["stream"], true);
    assert!(requests[1].get("stream").is_none());
}

#[tokio::test]
async fn raw_stream_chat_does_not_retry_non_stream_after_delta_error() {
    let (base_url, state) = spawn_mock_chat(
        vec![
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n",
                "data: {not-json}\n\n",
            )
            .to_owned(),
        ],
        StatusCode::OK,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let err = stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "sse");
    assert_eq!(state.lock().await.requests.len(), 1);
}

#[tokio::test]
async fn prompt_blocked_error_keeps_safety_code() {
    let (base_url, _state) = spawn_mock_chat(
        vec![
            json!({
                "error": {
                    "message": "request blocked by moderation policy",
                    "type": "prompt_blocked"
                }
            })
            .to_string(),
        ],
        StatusCode::BAD_REQUEST,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let err = non_stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "safety_blocked");
    assert_eq!(err.stage, "http");
    assert!(err.message.contains("prompt_blocked"));
}

#[tokio::test]
async fn non_stream_empty_reply_is_error() {
    let (base_url, _state) = spawn_mock_chat(
        vec![json!({"choices": [{"message": {"content": ""}}]}).to_string()],
        StatusCode::OK,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let err = non_stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "provider_error");
}

#[tokio::test]
async fn status_codes_are_classified() {
    let (base_url, _state) = spawn_mock_chat(
        vec!["rate limited".to_owned()],
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );

    let err = non_stream_completion(
        &client,
        "openai",
        "gpt-test",
        10 * 1024 * 1024,
        1200,
        &[ChatMessage::user("hi")],
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "rate_limited");
    assert!(err.message.contains("HTTP 429"));
}

#[test]
fn custom_endpoint_is_used() {
    assert_eq!(
        chat_completions_url(Some("https://proxy.example/v1/")),
        "https://proxy.example/v1/chat/completions"
    );
}
