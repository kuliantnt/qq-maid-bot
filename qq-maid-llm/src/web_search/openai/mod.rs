use std::time::Instant;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    config::{HttpAuthConfig, LlmConfig, OpenAiResponsesProviderConfig},
    error::{LlmError, LlmErrorKind},
    metrics::duration_ms,
    provider::openai::{
        ResponsesTransportContext, is_openai_responses_done_sentinel, openai_responses_url,
        send_openai_responses_request,
    },
    sse::{SseFrame, parse_sse_frame, take_sse_frame},
};
use qq_maid_common::time_context::request_time_context;

use super::{
    DEFAULT_SEARCH_CONTEXT_SIZE, WebSearchExecutor, WebSearchOutcome, WebSearchRequest,
    WebSearchSource, build_query_prompt, configured_max_results, trace_query_input_enabled,
};

pub(super) struct MissingWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for MissingWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::config(
            "OPENAI_API_KEY is required for Rust web query service",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

pub(super) struct ChatOnlyWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for ChatOnlyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::config(
            "OPENAI_API_MODE=chat_only only supports chat completions; /查 requires an OpenAI Responses web_search compatible endpoint",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

/// 基于 Responses API 的公共 Web Search 执行器。
///
/// 内置 OpenAI 与配置驱动的 `openai_responses` Provider 只提供不同的连接和认证
/// 元数据；payload、HTTP transport、SSE、正文与来源解析始终走同一份实现。
pub(super) struct ResponsesWebSearchExecutor {
    client: reqwest::Client,
    api_key: String,
    base_url: Option<String>,
    auth: Option<HttpAuthConfig>,
    provider: String,
    search_model: String,
    search_context_size_supported: bool,
}

impl ResponsesWebSearchExecutor {
    pub(super) fn new_openai(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .openai_api_key
            .clone()
            .ok_or_else(|| LlmError::config("OPENAI_API_KEY is required"))?;
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| {
                LlmError::config(format!("failed to configure OpenAI query TLS: {err}"))
            })?
            .timeout(std::time::Duration::from_secs(
                config.request_timeout_seconds,
            ))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build OpenAI query HTTP client: {err}"))
            })?;

        Ok(Self {
            client,
            api_key,
            base_url: config.openai_base_url.clone(),
            auth: None,
            provider: "openai".to_owned(),
            search_model: config.web_search.default_model.clone(),
            search_context_size_supported: true,
        })
    }

    pub(super) fn new_configured(
        config: &OpenAiResponsesProviderConfig,
        default_model: String,
        request_timeout_seconds: u64,
    ) -> Result<Self, LlmError> {
        let provider = config.id.as_str();
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| LlmError::config(format!("{} is required", config.api_key_env)))?;
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| {
                LlmError::config(format!(
                    "failed to configure {provider} web search TLS: {err}"
                ))
            })?
            .timeout(std::time::Duration::from_secs(
                config
                    .request_timeout_seconds
                    .unwrap_or(request_timeout_seconds),
            ))
            .build()
            .map_err(|err| {
                LlmError::config(format!(
                    "failed to build {provider} web search HTTP client: {err}"
                ))
            })?;

        Ok(Self {
            client,
            api_key,
            base_url: Some(config.base_url.clone()),
            auth: Some(config.auth.clone()),
            provider: provider.to_owned(),
            search_model: default_model,
            // search_context_size 是 OpenAI 扩展字段；通用 Responses Provider
            // 只发送标准 web_search tool，避免向 XAI 等上游夹带未知参数。
            search_context_size_supported: false,
        })
    }
}

#[async_trait]
impl WebSearchExecutor for ResponsesWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "query must not be empty",
                "request",
            ));
        }

        let started = Instant::now();
        let max_results = configured_max_results(req.max_results);
        let model = req.model_override.as_deref().unwrap_or(&self.search_model);
        let payload = responses_web_search_payload(
            &req,
            query,
            max_results,
            model,
            false,
            self.search_context_size_supported,
        );
        let url = openai_responses_url(self.base_url.as_deref());
        trace_responses_query_payload(&req, &url, &payload);

        let response = send_openai_responses_request(
            &self.client,
            &self.api_key,
            self.base_url.as_deref(),
            self.auth.as_ref(),
            &payload,
            ResponsesTransportContext {
                provider: &self.provider,
                model,
                stream: false,
            },
        )
        .await
        .map_err(|err| err.with_upstream_context(self.provider.clone(), model.to_owned()))?;

        let body: Value = response
            .json()
            .await
            .map_err(|err| {
                LlmError::from_response_source(&err, "failed to read Responses web search JSON")
            })
            .map_err(|err| err.with_upstream_context(self.provider.clone(), model.to_owned()))?;
        let answer = extract_output_text(&body).ok_or_else(|| {
            LlmError::provider(
                "Responses web search returned empty text output",
                "provider",
            )
        })?;
        let sources = extract_sources(&body, usize::from(max_results));

        Ok(WebSearchOutcome {
            answer,
            sources,
            provider: self.provider.clone(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "query must not be empty",
                "request",
            ));
        }

        let started = Instant::now();
        let max_results = configured_max_results(req.max_results);
        let model = req.model_override.as_deref().unwrap_or(&self.search_model);
        let payload = responses_web_search_payload(
            &req,
            query,
            max_results,
            model,
            true,
            self.search_context_size_supported,
        );
        let url = openai_responses_url(self.base_url.as_deref());
        trace_responses_query_payload(&req, &url, &payload);

        let mut response = send_openai_responses_request(
            &self.client,
            &self.api_key,
            self.base_url.as_deref(),
            self.auth.as_ref(),
            &payload,
            ResponsesTransportContext {
                provider: &self.provider,
                model,
                stream: true,
            },
        )
        .await
        .map_err(|err| err.with_upstream_context(self.provider.clone(), model.to_owned()))?;

        let mut frame_buffer = Vec::new();
        let mut answer = String::new();
        let mut completed_response: Option<Value> = None;
        let mut saw_completed = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| web_search_stream_transport_error(err, &answer))
            .map_err(|err| err.with_upstream_context(self.provider.clone(), model.to_owned()))?
        {
            frame_buffer.extend_from_slice(&chunk);
            while let Some(frame) = take_sse_frame(&mut frame_buffer) {
                let Some(event) = parse_sse_frame(&frame)? else {
                    continue;
                };
                // 部分 OpenAI 兼容供应商会在 response.completed 后追加 `[DONE]`；
                // 它只是流结束哨兵，不能继续按 JSON 事件解析。
                if is_openai_responses_done_sentinel(&event.data) {
                    continue;
                }
                handle_responses_web_search_stream_event(
                    event,
                    &mut answer,
                    &mut completed_response,
                    &mut saw_completed,
                    &delta_tx,
                )
                .await?;
            }
        }
        if !frame_buffer.is_empty()
            && let Some(event) = parse_sse_frame(&frame_buffer)?
            && !is_openai_responses_done_sentinel(&event.data)
        {
            handle_responses_web_search_stream_event(
                event,
                &mut answer,
                &mut completed_response,
                &mut saw_completed,
                &delta_tx,
            )
            .await?;
        }

        if !saw_completed {
            return Err(web_search_incomplete_eof_error(&answer));
        }

        if answer.trim().is_empty()
            && let Some(response) = completed_response.as_ref()
        {
            answer = extract_output_text(response).unwrap_or_default();
        }
        let answer = answer.trim().to_owned();
        if answer.is_empty() {
            return Err(LlmError::provider(
                "Responses web search returned empty text output",
                "provider",
            ));
        }
        let sources = completed_response
            .as_ref()
            .map(|response| extract_sources(response, usize::from(max_results)))
            .unwrap_or_default();

        Ok(WebSearchOutcome {
            answer,
            sources,
            provider: self.provider.clone(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    fn provider_name(&self) -> &'static str {
        "openai_responses"
    }
}

fn normalized_context_size(value: Option<&str>) -> &str {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("low") => "low",
        Some("medium") => "medium",
        Some("high") => "high",
        _ => DEFAULT_SEARCH_CONTEXT_SIZE,
    }
}

fn responses_web_search_payload(
    req: &WebSearchRequest,
    query: &str,
    max_results: u8,
    search_model: &str,
    stream: bool,
    search_context_size_supported: bool,
) -> Value {
    let tool = if search_context_size_supported {
        json!({
            "type": "web_search",
            "search_context_size": normalized_context_size(req.context_size.as_deref())
        })
    } else {
        json!({"type": "web_search"})
    };

    let mut payload = json!({
        "model": search_model,
        "tools": [tool],
        "tool_choice": "required",
        "include": ["web_search_call.action.sources"],
        "input": build_query_prompt(
            query,
            req.raw_question.as_deref(),
            max_results,
            &request_time_context()
        ),
    });
    if stream {
        payload["stream"] = json!(true);
    }
    payload
}

fn trace_responses_query_payload(req: &WebSearchRequest, url: &str, payload: &Value) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }

    let input = payload
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_choice = payload
        .get("tool_choice")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tools = payload.get("tools").unwrap_or(&Value::Null).to_string();
    let include = payload.get("include").unwrap_or(&Value::Null).to_string();
    tracing::trace!(
        upstream_url = url,
        model = model,
        tool_choice = tool_choice,
        tools = %tools,
        include = %include,
        input_chars = input.chars().count(),
        query_chars = req.query.trim().chars().count(),
        "Responses 联网搜索请求载荷摘要"
    );

    if trace_query_input_enabled() {
        tracing::trace!(
            upstream_url = url,
            input = %input,
            "Responses 联网搜索请求输入"
        );
    }
}

async fn handle_responses_web_search_stream_event(
    event: SseFrame,
    answer: &mut String,
    completed_response: &mut Option<Value>,
    saw_completed: &mut bool,
    delta_tx: &mpsc::Sender<String>,
) -> Result<(), LlmError> {
    let value = serde_json::from_str::<Value>(&event.data).map_err(|err| {
        LlmError::provider(
            format!("invalid Responses web search stream JSON: {err}"),
            "sse",
        )
    })?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");

    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                answer.push_str(delta);
                let _ = delta_tx.send(delta.to_owned()).await;
            }
        }
        "response.completed" => {
            *saw_completed = true;
            *completed_response = value
                .get("response")
                .cloned()
                .or_else(|| Some(value.clone()));
        }
        "response.failed" | "response.incomplete" | "error" => {
            let message = stream_error_message(&value)
                .unwrap_or_else(|| format!("Responses web search stream event {event_type}"));
            return Err(LlmError::provider(message, "sse"));
        }
        _ => {}
    }

    Ok(())
}

fn web_search_incomplete_eof_error(answer: &str) -> LlmError {
    let stage = if answer.trim().is_empty() {
        "stream"
    } else {
        "stream_after_delta"
    };
    LlmError::provider(
        "Responses web search stream ended before response.completed",
        stage,
    )
}

fn web_search_stream_transport_error(err: reqwest::Error, answer: &str) -> LlmError {
    let stage = if answer.trim().is_empty() {
        "web_search_stream_read"
    } else {
        "web_search_stream_read_after_delta"
    };
    LlmError::from_error_source(
        &err,
        LlmErrorKind::Network,
        stage,
        "Responses web search stream failed",
    )
}

fn stream_error_message(value: &Value) -> Option<String> {
    value
        .get("error")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .and_then(|error| error.get("message").or(Some(error)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn extract_output_text(body: &Value) -> Option<String> {
    if let Some(text) = body
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_owned());
    }

    let output = body.get("output").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for output_item in output {
        let Some(content_items) = output_item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for content_item in content_items {
            let item_type = content_item.get("type").and_then(Value::as_str);
            if !matches!(item_type, Some("output_text") | None) {
                continue;
            }
            let Some(text) = content_item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            parts.push(text.to_owned());
        }
    }

    let answer = parts.join("\n\n");
    let answer = answer.trim();
    if answer.is_empty() {
        None
    } else {
        Some(answer.to_owned())
    }
}

fn extract_sources(body: &Value, max_results: usize) -> Vec<WebSearchSource> {
    let mut sources = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    if let Some(output) = body.get("output").and_then(Value::as_array) {
        for output_item in output {
            if let Some(action_sources) = output_item
                .get("action")
                .and_then(|action| action.get("sources"))
                .and_then(Value::as_array)
            {
                collect_sources(action_sources, &mut sources, &mut seen_urls, max_results);
            }

            if let Some(content_items) = output_item.get("content").and_then(Value::as_array) {
                for content_item in content_items {
                    if let Some(annotations) =
                        content_item.get("annotations").and_then(Value::as_array)
                    {
                        collect_sources(annotations, &mut sources, &mut seen_urls, max_results);
                    }
                }
            }

            if sources.len() >= max_results {
                break;
            }
        }
    }

    sources
}

fn collect_sources(
    values: &[Value],
    sources: &mut Vec<WebSearchSource>,
    seen_urls: &mut std::collections::HashSet<String>,
    max_results: usize,
) {
    for value in values {
        if sources.len() >= max_results {
            return;
        }
        let Some(url) = value.get("url").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if url.is_empty() || seen_urls.contains(url) {
            continue;
        }
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(url);
        let snippet = value
            .get("snippet")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        sources.push(WebSearchSource {
            title: title.to_owned(),
            url: url.to_owned(),
            snippet: snippet.to_owned(),
        });
        seen_urls.insert(url.to_owned());
    }
}

#[cfg(test)]
mod configured_search_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Response, StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use futures::stream;
    use std::{convert::Infallible, sync::Arc, time::Duration};
    use tokio::{net::TcpListener, sync::Mutex};

    #[test]
    fn openai_url_uses_default_or_custom_base() {
        assert_eq!(
            openai_responses_url(None),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            openai_responses_url(Some("https://proxy.example/v1/")),
            "https://proxy.example/v1/responses"
        );
    }

    #[test]
    fn normal_payload_uses_web_search_context_size() {
        let req = WebSearchRequest {
            query: "Cloudflare D1".to_owned(),
            raw_question: None,
            max_results: Some(3),
            context_size: Some("high".to_owned()),
            topic: None,
            time_range: None,
            backend_override: None,
            model_override: None,
        };
        let payload = responses_web_search_payload(&req, &req.query, 3, "gpt-search", false, true);

        assert_eq!(payload["model"], "gpt-search");
        assert_eq!(payload["tools"][0]["type"], "web_search");
        assert_eq!(payload["tools"][0]["search_context_size"], "high");
        assert_eq!(payload["tool_choice"], "required");
        assert!(
            payload["input"]
                .as_str()
                .unwrap()
                .contains("参考来源最多列出 3 条")
        );
        assert!(
            payload["input"]
                .as_str()
                .unwrap()
                .contains("当前本地日期：")
        );
        assert!(payload.get("stream").is_none());
    }

    #[test]
    fn stream_payload_sets_stream_flag() {
        let req = WebSearchRequest {
            query: "Cloudflare D1".to_owned(),
            raw_question: None,
            max_results: Some(3),
            context_size: None,
            topic: None,
            time_range: None,
            backend_override: None,
            model_override: None,
        };
        let payload = responses_web_search_payload(&req, &req.query, 3, "gpt-search", true, true);

        assert_eq!(payload["stream"], true);
    }

    #[test]
    fn parses_sse_frames_across_chunks() {
        let mut buffer = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你"
            .as_bytes()
            .to_vec();
        assert!(take_sse_frame(&mut buffer).is_none());
        buffer.extend_from_slice("好\"}\n\n".as_bytes());

        let frame = take_sse_frame(&mut buffer).unwrap();
        let parsed = parse_sse_frame(&frame).unwrap().unwrap();

        assert_eq!(parsed.event.as_deref(), Some("response.output_text.delta"));
        assert!(parsed.data.contains("你好"));
    }

    #[derive(Debug)]
    struct MockSearchState {
        body: String,
        requests: Vec<Value>,
    }

    async fn mock_search_handler(
        State(state): State<Arc<Mutex<MockSearchState>>>,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let mut state = state.lock().await;
        state.requests.push(request);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            state.body.clone(),
        )
    }

    async fn spawn_mock_search(body: String) -> (String, Arc<Mutex<MockSearchState>>) {
        let state = Arc::new(Mutex::new(MockSearchState {
            body,
            requests: Vec::new(),
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

    async fn stalled_search_stream_handler() -> Response<Body> {
        let body = Body::from_stream(stream::pending::<Result<Bytes, Infallible>>());
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap()
    }

    async fn spawn_stalled_search_stream() -> String {
        let app = Router::new().route("/v1/responses", post(stalled_search_stream_handler));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn configured_responses_query_stream_emits_deltas_and_accepts_done_sentinel() {
        let body = concat!(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"好\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"你好\",\"output\":[]}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_owned();
        let (base_url, state) = spawn_mock_search(body).await;
        let executor = ResponsesWebSearchExecutor {
            client: qq_maid_common::http_client::client(),
            api_key: "test-key".to_owned(),
            base_url: Some(base_url),
            auth: Some(HttpAuthConfig::default()),
            provider: "xai".to_owned(),
            search_model: "gpt-search".to_owned(),
            search_context_size_supported: false,
        };
        let (delta_tx, mut delta_rx) = mpsc::channel(4);

        let outcome = executor
            .query_stream(
                WebSearchRequest {
                    query: "测试".to_owned(),
                    raw_question: Some("/查 测试".to_owned()),
                    max_results: None,
                    context_size: None,
                    topic: None,
                    time_range: None,
                    backend_override: None,
                    model_override: None,
                },
                delta_tx,
            )
            .await
            .unwrap();

        assert_eq!(delta_rx.recv().await.as_deref(), Some("你"));
        assert_eq!(delta_rx.recv().await.as_deref(), Some("好"));
        assert!(delta_rx.recv().await.is_none());
        assert_eq!(outcome.answer, "你好");
        assert_eq!(outcome.provider, "xai");
        assert_eq!(state.lock().await.requests[0]["stream"], true);
    }

    #[tokio::test]
    async fn responses_search_stream_read_timeout_stays_timeout() {
        let base_url = spawn_stalled_search_stream().await;
        let client = qq_maid_common::http_client::try_builder()
            .unwrap()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let executor = ResponsesWebSearchExecutor {
            client,
            api_key: "test-key".to_owned(),
            base_url: Some(base_url),
            auth: None,
            provider: "openai".to_owned(),
            search_model: "gpt-search".to_owned(),
            search_context_size_supported: true,
        };
        let (delta_tx, _delta_rx) = mpsc::channel(1);

        let error = executor
            .query_stream(
                WebSearchRequest {
                    query: "测试".to_owned(),
                    raw_question: None,
                    max_results: None,
                    context_size: None,
                    topic: None,
                    time_range: None,
                    backend_override: None,
                    model_override: None,
                },
                delta_tx,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "timeout");
        assert_eq!(error.kind(), LlmErrorKind::Timeout);
        assert_eq!(error.stage, "web_search_stream_read");
        assert_eq!(error.upstream_provider(), Some("openai"));
        assert_eq!(error.upstream_model(), Some("gpt-search"));
    }

    #[tokio::test]
    async fn query_stream_rejects_partial_delta_without_completed() {
        let body = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"半截\"}\n\n"
            .to_owned();
        let (base_url, _state) = spawn_mock_search(body).await;
        let executor = ResponsesWebSearchExecutor {
            client: qq_maid_common::http_client::client(),
            api_key: "test-key".to_owned(),
            base_url: Some(base_url),
            auth: None,
            provider: "openai".to_owned(),
            search_model: "gpt-search".to_owned(),
            search_context_size_supported: true,
        };
        let (delta_tx, _delta_rx) = mpsc::channel(4);

        let err = executor
            .query_stream(
                WebSearchRequest {
                    query: "测试".to_owned(),
                    raw_question: Some("/查 测试".to_owned()),
                    max_results: None,
                    context_size: None,
                    topic: None,
                    time_range: None,
                    backend_override: None,
                    model_override: None,
                },
                delta_tx,
            )
            .await
            .unwrap_err();

        assert_eq!(err.stage, "stream_after_delta");
        assert!(err.message.contains("response.completed"));
    }

    #[test]
    fn extracts_output_text_from_various_shapes() {
        let body = json!({
            "output": [{
                "type": "message",
                "content": [
                    {"type": "output_text", "text": "first"},
                    {"type": "refusal", "refusal": "skip"},
                    {"type": "output_text", "text": "second"}
                ]
            }]
        });

        assert_eq!(
            extract_output_text(&body).as_deref(),
            Some("first\n\nsecond")
        );
    }

    #[test]
    fn extracts_sources_from_action_and_annotations() {
        let body = json!({
            "output_text": "answer",
            "output": [
                {
                    "action": {
                        "sources": [
                            {"title": "A", "url": "https://a.test", "snippet": "aa"}
                        ]
                    },
                    "content": [
                        {
                            "annotations": [
                                {"title": "A duplicate", "url": "https://a.test"},
                                {"title": "B", "url": "https://b.test"}
                            ]
                        }
                    ]
                }
            ]
        });

        let sources = extract_sources(&body, 5);

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].title, "A");
        assert_eq!(sources[0].snippet, "aa");
        assert_eq!(sources[1].url, "https://b.test");
    }
}
