//! HTTP 路由和请求处理器。
//!
//! 定义 `/healthz` 健康检查和 `/v1/respond` 聊天响应接口。
//! 路由层负责参数校验、超时控制、错误处理和指标记录。

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::stream;
use serde::Deserialize;
use serde_json::json;
use tokio::time::timeout;
use tracing::{error, info, warn};

use crate::{
    config::AppConfig,
    error::LlmError,
    provider::DynLlmProvider,
    runtime::{
        memory::MemoryStore,
        prompt::PromptConfig,
        query::DynQueryExecutor,
        respond::{
            RespondRequest, RespondResponse, RespondServiceOptions, RespondStores,
            RespondStreamEvent, RespondTransport, RustRespondService,
        },
        rss::{RssFetcher, RssStore},
        session::SessionStore,
        todo::TodoStore,
        weather::DynWeatherExecutor,
    },
    util::metrics::MetricsRecorder,
};

/// 应用全局状态，通过 Axum 的 State 注入到各处理器中。
#[derive(Clone)]
pub struct AppState {
    /// 全局应用配置。
    pub config: AppConfig,
    /// LLM 提供商（可为主备模式）。
    pub provider: DynLlmProvider,
    /// 联网搜索执行器。
    pub query_executor: DynQueryExecutor,
    /// 天气查询执行器。
    pub weather_executor: DynWeatherExecutor,
    /// 记忆存储。
    pub memory_store: MemoryStore,
    /// 会话存储。
    pub session_store: SessionStore,
    /// 待办事项存储。
    pub todo_store: TodoStore,
    /// RSS 订阅存储。
    pub rss_store: RssStore,
    /// RSS / Atom 拉取解析器。
    pub rss_fetcher: RssFetcher,
    /// 提示词配置（system prompt 模板等）。
    pub prompt_config: PromptConfig,
}

/// HTTP /v1/respond 接口的请求体。
///
/// 使用 `#[serde(deny_unknown_fields)]` 拒绝旧版遗留字段。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HttpRespondRequest {
    /// 会话作用域键，用于隔离不同会话的上下文。
    scope_key: String,
    /// 用户发送的文本内容。
    content: String,
    /// 消息来源平台（如 `qq_official`）。
    platform: String,
    /// 事件类型（如 `FakeEvent`）。
    event_type: String,
    /// 发送者用户 ID。
    #[serde(default)]
    user_id: Option<String>,
    /// 群聊 ID。
    #[serde(default)]
    group_id: Option<String>,
    /// 频道/子区 ID（用于 guild 场景）。
    #[serde(default)]
    guild_id: Option<String>,
    /// 子频道 ID。
    #[serde(default)]
    channel_id: Option<String>,
    /// 消息 ID。
    #[serde(default)]
    message_id: Option<String>,
    /// 消息时间戳。
    #[serde(default)]
    timestamp: Option<String>,
}

impl From<HttpRespondRequest> for RespondRequest {
    fn from(value: HttpRespondRequest) -> Self {
        Self {
            content: value.content,
            scope_key: value.scope_key,
            user_id: value.user_id,
            group_id: value.group_id,
            guild_id: value.guild_id,
            channel_id: value.channel_id,
            message_id: value.message_id,
            timestamp: value.timestamp,
            platform: value.platform,
            event_type: value.event_type,
            ..Default::default()
        }
    }
}

/// 构建 Axum 路由树，注册所有 HTTP 端点。
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/respond", post(respond))
        .with_state(state)
}

/// 健康检查端点，返回当前提供商和模型信息。
async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "provider": state.provider.name(),
        "model": state.provider.model(),
        "stream": state.provider.stream_enabled(),
    }))
}

/// /v1/respond 处理器：解析请求、调用 LLM 服务并返回结果。
///
/// 负责：
/// - 请求体 JSON 反序列化与校验
/// - 创建 [`RustRespondService`] 实例
/// - 超时控制（由 `request_timeout_seconds` 配置）
/// - 成功 / 业务错误 / 超时三种响应的指标记录与错误日志
async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<HttpRespondRequest>, JsonRejection>,
) -> Response {
    let Json(req) = match payload {
        Ok(req) => req,
        Err(err) => {
            warn!(error = %err, "respond request payload rejected");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": {
                        "code": "invalid_request",
                        "message": "invalid /v1/respond payload",
                        "stage": "http",
                    }
                })),
            )
                .into_response();
        }
    };
    let req: RespondRequest = req.into();
    let service = RustRespondService::new(
        state.provider.clone(),
        state.query_executor.clone(),
        state.weather_executor.clone(),
        RespondStores {
            memory_store: state.memory_store.clone(),
            session_store: state.session_store.clone(),
            todo_store: state.todo_store.clone(),
            rss_store: state.rss_store.clone(),
        },
        state.rss_fetcher.clone(),
        state.prompt_config.clone(),
        RespondServiceOptions {
            title_model: state.config.title_model.clone(),
            todo_model: state.config.todo_model.clone(),
            memory_model: state.config.memory_model.clone(),
            compact_model: state.config.compact_model.clone(),
            send_mode: state.config.send_mode.clone(),
            rss_summary_max_chars: state.config.rss_summary_max_chars as usize,
            rss_seen_retention: state.config.rss_seen_retention as usize,
        },
    );
    let recorder = MetricsRecorder::start();
    let scope_key = req.scope_key.clone();
    let allow_streaming =
        state.config.send_mode.eq_ignore_ascii_case("streaming") && accepts_streaming(&headers);
    let result = timeout(
        Duration::from_secs(state.config.request_timeout_seconds),
        service.respond_transport(req, allow_streaming),
    )
    .await;

    match result {
        Ok(Ok(RespondTransport::Json(response))) => {
            if let Some(text) = response.text.as_deref() {
                info!(
                    scope_key = response.session_id.as_deref().unwrap_or(&scope_key),
                    reply_len = text.chars().count(),
                    command = response.command.as_deref().unwrap_or("chat"),
                    "respond request succeeded"
                );
            }
            Json(*response).into_response()
        }
        Ok(Ok(RespondTransport::Stream(stream))) => {
            // 目前只有 `/查` 联网搜索会返回 SSE，这里显式记录 command，
            // 便于和普通 chat 请求区分，避免排障时误判成聊天链路。
            info!(scope_key = %scope_key, command = "web_search", "respond request streaming");
            stream_response(stream)
        }
        Ok(Err(err)) => {
            warn_respond_error(&scope_key, &err);
            let metrics = recorder.fail(
                state.provider.name(),
                state.provider.model(),
                state.provider.stream_enabled(),
            );
            Json(RespondResponse {
                ok: false,
                text: None,
                handled: Some(false),
                session_id: None,
                command: None,
                diagnostics: None,
                metrics,
                usage: None,
                error: Some(err.as_info()),
            })
            .into_response()
        }
        Err(_) => {
            let err = LlmError::timeout("request");
            error_respond_error(&scope_key, &err);
            let metrics = recorder.fail(
                state.provider.name(),
                state.provider.model(),
                state.provider.stream_enabled(),
            );
            Json(RespondResponse {
                ok: false,
                text: None,
                handled: Some(false),
                session_id: None,
                command: None,
                diagnostics: None,
                metrics,
                usage: None,
                error: Some(err.as_info()),
            })
            .into_response()
        }
    }
}

fn accepts_streaming(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn stream_response(stream: crate::runtime::respond::RespondStream) -> Response {
    let sse_stream = stream::unfold(stream.receiver, |mut receiver| async move {
        let event = receiver.recv().await?;
        let event = match event {
            RespondStreamEvent::Delta { text } => axum::response::sse::Event::default()
                .event("delta")
                .data(text),
            RespondStreamEvent::Final { response } => axum::response::sse::Event::default()
                .event("final")
                .data(serde_json::to_string(&response).unwrap_or_else(|_| {
                    "{\"ok\":false,\"error\":{\"code\":\"http_error\",\"message\":\"stream response serialization failed\",\"stage\":\"http\"}}".to_owned()
                })),
        };
        Some((Ok::<_, std::convert::Infallible>(event), receiver))
    });
    axum::response::sse::Sse::new(sse_stream).into_response()
}

/// 记录业务错误的警告日志。
fn warn_respond_error(scope_key: &str, err: &LlmError) {
    warn!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        "respond request failed"
    );
}

/// 记录请求超时的错误日志。
fn error_respond_error(scope_key: &str, err: &LlmError) {
    error!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        "respond request timed out"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_RSS_SUMMARY_MAX_CHARS, ProviderMode},
        provider::{
            ChatOutcome, LlmProvider,
            types::{ChatRequest, TokenUsage},
        },
        runtime::{
            prompt::PromptConfig,
            query::{QueryExecutor, QueryOutcome, QueryRequest, QuerySource},
            rss::RssFetchConfig,
            session::SessionStore,
            weather::{
                CurrentWeather, DailyWeather, WeatherExecutor, WeatherLocation, WeatherOutcome,
                WeatherRequest, WeatherSupplement,
            },
        },
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
        util::metrics::LlmMetrics,
    };
    use async_trait::async_trait;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::{convert::Infallible, fs, sync::Arc};
    use tower::ServiceExt;

    #[derive(Clone)]
    struct MockProvider;

    struct MockQueryExecutor;

    struct MockWeatherExecutor;

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            Ok(ChatOutcome {
                reply: "# 标题\n- hello".to_owned(),
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: "mock-model".to_owned(),
                    stream: true,
                    ttfe_ms: Some(1),
                    ttft_ms: Some(2),
                    total_latency_ms: 3,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
            })
        }

        fn name(&self) -> &'static str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            true
        }
    }

    #[async_trait]
    impl QueryExecutor for MockQueryExecutor {
        async fn query(&self, req: QueryRequest) -> Result<QueryOutcome, LlmError> {
            Ok(QueryOutcome {
                answer: format!("web answer: {}", req.query),
                sources: vec![QuerySource {
                    title: "Source A".to_owned(),
                    url: "https://a.test".to_owned(),
                    snippet: "snippet".to_owned(),
                }],
                provider: "mock-query".to_owned(),
                elapsed_ms: 7,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-query"
        }
    }

    #[async_trait]
    impl WeatherExecutor for MockWeatherExecutor {
        async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
            Ok(WeatherOutcome {
                location: WeatherLocation {
                    id: Some("101210101".to_owned()),
                    name: req.city,
                    country: Some("中国".to_owned()),
                    admin1: Some("浙江".to_owned()),
                    admin2: Some("杭州".to_owned()),
                    timezone: Some("Asia/Shanghai".to_owned()),
                    latitude: 30.29365,
                    longitude: 120.16142,
                },
                current: CurrentWeather {
                    time: "2026-06-12T20:15".to_owned(),
                    temperature_c: 27.7,
                    apparent_temperature_c: Some(28.5),
                    weather_code: 3,
                    humidity_percent: None,
                    precipitation_mm: None,
                    pressure_hpa: None,
                    wind_direction: None,
                    wind_scale: None,
                    wind_speed_kmh: Some(6.7),
                },
                daily: vec![DailyWeather {
                    date: "2026-06-12".to_owned(),
                    weather_code: 3,
                    weather_day: None,
                    weather_night: None,
                    temperature_max_c: 32.5,
                    temperature_min_c: 21.0,
                    precipitation_probability_max: Some(2),
                    precipitation_mm: None,
                    humidity_percent: None,
                    wind_direction_day: None,
                    wind_scale_day: None,
                }],
                provider: "mock-weather".to_owned(),
                elapsed_ms: 7,
                forecast_days: req.forecast_days,
                alerts: WeatherSupplement::default(),
                air_quality: WeatherSupplement::default(),
                life_indices: WeatherSupplement::default(),
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-weather"
        }
    }

    fn write_prompt_set(dir: &std::path::Path) {
        fs::create_dir_all(dir).unwrap();
        for file_name in crate::runtime::prompt::PROMPT_FILES {
            fs::write(dir.join(file_name), format!("{file_name} content")).unwrap();
        }
    }

    fn test_state() -> AppState {
        let prompt_dir = std::env::temp_dir().join(format!(
            "qq-maid-route-prompt-test-{}",
            uuid::Uuid::new_v4()
        ));
        write_prompt_set(&prompt_dir);
        let member_id_mapping_file = std::env::temp_dir().join(format!(
            "qq-maid-route-member-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        fs::write(&member_id_mapping_file, "{}").unwrap();
        let app_db_file = std::env::temp_dir().join(format!(
            "qq-maid-route-app-test-{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = SqliteDatabase::open(&app_db_file, APP_MIGRATIONS).unwrap();

        AppState {
            config: AppConfig {
                provider: ProviderMode::OpenAi,
                model: "mock-model".to_owned(),
                title_model: None,
                todo_model: None,
                memory_model: None,
                compact_model: None,
                openai_search_model: "mock-search-model".to_owned(),
                openai_api_key: Some("test".to_owned()),
                openai_base_url: None,
                deepseek_api_key: None,
                deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
                deepseek_model: "deepseek-chat".to_owned(),
                stream: true,
                send_mode: "final".to_owned(),
                request_timeout_seconds: 5,
                ttft_warn_seconds: 30,
                max_output_tokens: 1200,
                server_host: "127.0.0.1".to_owned(),
                server_port: 8787,
                app_db_file: app_db_file.to_string_lossy().into_owned(),
                rss_enabled: true,
                rss_poll_interval_seconds: 300,
                rss_http_timeout_seconds: 15,
                rss_max_body_bytes: 2 * 1024 * 1024,
                rss_max_push_per_feed: 3,
                rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS,
                rss_seen_retention: 500,
                rss_push_max_failures: 3,
                rss_push_url: "http://127.0.0.1:8788/internal/push".to_owned(),
                rss_push_token: None,
                rss_push_message_type: "markdown".to_owned(),
                rss_allow_private_urls: true,
                prompt_dir: prompt_dir.to_string_lossy().into_owned(),
                prompt_dir_uses_builtin_defaults: false,
                world_file: None,
                member_id_mapping_file: member_id_mapping_file.to_string_lossy().into_owned(),
                qweather_api_key: "test-qweather-key".to_owned(),
                qweather_api_host: "https://api.qweather.com".to_owned(),
                qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
            },
            provider: Arc::new(MockProvider),
            query_executor: Arc::new(MockQueryExecutor),
            weather_executor: Arc::new(MockWeatherExecutor),
            memory_store: MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: TodoStore::new(database.clone()),
            rss_store: RssStore::new(database),
            rss_fetcher: RssFetcher::new(RssFetchConfig {
                allow_private_networks: true,
                ..RssFetchConfig::default()
            })
            .unwrap(),
            prompt_config: PromptConfig::new(prompt_dir, member_id_mapping_file),
        }
    }

    async fn request_raw_response(
        state: AppState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
        accept: Option<&str>,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let app = build_router(state);
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(accept) = accept {
            builder = builder.header("accept", accept);
        }
        let body = value
            .map(|value| Body::from(value.to_string()))
            .unwrap_or_else(Body::empty);
        let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    async fn request_response(
        state: AppState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let (status, _headers, body) = request_raw_response(state, method, path, value, None).await;
        let json = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        (status, json)
    }

    async fn request_text_response(
        state: AppState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
        accept: Option<&str>,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap, String) {
        let (status, headers, body) =
            request_raw_response(state, method, path, value, accept).await;
        let text = String::from_utf8_lossy(&body).into_owned();
        (status, headers, text)
    }

    async fn request_json(
        state: AppState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let (status, json) = request_response(state, method, path, value).await;
        assert!(status.is_success(), "unexpected status {status}: {json}");
        json
    }

    async fn post_json(path: &str, value: serde_json::Value) -> serde_json::Value {
        request_json(test_state(), "POST", path, Some(value)).await
    }

    fn standard_qq_payload(content: &str) -> serde_json::Value {
        json!({
            "scope_key": "group:g1",
            "content": content,
            "platform": "qq_official",
            "event_type": "FakeEvent",
            "user_id": "u1",
            "group_id": "g1",
            "message_id": "m1",
            "timestamp": "2026-06-10T10:00:00+08:00"
        })
    }

    #[tokio::test]
    async fn healthz_returns_ok() -> Result<(), Infallible> {
        let (status, json) = request_response(test_state(), "GET", "/healthz", None).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(json["ok"], true);
        assert_eq!(json["provider"], "mock");
        assert_eq!(json["model"], "mock-model");
        Ok(())
    }

    #[tokio::test]
    async fn respond_accepts_standard_qq_payload() -> Result<(), Infallible> {
        let json = post_json("/v1/respond", standard_qq_payload("普通聊天")).await;

        assert_eq!(json["ok"], true);
        assert_eq!(json["text"], "标题\n· hello");
        assert!(json.get("reply").is_none());
        assert!(json.get("raw_reply").is_none());
        assert!(json.get("deltas").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn respond_keeps_structured_markdown() -> Result<(), Infallible> {
        let json = post_json("/v1/respond", standard_qq_payload("给 codex")).await;

        assert_eq!(json["text"], "# 标题\n- hello");
        assert!(json.get("reply").is_none());
        assert!(json.get("raw_reply").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn respond_streams_when_accepts_event_stream_and_enabled() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.send_mode = "streaming".to_owned();
        let (status, headers, body) = request_text_response(
            state,
            "POST",
            "/v1/respond",
            Some(standard_qq_payload("/查 Cloudflare D1")),
            Some("text/event-stream"),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::OK);
        let content_type = headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));
        assert!(body.contains("event: delta"));
        assert!(body.contains("event: final"));
        Ok(())
    }

    #[tokio::test]
    async fn respond_rejects_legacy_payload_fields() -> Result<(), Infallible> {
        for (field, value) in [
            ("session_id", json!("group:g1")),
            ("user_text", json!("旧文本")),
            ("system_prompts", json!([])),
            ("history_messages", json!([])),
            ("purpose", json!("chat")),
        ] {
            let mut payload = standard_qq_payload("普通聊天");
            payload
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), value);

            let (status, _json) =
                request_response(test_state(), "POST", "/v1/respond", Some(payload)).await;

            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{field}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn legacy_http_routes_are_not_registered() -> Result<(), Infallible> {
        for (method, path, body) in [
            ("POST", "/query", Some(json!({"query": "Cloudflare D1"}))),
            ("GET", "/memory", None),
            ("POST", "/memory", Some(json!({"content": "记忆"}))),
            ("GET", "/memory/abcdef12", None),
            (
                "PATCH",
                "/memory/abcdef12",
                Some(json!({"content": "更新"})),
            ),
            ("DELETE", "/memory/abcdef12", None),
            (
                "POST",
                "/v1/chat",
                Some(json!({
                    "session_id": "group:g1",
                    "messages": [{"role": "user", "content": "hi"}]
                })),
            ),
        ] {
            let (status, _json) = request_response(test_state(), method, path, body).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{method} {path}");
        }
        Ok(())
    }
}
