use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    config::{HttpAuthConfig, OpenAiResponsesProviderConfig},
    provider::types::ModelProvider,
    web_search::{
        DynWebSearchExecutor, WebSearchBackend, WebSearchExecutor, WebSearchRequest,
        routing::RoutedWebSearchExecutor,
    },
};

use super::{MissingWebSearchExecutor, ResponsesWebSearchExecutor};

#[derive(Debug)]
struct MockSearchState {
    status: StatusCode,
    body: String,
    requests: Vec<Value>,
    request_headers: Vec<HeaderMap>,
    request_paths: Vec<String>,
}

async fn mock_search_handler(
    State(state): State<Arc<Mutex<MockSearchState>>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    let mut state = state.lock().await;
    state.requests.push(request);
    state.request_headers.push(headers);
    state.request_paths.push(uri.path().to_owned());
    let status = state.status;
    let body = state.body.clone();
    (status, [(header::CONTENT_TYPE, "application/json")], body)
}

async fn spawn_mock_search(body: String) -> (String, Arc<Mutex<MockSearchState>>) {
    spawn_mock_search_with_status(StatusCode::OK, body).await
}

async fn spawn_mock_search_with_status(
    status: StatusCode,
    body: String,
) -> (String, Arc<Mutex<MockSearchState>>) {
    let state = Arc::new(Mutex::new(MockSearchState {
        status,
        body,
        requests: Vec::new(),
        request_headers: Vec::new(),
        request_paths: Vec::new(),
    }));
    let app = Router::new()
        .route("/v1/responses", post(mock_search_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state)
}

fn provider_config(id: &str, base_url: String) -> OpenAiResponsesProviderConfig {
    OpenAiResponsesProviderConfig {
        id: ModelProvider::Custom(id.to_owned()),
        base_url,
        api_key_env: format!("{}_API_KEY", id.to_ascii_uppercase()),
        api_key: Some(format!("{id}-secret")),
        auth: HttpAuthConfig::default(),
        request_timeout_seconds: Some(5),
        chat_fallback: false,
    }
}

fn request(model: &str) -> WebSearchRequest {
    WebSearchRequest {
        query: "Responses search test".to_owned(),
        raw_question: None,
        max_results: Some(5),
        context_size: None,
        topic: None,
        time_range: None,
        backend_override: None,
        model_override: Some(model.to_owned()),
    }
}

#[tokio::test]
async fn configured_responses_search_uses_own_transport_raw_model_and_common_parser() {
    let body = json!({
        "output_text": "XAI 搜索正文",
        "output": [{
            "type": "web_search_call",
            "action": {"sources": [{
                "title": "XAI 文档",
                "url": "https://docs.x.ai/search",
                "snippet": "source"
            }]}
        }]
    })
    .to_string();
    let (base_url, state) = spawn_mock_search(body).await;
    let mut config = provider_config("xai", base_url);
    config.auth = HttpAuthConfig {
        header: "x-api-key".to_owned(),
        scheme: None,
    };
    let executor =
        ResponsesWebSearchExecutor::new_configured(&config, "grok-default".to_owned(), 10).unwrap();

    let outcome = executor.query(request("grok-4")).await.unwrap();

    assert_eq!(outcome.answer, "XAI 搜索正文");
    assert_eq!(outcome.provider, "xai");
    assert_eq!(outcome.sources.len(), 1);
    assert_eq!(outcome.sources[0].url, "https://docs.x.ai/search");
    let state = state.lock().await;
    assert_eq!(state.request_paths, ["/v1/responses"]);
    assert_eq!(state.requests[0]["model"], "grok-4");
    assert_eq!(state.requests[0]["tools"], json!([{"type": "web_search"}]));
    assert_eq!(
        state.request_headers[0]
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("xai-secret")
    );
}

#[tokio::test]
async fn same_model_name_routes_to_each_configured_responses_base_url() {
    let response = json!({"output_text": "ok", "output": []}).to_string();
    let (base_a, state_a) = spawn_mock_search(response.clone()).await;
    let (base_b, state_b) = spawn_mock_search(response).await;
    let native_providers = HashMap::from([
        (
            ModelProvider::Custom("routera".to_owned()),
            Arc::new(
                ResponsesWebSearchExecutor::new_configured(
                    &provider_config("routera", base_a),
                    "same-model".to_owned(),
                    10,
                )
                .unwrap(),
            ) as DynWebSearchExecutor,
        ),
        (
            ModelProvider::Custom("routerb".to_owned()),
            Arc::new(
                ResponsesWebSearchExecutor::new_configured(
                    &provider_config("routerb", base_b),
                    "same-model".to_owned(),
                    10,
                )
                .unwrap(),
            ) as DynWebSearchExecutor,
        ),
    ]);
    let router = RoutedWebSearchExecutor::new(
        WebSearchBackend::ProviderNative,
        "routera:same-model".to_owned(),
        5,
        native_providers,
        Arc::new(MissingWebSearchExecutor),
        Arc::new(MissingWebSearchExecutor),
    );

    let first = router.query(request("routera:same-model")).await.unwrap();
    let second = router.query(request("routerb:same-model")).await.unwrap();

    assert_eq!(first.provider, "routera");
    assert_eq!(second.provider, "routerb");
    assert_eq!(state_a.lock().await.requests[0]["model"], "same-model");
    assert_eq!(state_b.lock().await.requests[0]["model"], "same-model");
    assert_eq!(state_a.lock().await.requests.len(), 1);
    assert_eq!(state_b.lock().await.requests.len(), 1);
}

#[tokio::test]
async fn configured_responses_search_preserves_upstream_error() {
    let (base_url, _state) = spawn_mock_search_with_status(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error": {"message": "xai quota exhausted"}}).to_string(),
    )
    .await;
    let config = provider_config("xai", base_url);
    let executor =
        ResponsesWebSearchExecutor::new_configured(&config, "grok-4".to_owned(), 10).unwrap();

    let error = executor.query(request("grok-4")).await.unwrap_err();

    assert_eq!(error.code, "rate_limited");
    assert!(error.message.contains("xai quota exhausted"));
    assert!(error.message.contains("xai Responses returned HTTP 429"));
}
