use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::{error::LlmError, metrics::duration_ms};

use super::{
    MAX_RESULTS_LIMIT, WebSearchConfig, WebSearchExecutor, WebSearchOutcome, WebSearchRequest,
    WebSearchSource, WebSearchTimeRange, WebSearchTopic, truncate_error_detail,
};

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const TAVILY_SNIPPET_MAX_CHARS: usize = 1_000;

pub(super) struct MissingTavilyWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for MissingTavilyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::new(
            "web_search_not_configured",
            "TAVILY_API_KEY is required when tools.web_search.backend is tavily",
            "config",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "tavily"
    }
}

/// Tavily Search API 执行器。连接、首响应和请求总时长分别受限，避免搜索占满 Agent 预算。
pub(super) struct TavilyWebSearchExecutor {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
    config: WebSearchConfig,
}

impl TavilyWebSearchExecutor {
    pub(super) fn new(api_key: String, config: WebSearchConfig) -> Result<Self, LlmError> {
        validate_timeouts(&config)?;
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| LlmError::config(format!("failed to configure Tavily TLS: {err}")))?
            .connect_timeout(Duration::from_secs(config.connect_timeout_seconds))
            .timeout(Duration::from_secs(config.total_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build Tavily HTTP client: {err}"))
            })?;
        Ok(Self {
            client,
            api_key,
            endpoint: TAVILY_SEARCH_URL.to_owned(),
            config,
        })
    }

    async fn send(&self, payload: &TavilySearchRequest<'_>) -> Result<reqwest::Response, LlmError> {
        let request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(payload)
            .send();
        tokio::time::timeout(
            Duration::from_secs(self.config.first_response_timeout_seconds),
            request,
        )
        .await
        .map_err(|_| {
            LlmError::new(
                "timeout",
                "Tavily did not return response headers before the configured deadline",
                "tavily_first_response",
            )
        })?
        .map_err(tavily_transport_error)
    }
}

#[async_trait]
impl WebSearchExecutor for TavilyWebSearchExecutor {
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
        let topic = request_topic(req.topic.as_deref(), self.config.topic)?;
        let time_range = request_time_range(req.time_range.as_deref(), self.config.time_range)?;
        let payload = TavilySearchRequest {
            query,
            search_depth: self.config.search_depth.as_str(),
            max_results: req
                .max_results
                .unwrap_or(self.config.max_results)
                .clamp(1, MAX_RESULTS_LIMIT),
            topic: topic.as_str(),
            time_range: time_range.map(WebSearchTimeRange::as_str),
            include_answer: "basic",
            include_raw_content: false,
            include_images: false,
        };
        let response = self.send(&payload).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(tavily_status_error(status, response).await);
        }

        let body: TavilySearchResponse = response.json().await.map_err(|err| {
            if err.is_timeout() {
                LlmError::new(
                    "timeout",
                    "Tavily response body exceeded the configured total timeout",
                    "tavily_total",
                )
            } else {
                LlmError::provider(format!("invalid Tavily search JSON: {err}"), "tavily_json")
            }
        })?;
        let sources = tavily_sources(body.results, usize::from(payload.max_results));
        if sources.is_empty() {
            return Err(LlmError::new(
                "empty_result",
                "Tavily search returned no usable results",
                "tavily_search",
            ));
        }

        Ok(WebSearchOutcome {
            answer: render_tavily_answer(body.answer.as_deref(), &sources),
            sources,
            provider: "tavily".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    fn provider_name(&self) -> &'static str {
        "tavily"
    }
}

#[derive(Serialize)]
struct TavilySearchRequest<'a> {
    query: &'a str,
    search_depth: &'static str,
    max_results: u8,
    topic: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_range: Option<&'static str>,
    include_answer: &'static str,
    include_raw_content: bool,
    include_images: bool,
}

#[derive(Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilySearchResult>,
}

#[derive(Deserialize)]
struct TavilySearchResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

fn validate_timeouts(config: &WebSearchConfig) -> Result<(), LlmError> {
    if config.connect_timeout_seconds == 0
        || config.first_response_timeout_seconds == 0
        || config.total_timeout_seconds == 0
    {
        return Err(LlmError::config(
            "tools.web_search timeout values must be greater than zero",
        ));
    }
    if config.connect_timeout_seconds > config.first_response_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.connect_timeout_seconds must not exceed first_response_timeout_seconds",
        ));
    }
    if config.first_response_timeout_seconds > config.total_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.first_response_timeout_seconds must not exceed total_timeout_seconds",
        ));
    }
    Ok(())
}

fn request_topic(
    value: Option<&str>,
    configured: WebSearchTopic,
) -> Result<WebSearchTopic, LlmError> {
    value
        .map(|value| {
            WebSearchTopic::parse_config(value, "topic").map_err(|_| {
                LlmError::new(
                    "bad_request",
                    "topic must be general, news, or finance",
                    "request",
                )
            })
        })
        .unwrap_or(Ok(configured))
}

fn request_time_range(
    value: Option<&str>,
    configured: Option<WebSearchTimeRange>,
) -> Result<Option<WebSearchTimeRange>, LlmError> {
    value
        .map(|value| {
            WebSearchTimeRange::parse_config(value, "time_range")
                .map(Some)
                .map_err(|_| {
                    LlmError::new(
                        "bad_request",
                        "time_range must be day, week, month, or year",
                        "request",
                    )
                })
        })
        .unwrap_or(Ok(configured))
}

fn tavily_sources(results: Vec<TavilySearchResult>, limit: usize) -> Vec<WebSearchSource> {
    results
        .into_iter()
        .filter_map(|result| {
            let title = result.title.trim();
            let parsed_url = Url::parse(result.url.trim()).ok()?;
            if !matches!(parsed_url.scheme(), "http" | "https") {
                return None;
            }
            Some(WebSearchSource {
                title: if title.is_empty() {
                    parsed_url
                        .host_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| "未命名来源".to_owned())
                } else {
                    truncate_chars(title, 200)
                },
                url: parsed_url.to_string(),
                snippet: truncate_chars(result.content.trim(), TAVILY_SNIPPET_MAX_CHARS),
            })
        })
        .take(limit)
        .collect()
}

fn render_tavily_answer(answer: Option<&str>, sources: &[WebSearchSource]) -> String {
    let answer = answer.map(str::trim).filter(|value| !value.is_empty());
    let mut output = answer
        .map(str::to_owned)
        .unwrap_or_else(|| "搜索到以下公开网页结果：".to_owned());
    output.push_str("\n\n参考来源：");
    for source in sources {
        output.push_str("\n- [");
        output.push_str(&escape_markdown_label(&source.title));
        output.push_str("](");
        output.push_str(&source.url);
        output.push(')');
    }
    output
}

fn escape_markdown_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit).collect()
}

fn tavily_transport_error(err: reqwest::Error) -> LlmError {
    if err.is_timeout() && err.is_connect() {
        LlmError::new(
            "timeout",
            "Tavily connection exceeded the configured timeout",
            "tavily_connect",
        )
    } else if err.is_timeout() {
        LlmError::new(
            "timeout",
            "Tavily request exceeded the configured total timeout",
            "tavily_total",
        )
    } else {
        LlmError::http(format!("Tavily search request failed: {err}"))
    }
}

async fn tavily_status_error(status: StatusCode, response: reqwest::Response) -> LlmError {
    let detail = response.text().await.unwrap_or_default();
    let detail = truncate_error_detail(&detail, 300);
    match status.as_u16() {
        401 => LlmError::new(
            "tavily_auth_error",
            "Tavily rejected the configured API key",
            "tavily_http",
        ),
        429 => LlmError::new("rate_limited", "Tavily rate limit exceeded", "tavily_http"),
        432 | 433 => LlmError::new(
            "quota_exhausted",
            "Tavily plan or usage quota is exhausted",
            "tavily_http",
        ),
        _ => LlmError::provider(
            format!("Tavily search returned HTTP {status}: {detail}"),
            "tavily_http",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Default)]
    struct MockState {
        requests: Vec<Value>,
        authorization: Option<String>,
        status: u16,
        response: Value,
        delay_ms: u64,
    }

    async fn mock_search(
        State(state): State<Arc<Mutex<MockState>>>,
        headers: HeaderMap,
        Json(payload): Json<Value>,
    ) -> impl IntoResponse {
        let delay_ms = state.lock().await.delay_ms;
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let mut state = state.lock().await;
        state.requests.push(payload);
        state.authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        (
            StatusCode::from_u16(state.status).unwrap(),
            Json(state.response.clone()),
        )
    }

    async fn test_executor(
        status: u16,
        response: Value,
    ) -> (TavilyWebSearchExecutor, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState {
            status,
            response,
            ..MockState::default()
        }));
        let app = Router::new()
            .route("/search", post(mock_search))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut executor =
            TavilyWebSearchExecutor::new("test-key".to_owned(), WebSearchConfig::default())
                .unwrap();
        executor.endpoint = format!("http://{addr}/search");
        (executor, state)
    }

    fn request() -> WebSearchRequest {
        WebSearchRequest {
            query: "Rust 2026".to_owned(),
            raw_question: None,
            max_results: Some(3),
            context_size: None,
            topic: Some("news".to_owned()),
            time_range: Some("week".to_owned()),
            backend_override: None,
            model_override: None,
        }
    }

    #[tokio::test]
    async fn query_maps_tavily_results_and_request_options() {
        let (executor, state) = test_executor(
            200,
            json!({
                "answer": "Rust 新闻摘要",
                "results": [{
                    "title": "Rust News",
                    "url": "https://example.com/rust",
                    "content": "正文摘要",
                    "domain": "example.com"
                }]
            }),
        )
        .await;

        let outcome = executor.query(request()).await.unwrap();

        assert_eq!(outcome.provider, "tavily");
        assert_eq!(outcome.sources[0].snippet, "正文摘要");
        assert!(outcome.answer.contains("https://example.com/rust"));
        let state = state.lock().await;
        assert_eq!(state.authorization.as_deref(), Some("Bearer test-key"));
        assert_eq!(state.requests[0]["search_depth"], "basic");
        assert_eq!(state.requests[0]["max_results"], 3);
        assert_eq!(state.requests[0]["topic"], "news");
        assert_eq!(state.requests[0]["time_range"], "week");
        assert_eq!(state.requests[0]["include_answer"], "basic");
    }

    #[tokio::test]
    async fn query_classifies_auth_quota_and_empty_results() {
        let (executor, _) = test_executor(401, json!({"detail": "invalid key"})).await;
        assert_eq!(
            executor.query(request()).await.unwrap_err().code,
            "tavily_auth_error"
        );

        let (executor, _) = test_executor(429, json!({"detail": "rate limit"})).await;
        assert_eq!(
            executor.query(request()).await.unwrap_err().code,
            "rate_limited"
        );

        let (executor, _) = test_executor(432, json!({"detail": "plan limit"})).await;
        assert_eq!(
            executor.query(request()).await.unwrap_err().code,
            "quota_exhausted"
        );

        let (executor, _) = test_executor(200, json!({"results": []})).await;
        assert_eq!(
            executor.query(request()).await.unwrap_err().code,
            "empty_result"
        );
    }

    #[tokio::test]
    async fn query_classifies_first_response_timeout() {
        let (mut executor, state) = test_executor(200, json!({"results": []})).await;
        executor.config.first_response_timeout_seconds = 1;
        state.lock().await.delay_ms = 1_100;

        let error = executor.query(request()).await.unwrap_err();

        assert_eq!(error.code, "timeout");
        assert_eq!(error.stage, "tavily_first_response");
    }

    #[test]
    fn timeout_order_is_validated() {
        let config = WebSearchConfig {
            connect_timeout_seconds: 31,
            first_response_timeout_seconds: 30,
            ..WebSearchConfig::default()
        };
        assert!(TavilyWebSearchExecutor::new("key".to_owned(), config).is_err());
    }
}
