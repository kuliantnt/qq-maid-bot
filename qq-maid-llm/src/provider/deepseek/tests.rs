use std::{collections::VecDeque, sync::Arc};

use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use futures::StreamExt;
use qq_maid_common::input_part::{MessageInputPart, MessageMedia};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::*;
use crate::{
    config::{OpenAiApiMode, ProviderMode},
    provider::{
        LlmStreamEvent, ToolChatRequest, collect_llm_stream,
        test_support::{WeatherToolStub, test_tool_context},
        types::{ChatMessage, ModelRoute, ReasoningEffort},
    },
    tool::ToolRegistry,
    web_search::{WebSearchBackend, WebSearchConfig, WebSearchRequest, build_web_search_executor},
};

#[derive(Default)]
struct MockState {
    replies: VecDeque<(StatusCode, String)>,
    requests: Vec<(String, HeaderMap, Value)>,
}

async fn mock(replies: Vec<(StatusCode, String)>) -> (String, Arc<Mutex<MockState>>) {
    async fn handler(
        State(state): State<Arc<Mutex<MockState>>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, String) {
        let mut state = state.lock().await;
        state.requests.push((uri.path().to_owned(), headers, body));
        state.replies.pop_front().unwrap_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected retry".to_owned(),
        ))
    }
    let state = Arc::new(Mutex::new(MockState {
        replies: replies.into(),
        ..Default::default()
    }));
    let app = Router::new()
        .route("/responses", post(handler))
        .route("/chat/completions", post(handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), state)
}

fn config(base_url: String, stream: bool) -> LlmConfig {
    let route = ModelRoute::parse_config("deepseek:model-under-test", "test").unwrap();
    LlmConfig {
        provider: ProviderMode::DeepSeek,
        model_route: route.clone(),
        configured_model_routes: vec![("test".to_owned(), route)],
        web_search: WebSearchConfig {
            default_backend: WebSearchBackend::ProviderNative,
            default_model: "deepseek:model-under-test".to_owned(),
            ..Default::default()
        },
        tavily_api_key: None,
        openai_api_key: None,
        openai_base_url: None,
        openai_api_mode: OpenAiApiMode::ChatOnly,
        deepseek_api_key: Some("test-deepseek-key".to_owned()),
        deepseek_base_url: base_url,
        deepseek_model: "deepseek:model-under-test".to_owned(),
        bigmodel_api_key: None,
        bigmodel_base_url: "https://example.test".to_owned(),
        bigmodel_model: "unused".to_owned(),
        gemini_api_key: None,
        gemini_base_url: "https://example.test".to_owned(),
        gemini_model: "unused".to_owned(),
        openai_compatible_providers: Vec::new(),
        openai_responses_providers: Vec::new(),
        stream,
        request_timeout_seconds: 5,
        media_max_bytes: 1024,
        max_output_tokens: 1200,
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        session_id: "test-session".to_owned(),
        model: Some("deepseek:arbitrary-model".to_owned()),
        messages: vec![ChatMessage::user("测试")],
        context_budget: None,
        max_output_tokens: Some(64),
        reasoning_effort: Some(ReasoningEffort::High),
        metadata: Default::default(),
    }
}

fn answer() -> Value {
    json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "回复"}]}],
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4},
            "output_tokens": 6, "output_tokens_details": {"reasoning_tokens": 2}, "total_tokens": 16}})
}

fn sse(body: Value) -> String {
    format!(
        "event: response.reasoning_text.delta\ndata: {{\"type\":\"response.reasoning_text.delta\",\"delta\":\"reasoning\"}}\n\nevent: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"回复\"}}\n\nevent: response.completed\ndata: {}\n\n",
        json!({"type": "response.completed", "response": body})
    )
}

#[test]
fn model_identity_and_protocol_are_responses_without_name_heuristics() {
    let provider = DeepSeekProvider::new(&config("https://example.test".to_owned(), true)).unwrap();
    assert_eq!(provider.model(), "model-under-test");
    assert_eq!(
        provider.tool_calling_protocol(Some("deepseek:arbitrary-model")),
        Some(ToolCallingProtocol::OpenAiResponses)
    );
    assert!(provider.supports_vision(Some("deepseek:arbitrary-model")));
    assert_eq!(
        provider.tool_calling_protocol(Some("openai:gpt-test")),
        None
    );
    assert!(!provider.supports_vision(Some("openai:gpt-test")));
    assert!(deepseek_config_model("openai:wrong").is_err());
}

#[tokio::test]
async fn chat_and_stream_use_responses_images_reasoning_and_usage() {
    for stream in [false, true] {
        let body = if stream {
            sse(answer())
        } else {
            answer().to_string()
        };
        let (url, state) = mock(vec![(StatusCode::OK, body)]).await;
        let provider = DeepSeekProvider::new(&config(url, stream)).unwrap();
        let mut req = request();
        req.messages = vec![ChatMessage::user_with_parts(
            "看图",
            vec![
                MessageInputPart::text("看图"),
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    url: Some("https://example.test/image.png".to_owned()),
                    ..Default::default()
                }),
            ],
        )];
        let outcome = if stream {
            collect_llm_stream(
                provider.stream_chat(req).await.unwrap(),
                "deepseek",
                "arbitrary-model",
            )
            .await
            .unwrap()
        } else {
            provider.chat(req).await.unwrap()
        };
        assert_eq!(outcome.reply, "回复");
        assert_eq!(outcome.metrics.provider, "deepseek");
        assert_eq!(outcome.usage.as_ref().unwrap().cached_input_tokens, Some(4));
        assert_eq!(outcome.usage.as_ref().unwrap().total_tokens, Some(16));
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 1);
        let (path, headers, body) = &state.requests[0];
        assert_eq!(path, "/responses");
        assert_eq!(headers["authorization"], "Bearer test-deepseek-key");
        assert_eq!(body["model"], "arbitrary-model");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["max_output_tokens"], 64);
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(
            body["input"][0]["content"][1]["image_url"],
            "https://example.test/image.png"
        );
        assert!(body.get("tools").is_none());
    }
}

#[tokio::test]
async fn empty_stream_retries_responses_non_stream() {
    let (url, state) = mock(vec![
        (StatusCode::OK, String::new()),
        (StatusCode::OK, answer().to_string()),
    ])
    .await;
    let provider = DeepSeekProvider::new(&config(url, true)).unwrap();
    assert_eq!(provider.chat(request()).await.unwrap().reply, "回复");
    let state = state.lock().await;
    assert_eq!(state.requests.len(), 2);
    assert!(
        state
            .requests
            .iter()
            .all(|(path, _, _)| path == "/responses")
    );
    assert_eq!(state.requests[0].2["stream"], true);
    assert!(state.requests[1].2.get("stream").is_none());
}

#[tokio::test]
async fn stream_error_after_delta_does_not_retry() {
    let (url, state) = mock(vec![(StatusCode::OK,
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"半截\"}\n\ndata: {not-json}\n\n".to_owned())]).await;
    let provider = DeepSeekProvider::new(&config(url, true)).unwrap();
    let mut stream = provider.stream_chat(request()).await.unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Ok(LlmStreamEvent::TextDelta(_)))
    ));
    assert!(stream.next().await.unwrap().is_err());
    assert_eq!(state.lock().await.requests.len(), 1);
}

#[tokio::test]
async fn http_failure_preserves_error_without_chat_fallback() {
    let (url, state) = mock(vec![(
        StatusCode::NOT_FOUND,
        "Responses unavailable".to_owned(),
    )])
    .await;
    let provider = DeepSeekProvider::new(&config(url, false)).unwrap();
    let error = provider.chat(request()).await.unwrap_err();
    assert!(error.message.contains("Responses unavailable"));
    assert!(error.message.contains("deepseek"));
    assert_eq!(state.lock().await.requests.len(), 1);
}

#[tokio::test]
async fn tool_loop_replays_reasoning_function_call_and_real_tool_output() {
    for stream in [false, true] {
        let reasoning = json!({"type": "reasoning", "content": [{"type": "reasoning_text", "text": "need tool"}]});
        let replies = vec![(StatusCode::OK, json!({"output": [reasoning.clone(),
        {"type": "function_call", "call_id": "call_test", "name": "get_weather", "arguments": "{\"city\":\"杭州\"}"}]}).to_string()),
        (StatusCode::OK, answer().to_string())];
        let replies = replies
            .into_iter()
            .map(|(status, body)| {
                (
                    status,
                    if stream {
                        sse(serde_json::from_str(&body).unwrap())
                    } else {
                        body
                    },
                )
            })
            .collect();
        let (url, state) = mock(replies).await;
        let provider = DeepSeekProvider::new(&config(url, stream)).unwrap();
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let outcome = provider
            .chat_with_tools(ToolChatRequest {
                chat: request(),
                tools: ToolRegistry::new()
                    .register(WeatherToolStub::new("晴"))
                    .unwrap(),
                tool_context: test_tool_context(),
                max_rounds: 3,
                progress_sink: None,
                final_delta_sink: stream.then(|| {
                    let deltas = deltas.clone();
                    Arc::new(
                        move |delta: String| -> crate::agent_loop::AgentTextDeltaFuture {
                            let deltas = deltas.clone();
                            Box::pin(async move {
                                deltas.lock().await.push(delta);
                                Ok(crate::agent_loop::AgentTextDeltaDelivery::Visible)
                            })
                        },
                    ) as crate::agent_loop::AgentTextDeltaSink
                }),
                run_handle: None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.reply, "回复");
        assert_eq!(outcome.agent.executed_tools, vec!["get_weather"]);
        if stream {
            assert_eq!(*deltas.lock().await, vec!["回复"]);
        }
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        for (path, _, body) in &state.requests {
            assert_eq!(path, "/responses");
            assert_eq!(
                body.get("stream").and_then(Value::as_bool).unwrap_or(false),
                stream
            );
            assert_eq!(body["reasoning"]["effort"], "high");
        }
        assert_eq!(state.requests[0].2["tools"][0]["type"], "function");
        let input = state.requests[1].2["input"].as_array().unwrap();
        assert!(input.contains(&reasoning));
        let output = input
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["call_id"], "call_test");
        assert!(output["output"].as_str().unwrap().contains("晴"));
    }
}

fn search_request() -> WebSearchRequest {
    WebSearchRequest {
        query: "测试搜索".to_owned(),
        raw_question: None,
        max_results: None,
        context_size: None,
        topic: None,
        time_range: None,
        backend_override: None,
        model_override: None,
    }
}

#[tokio::test]
async fn native_search_routes_deepseek_without_openai_credentials() {
    for stream in [false, true] {
        let body = json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "回复",
            "annotations": [{"type": "url_citation", "title": "测试来源", "url": "https://example.test/source"}]}]}]});
        let (url, state) = mock(vec![(
            StatusCode::OK,
            if stream { sse(body) } else { body.to_string() },
        )])
        .await;
        let executor = build_web_search_executor(&config(url, false)).unwrap();
        let outcome = if stream {
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let outcome = executor.query_stream(search_request(), tx).await.unwrap();
            assert_eq!(rx.recv().await.as_deref(), Some("回复"));
            outcome
        } else {
            executor.query(search_request()).await.unwrap()
        };
        assert_eq!(outcome.provider, "deepseek");
        assert_eq!(outcome.answer, "回复");
        assert_eq!(outcome.sources[0].url, "https://example.test/source");
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 1);
        let (path, headers, body) = &state.requests[0];
        assert_eq!(path, "/responses");
        assert_eq!(headers["authorization"], "Bearer test-deepseek-key");
        assert_eq!(body["model"], "model-under-test");
        assert_eq!(body["tools"], json!([{"type": "web_search"}]));
    }
}

#[tokio::test]
async fn missing_search_key_reports_deepseek_config_error() {
    let mut config = config("https://example.test".to_owned(), false);
    config.deepseek_api_key = None;
    let error = build_web_search_executor(&config)
        .unwrap()
        .query(search_request())
        .await
        .unwrap_err();
    assert!(error.message.contains("DEEPSEEK_API_KEY"));
}
