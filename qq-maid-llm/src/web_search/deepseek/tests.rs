use super::*;
use crate::config::{OpenAiApiMode, ProviderMode};
use crate::provider::types::ModelRoute;
use crate::web_search::{WebSearchBackend, build_web_search_executor};
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

fn request() -> WebSearchRequest {
    WebSearchRequest {
        query: "Rust site:rust-lang.org".to_owned(),
        raw_question: Some("不应发送的原始上下文".to_owned()),
        max_results: Some(10),
        context_size: None,
        topic: None,
        time_range: None,
        backend_override: Some(WebSearchBackend::DeepSeek),
        model_override: None,
    }
}

async fn server(status: u16, body: Value) -> (LlmConfig, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let app = Router::new().route(
        "/v1/tools/search",
        post(move |headers: HeaderMap, Json(payload): Json<Value>| {
            let captured = captured.clone();
            let body = body.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer test-key");
                captured.lock().await.push(payload);
                (StatusCode::from_u16(status).unwrap(), Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = LlmConfig {
        deepseek_api_key: Some("test-key".to_owned()),
        deepseek_base_url: base.clone(),
        ..config(base.clone(), false)
    };
    (config, requests)
}

#[tokio::test]
async fn routed_search_uses_independent_api_and_summary_without_conversation() {
    let (mut config, captured) = server(200, json!({"results":[
        {"title":"Rust", "url":"https://rust-lang.org", "snippet":"短摘要", "summary":"模型读取的长摘要"},
        {"title":"unsafe", "url":"javascript:alert(1)", "snippet":"", "summary":""}
    ]})).await;
    config.deepseek_base_url.push_str("/v1/");
    config.web_search.max_results = 3;
    let executor = build_web_search_executor(&config).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let outcome = executor.query_stream(request(), tx).await.unwrap();
    assert_eq!(outcome.provider, "deepseek");
    assert_eq!(outcome.sources.len(), 1);
    assert_eq!(outcome.sources[0].snippet, "模型读取的长摘要");
    // answer 必须带真实检索摘要，而不只是参考来源链接。
    assert!(outcome.answer.contains("模型读取的长摘要"));
    assert!(outcome.answer.contains("https://rust-lang.org"));
    // 非流式上游仍通过公共默认实现输出最终文本。
    assert!(rx.recv().await.is_some());
    assert_eq!(
        captured.lock().await[0],
        json!({"query":"Rust site:rust-lang.org", "count":3, "need_content":false})
    );
}

#[tokio::test]
async fn answer_keeps_each_retrieval_summary_within_display_budget() {
    let summaries = (0..5)
        .map(|index| format!("第 {index} 条检索摘要|{}", "内容".repeat(300)))
        .collect::<Vec<_>>();
    let body = json!({
        "results": summaries.iter().enumerate().map(|(index, summary)| json!({
            "title": format!("来源 {index}"),
            "url": format!("https://example.test/{index}"),
            "snippet": "短摘要",
            "summary": summary,
        })).collect::<Vec<_>>()
    });
    let (config, _) = server(200, body).await;
    let outcome = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap();

    assert_eq!(outcome.sources.len(), 5);
    for (index, summary) in summaries.iter().enumerate() {
        let head = summary.chars().take(20).collect::<String>();
        assert!(outcome.answer.contains(&head), "missing summary {index}");
        // 参考来源链接仍完整保留在 answer 和结构化 sources 中。
        assert!(
            outcome
                .answer
                .contains(&format!("https://example.test/{index}"))
        );
        // 结构化 sources 保留完整摘要，展示层的截断只作用于 answer。
        assert_eq!(&outcome.sources[index].snippet, summary);
    }
    // 摘要正文受总预算约束，`/查` 的 1500 字符展示上限仍能容纳参考来源。
    assert!(outcome.answer.chars().count() < 1_500);
}

#[tokio::test]
async fn errors_and_empty_results_are_distinct() {
    for (status, code) in [
        (401, "authentication_failed"),
        (402, "quota_exhausted"),
        (429, "rate_limited"),
        (503, "upstream_unavailable"),
    ] {
        let (config, _) = server(status, json!({"error":{"message":"upstream failure"}})).await;
        let err = DeepSeekSearchExecutor::new(&config)
            .unwrap()
            .query(request())
            .await
            .unwrap_err();
        assert_eq!(err.code, code);
        assert!(err.message.contains("upstream failure"));
    }
    let (config, _) = server(200, json!({"results":[]})).await;
    let out = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap();
    assert!(out.sources.is_empty() && out.answer.is_empty());
    let (config, _) = server(200, json!({})).await;
    assert!(
        DeepSeekSearchExecutor::new(&config)
            .unwrap()
            .query(request())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validates_unicode_length_and_missing_key_before_network() {
    let (config, captured) = server(200, json!({"results":[]})).await;
    let executor = DeepSeekSearchExecutor::new(&config).unwrap();
    for query in [" ".to_owned(), "搜".repeat(129)] {
        let err = executor
            .query(WebSearchRequest { query, ..request() })
            .await
            .unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
    assert!(captured.lock().await.is_empty());
    executor
        .query(WebSearchRequest {
            query: "搜".repeat(128),
            ..request()
        })
        .await
        .unwrap();
    let config = LlmConfig {
        deepseek_api_key: None,
        ..config
    };
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert_eq!(err.code, "web_search_not_configured");
    // Core 依靠 stage 前缀区分 DeepSeek 和 Tavily 的缺 Key 文案。
    assert_eq!(err.stage, "deepseek_search_config");
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

#[tokio::test]
async fn first_response_timeout_is_bounded() {
    let app = Router::new().route(
        "/v1/tools/search",
        post(|| async {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Json(json!({"results": []}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = config(base, false);
    config.web_search.connect_timeout_seconds = 1;
    config.web_search.first_response_timeout_seconds = 1;
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "deepseek_search_first_response");
}

#[tokio::test]
async fn upstream_errors_redact_credentials() {
    let (config, _) = server(401, json!({"error":{"message":"invalid key test-key"}})).await;
    let err = DeepSeekSearchExecutor::new(&config)
        .unwrap()
        .query(request())
        .await
        .unwrap_err();
    assert!(!err.message.contains("test-key"));
    assert!(err.message.contains("<redacted>"));
}
