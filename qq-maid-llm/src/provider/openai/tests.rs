use super::*;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode as AxumStatusCode, header},
    response::IntoResponse,
    routing::post,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};

#[derive(Debug)]
struct MockOpenAiState {
    responses_status: AxumStatusCode,
    responses_body: Value,
    chat_body: Value,
    responses_calls: usize,
    chat_calls: usize,
    chat_requests: Vec<Value>,
}

async fn mock_responses_handler(
    State(state): State<Arc<Mutex<MockOpenAiState>>>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    state.responses_calls += 1;
    (state.responses_status, Json(state.responses_body.clone()))
}

async fn mock_chat_completions_handler(
    State(state): State<Arc<Mutex<MockOpenAiState>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    state.chat_calls += 1;
    state.chat_requests.push(body);
    Json(state.chat_body.clone())
}

async fn spawn_mock_openai(state: MockOpenAiState) -> (String, Arc<Mutex<MockOpenAiState>>) {
    let state = Arc::new(Mutex::new(state));
    let app = Router::new()
        .route("/v1/responses", post(mock_responses_handler))
        .route("/v1/chat/completions", post(mock_chat_completions_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state)
}

#[derive(Debug)]
struct MockOpenAiStreamState {
    responses_body: String,
    chat_body: String,
    responses_calls: usize,
    chat_calls: usize,
}

async fn mock_stream_responses_handler(
    State(state): State<Arc<Mutex<MockOpenAiStreamState>>>,
    _body: Body,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    state.responses_calls += 1;
    (
        AxumStatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        state.responses_body.clone(),
    )
}

async fn mock_stream_chat_handler(
    State(state): State<Arc<Mutex<MockOpenAiStreamState>>>,
    _body: Body,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    state.chat_calls += 1;
    (
        AxumStatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        state.chat_body.clone(),
    )
}

async fn spawn_mock_openai_stream(
    responses_body: String,
    chat_body: String,
) -> (String, Arc<Mutex<MockOpenAiStreamState>>) {
    let state = Arc::new(Mutex::new(MockOpenAiStreamState {
        responses_body,
        chat_body,
        responses_calls: 0,
        chat_calls: 0,
    }));
    let app = Router::new()
        .route("/v1/responses", post(mock_stream_responses_handler))
        .route("/v1/chat/completions", post(mock_stream_chat_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state)
}

fn mock_chat_response(text: &str) -> Value {
    serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": text
            }
        }],
        "usage": {
            "prompt_tokens": 2,
            "completion_tokens": 3,
            "total_tokens": 5
        }
    })
}

fn provider_with_api_mode(api_mode: OpenAiApiMode) -> OpenAiProvider {
    let http_client = qq_maid_common::http_client::client();
    OpenAiProvider {
        responses_client: http_client.clone(),
        chat_client: ChatCompletionsClient::new("test-key".to_owned(), None, http_client),
        api_key: "test-key".to_owned(),
        base_url: None,
        model: "gpt-5.5".to_owned(),
        api_mode,
        stream: false,
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
    }
}

#[test]
fn openai_tool_calling_protocol_requires_responses_auto_mode() {
    assert_eq!(
        provider_with_api_mode(OpenAiApiMode::Auto).tool_calling_protocol(None),
        Some(ToolCallingProtocol::OpenAiResponses)
    );
    assert_eq!(
        provider_with_api_mode(OpenAiApiMode::ChatOnly).tool_calling_protocol(None),
        None
    );
}

#[tokio::test]
async fn openai_chat_uses_responses_without_chat_fallback_when_responses_succeeds() {
    let (base_url, state) = spawn_mock_openai(MockOpenAiState {
        responses_status: AxumStatusCode::OK,
        responses_body: serde_json::json!({"output_text": "responses ok"}),
        chat_body: mock_chat_response("chat fallback"),
        responses_calls: 0,
        chat_calls: 0,
        chat_requests: Vec::new(),
    })
    .await;
    let http_client = qq_maid_common::http_client::client();
    let chat_client = ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

    let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
        api_mode: OpenAiApiMode::Auto,
        stream: false,
        responses_client: &http_client,
        chat_client: &chat_client,
        api_key: "test-key",
        base_url: Some(&base_url),
        responses_auth: None,
        provider: "openai",
        model: "gpt-5.5",
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
        reasoning_effort: None,
        messages: &[ChatMessage::user("hi")],
        image_generation_enabled: false,
    })
    .await
    .unwrap();

    assert_eq!(outcome.reply, "responses ok");
    let state = state.lock().await;
    assert_eq!(state.responses_calls, 1);
    assert_eq!(state.chat_calls, 0);
}

#[tokio::test]
async fn openai_chat_falls_back_to_chat_completions_after_responses_http_error() {
    let (base_url, state) = spawn_mock_openai(MockOpenAiState {
        responses_status: AxumStatusCode::BAD_REQUEST,
        responses_body: serde_json::json!({"error": {"message": "invalid responses schema"}}),
        chat_body: mock_chat_response("chat fallback ok"),
        responses_calls: 0,
        chat_calls: 0,
        chat_requests: Vec::new(),
    })
    .await;
    let http_client = qq_maid_common::http_client::client();
    let chat_client = ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());
    let messages = [
        ChatMessage::system("system"),
        ChatMessage {
            role: crate::provider::types::ChatRole::Assistant,
            content: "old reply".to_owned(),
            content_parts: Vec::new(),
        },
        ChatMessage::user("again"),
    ];

    let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
        api_mode: OpenAiApiMode::Auto,
        stream: false,
        responses_client: &http_client,
        chat_client: &chat_client,
        api_key: "test-key",
        base_url: Some(&base_url),
        responses_auth: None,
        provider: "openai",
        model: "gpt-5.5",
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
        reasoning_effort: None,
        messages: &messages,
        image_generation_enabled: false,
    })
    .await
    .unwrap();

    assert_eq!(outcome.reply, "chat fallback ok");
    let state = state.lock().await;
    assert_eq!(state.responses_calls, 1);
    assert_eq!(state.chat_calls, 1);
    let request = state.chat_requests.first().unwrap();
    assert_eq!(request["model"], "gpt-5.5");
    assert_eq!(request["messages"][1]["role"], "assistant");
    assert_eq!(request["messages"][1]["content"][0]["type"], "text");
    assert_eq!(request["messages"][1]["content"][0]["text"], "old reply");
    assert!(request.get("input").is_none());
}

#[tokio::test]
async fn openai_chat_falls_back_after_responses_incompatible_status() {
    for responses_status in [
        AxumStatusCode::NOT_FOUND,
        AxumStatusCode::UNPROCESSABLE_ENTITY,
    ] {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
                responses_status,
                responses_body: serde_json::json!({"error": {"message": "Responses API is incompatible"}}),
                chat_body: mock_chat_response("chat fallback ok"),
                responses_calls: 0,
                chat_calls: 0,
                chat_requests: Vec::new(),
            })
            .await;
        let http_client = qq_maid_common::http_client::client();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: false,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            responses_auth: None,
            provider: "openai",
            model: "gpt-5.5",
            media_max_bytes: 10 * 1024 * 1024,
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages: &[ChatMessage::user("hi")],
            image_generation_enabled: false,
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "chat fallback ok");
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1, "status={responses_status}");
        assert_eq!(state.chat_calls, 1, "status={responses_status}");
    }
}

#[tokio::test]
async fn openai_chat_does_not_fallback_after_responses_authentication_status() {
    for responses_status in [AxumStatusCode::UNAUTHORIZED, AxumStatusCode::FORBIDDEN] {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
            responses_status,
            responses_body: serde_json::json!({"error": {"message": "authentication rejected"}}),
            chat_body: mock_chat_response("must not run"),
            responses_calls: 0,
            chat_calls: 0,
            chat_requests: Vec::new(),
        })
        .await;
        let http_client = qq_maid_common::http_client::client();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let error = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: false,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            responses_auth: None,
            provider: "openai",
            model: "gpt-5.5",
            media_max_bytes: 10 * 1024 * 1024,
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages: &[ChatMessage::user("hi")],
            image_generation_enabled: false,
        })
        .await
        .unwrap_err();

        assert_eq!(error.code, "authentication_failed");
        assert_eq!(error.upstream_status, Some(responses_status.as_u16()));
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1, "status={responses_status}");
        assert_eq!(state.chat_calls, 0, "status={responses_status}");
    }
}

#[tokio::test]
async fn openai_chat_only_uses_chat_completions_without_responses() {
    let (base_url, state) = spawn_mock_openai(MockOpenAiState {
        responses_status: AxumStatusCode::INTERNAL_SERVER_ERROR,
        responses_body: serde_json::json!({"error": {"message": "responses should not be called"}}),
        chat_body: mock_chat_response("chat only ok"),
        responses_calls: 0,
        chat_calls: 0,
        chat_requests: Vec::new(),
    })
    .await;
    let http_client = qq_maid_common::http_client::client();
    let chat_client = ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

    let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
        api_mode: OpenAiApiMode::ChatOnly,
        stream: false,
        responses_client: &http_client,
        chat_client: &chat_client,
        api_key: "test-key",
        base_url: Some(&base_url),
        responses_auth: None,
        provider: "openai",
        model: "gpt-5.5",
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
        reasoning_effort: None,
        messages: &[ChatMessage::user("hi")],
        image_generation_enabled: false,
    })
    .await
    .unwrap();

    assert_eq!(outcome.reply, "chat only ok");
    let state = state.lock().await;
    assert_eq!(state.responses_calls, 0);
    assert_eq!(state.chat_calls, 1);
}

#[tokio::test]
async fn openai_responses_stream_falls_back_before_first_delta() {
    let (base_url, state) = spawn_mock_openai_stream(
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"responses unavailable\"}}}\n\n"
                .to_owned(),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"chat fallback\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        )
        .await;
    let http_client = qq_maid_common::http_client::client();
    let chat_client = ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

    let stream = openai_auto_stream_with_chat_fallback(OpenAiChatFallbackRequest {
        api_mode: OpenAiApiMode::Auto,
        stream: true,
        responses_client: &http_client,
        chat_client: &chat_client,
        api_key: "test-key",
        base_url: Some(&base_url),
        responses_auth: None,
        provider: "openai",
        model: "gpt-5.5",
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
        reasoning_effort: None,
        messages: &[ChatMessage::user("hi")],
        image_generation_enabled: false,
    })
    .await
    .unwrap();
    let outcome = crate::provider::collect_llm_stream(stream, "openai", "gpt-5.5")
        .await
        .unwrap();

    assert_eq!(outcome.reply, "chat fallback");
    assert!(outcome.fallback_used);
    let state = state.lock().await;
    assert_eq!(state.responses_calls, 1);
    assert_eq!(state.chat_calls, 1);
}

#[tokio::test]
async fn openai_responses_stream_does_not_fallback_after_delta() {
    let (base_url, state) = spawn_mock_openai_stream(
            concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
                "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"broken\"}}}\n\n",
            )
            .to_owned(),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"must not append\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        )
        .await;
    let http_client = qq_maid_common::http_client::client();
    let chat_client = ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

    let stream = openai_auto_stream_with_chat_fallback(OpenAiChatFallbackRequest {
        api_mode: OpenAiApiMode::Auto,
        stream: true,
        responses_client: &http_client,
        chat_client: &chat_client,
        api_key: "test-key",
        base_url: Some(&base_url),
        responses_auth: None,
        provider: "openai",
        model: "gpt-5.5",
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
        reasoning_effort: None,
        messages: &[ChatMessage::user("hi")],
        image_generation_enabled: false,
    })
    .await
    .unwrap();
    let err = crate::provider::collect_llm_stream(stream, "openai", "gpt-5.5")
        .await
        .unwrap_err();

    assert_eq!(err.stage, "sse");
    let state = state.lock().await;
    assert_eq!(state.responses_calls, 1);
    assert_eq!(state.chat_calls, 0);
}

#[test]
fn effective_openai_model_strips_openai_prefix() {
    assert_eq!(
        effective_openai_model(Some("openai:gpt-5-mini"), "default").unwrap(),
        "gpt-5-mini"
    );
    assert_eq!(
        effective_openai_model(Some("gpt-5-mini"), "default").unwrap(),
        "gpt-5-mini"
    );
    assert_eq!(effective_openai_model(None, "default").unwrap(), "default");
}

#[test]
fn effective_openai_model_rejects_deepseek_prefix() {
    let err = effective_openai_model(Some("deepseek:deepseek-chat"), "default").unwrap_err();
    assert_eq!(err.code, "bad_request");
}
