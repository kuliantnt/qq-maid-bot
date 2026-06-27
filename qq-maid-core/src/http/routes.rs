//! HTTP 路由和请求处理器。
//!
//! 定义进程级 `/healthz`、控制台和 Markdown 预览接口。
//!
//! Gateway 与 Core 之间的业务调用已经改为进程内 `CoreService`，这里不再公开
//! 内部 respond 或 SSE 传入口，避免同进程组件保留长期双轨。

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use pulldown_cmark::{Options, Parser, html};
use serde::Deserialize;
use serde_json::json;

use crate::{
    config::AppConfig,
    provider::{DynLlmProvider, status::UpstreamStatus},
    runtime::{
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        prompt::PromptConfig,
        query::DynQueryExecutor,
        rss::{RssFetcher, RssStore},
        session::SessionStore,
        todo::TodoStore,
        train::DynTrainExecutor,
        weather::DynWeatherExecutor,
    },
};

/// 应用全局状态，通过 Axum 的 State 注入到各处理器中。
#[derive(Clone)]
pub struct AppState {
    /// 全局应用配置。
    pub config: AppConfig,
    /// LLM 提供商（可为主备模式）。
    pub provider: DynLlmProvider,
    /// 最近一次真实上游调用的脱敏状态。
    pub upstream_status: UpstreamStatus,
    /// 联网搜索执行器。
    pub query_executor: DynQueryExecutor,
    /// 天气查询执行器。
    pub weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器。
    pub train_executor: DynTrainExecutor,
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
    /// 本地 Markdown 知识检索索引。
    pub knowledge_index: KnowledgeIndex,
    /// 提示词配置（system prompt 模板等）。
    pub prompt_config: PromptConfig,
}

/// 构建 Axum 路由树，注册所有 HTTP 端点。
pub fn build_router(state: AppState) -> Router {
    let console_enabled = state.config.web_console_enabled;
    let router = Router::new().route("/healthz", get(healthz));
    let router = if console_enabled {
        router.route("/console/", get(console_index)).route(
            "/api/v1/markdown/render",
            post(markdown_render).options(markdown_render_preflight),
        )
    } else {
        router
    };
    router.with_state(state)
}

/// 健康检查端点，返回当前提供商和模型信息。
async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "provider": state.provider.name(),
        "model": state.provider.model(),
        "stream": state.provider.stream_enabled(),
        "upstream": state.upstream_status.snapshot(),
    }))
}

async fn console_index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let mut response = with_console_cors(
        Html(include_str!("../../../runtime/static/index.html")).into_response(),
        &state,
        &headers,
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; style-src 'unsafe-inline'; script-src 'self' 'unsafe-inline'; img-src 'self' data:;",
        ),
    );
    response
}

#[derive(Debug, Deserialize)]
struct MarkdownRenderRequest {
    markdown: String,
}

async fn markdown_render(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > 64 * 1024 {
        return with_console_cors(
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"ok": false, "error": "markdown payload too large"})),
            )
                .into_response(),
            &state,
            &headers,
        );
    }

    let payload = match serde_json::from_slice::<MarkdownRenderRequest>(&body) {
        Ok(payload) => payload,
        Err(_) => {
            return with_console_cors(
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": "invalid markdown render payload"})),
                )
                    .into_response(),
                &state,
                &headers,
            );
        }
    };
    let html = render_markdown_html(&payload.markdown);
    with_console_cors(
        Json(json!({"ok": true, "html": html})).into_response(),
        &state,
        &headers,
    )
}

async fn markdown_render_preflight(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // 跨站 `application/json` 请求会先发 OPTIONS 预检；这里必须显式返回允许的方法
    // 和请求头，否则 allowlist origin 仍会被浏览器拦下。
    with_console_preflight_cors(StatusCode::NO_CONTENT.into_response(), &state, &headers)
}

fn render_markdown_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(markdown, options);
    let mut html = String::new();
    html::push_html(&mut html, parser);
    let mut cleaner = ammonia::Builder::default();
    cleaner.add_tags(["input"]);
    cleaner.add_tag_attributes("input", ["type", "checked", "disabled"]);
    cleaner.clean(&html).to_string()
}

fn with_console_security(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

fn with_console_cors(mut response: Response, state: &AppState, headers: &HeaderMap) -> Response {
    let Some(origin) = allowed_console_origin(state, headers) else {
        return with_console_security(response);
    };
    let Ok(value) = HeaderValue::from_str(origin) else {
        return with_console_security(response);
    };
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("origin"));
    with_console_security(response)
}

fn with_console_preflight_cors(
    mut response: Response,
    state: &AppState,
    headers: &HeaderMap,
) -> Response {
    let Some(origin) = allowed_console_origin(state, headers) else {
        return with_console_security(response);
    };
    let Ok(value) = HeaderValue::from_str(origin) else {
        return with_console_security(response);
    };
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static(
            "origin, access-control-request-method, access-control-request-headers",
        ),
    );
    with_console_security(response)
}

fn allowed_console_origin<'a>(state: &'a AppState, headers: &'a HeaderMap) -> Option<&'a str> {
    let origin = headers.get(header::ORIGIN)?.to_str().ok()?;
    state
        .config
        .web_console_allowed_origins
        .iter()
        .map(String::as_str)
        .find(|allowed| *allowed == origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_RSS_SUMMARY_MAX_CHARS, ProviderMode},
        error::LlmError,
        provider::{
            ChatOutcome, LlmProvider,
            status::{UpstreamStatus, observe_provider},
            types::{ChatRequest, TokenUsage},
        },
        runtime::{
            prompt::PromptConfig,
            query::{QueryExecutor, QueryOutcome, QueryRequest, QuerySource},
            rss::RssFetchConfig,
            session::SessionStore,
            train::{TrainExecutor, TrainSchedule, TrainScheduleRequest, TrainStop},
            weather::{
                CurrentWeather, DailyWeather, WeatherExecutor, WeatherLocation, WeatherOutcome,
                WeatherRequest, WeatherSupplement,
            },
        },
        storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
        util::metrics::LlmMetrics,
    };
    use async_trait::async_trait;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use std::{
        convert::Infallible,
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tower::ServiceExt;

    #[derive(Clone)]
    struct MockProvider;

    #[derive(Clone)]
    struct CountingProvider {
        calls: Arc<AtomicUsize>,
    }

    struct MockQueryExecutor;

    struct MockWeatherExecutor;

    struct MockTrainExecutor;

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
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
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
    impl LlmProvider for CountingProvider {
        async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            MockProvider.chat(req).await
        }

        fn name(&self) -> &'static str {
            "counting-mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            false
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

    #[async_trait]
    impl TrainExecutor for MockTrainExecutor {
        async fn query_train_schedule(
            &self,
            req: TrainScheduleRequest,
        ) -> Result<TrainSchedule, LlmError> {
            Ok(TrainSchedule {
                train_code: req.train_code,
                travel_date: req.travel_date,
                start_station: "北京南".to_owned(),
                end_station: "上海虹桥".to_owned(),
                stops: vec![TrainStop {
                    station_no: 1,
                    station_name: "北京南".to_owned(),
                    arrive_time: None,
                    departure_time: Some("06:30".to_owned()),
                    stopover_minutes: None,
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: "G1".to_owned(),
                }],
                full_train_code: None,
                corporation: None,
                train_style: None,
                dept_train: None,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-train"
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
        let knowledge_dir = std::env::temp_dir().join(format!(
            "qq-maid-route-knowledge-test-{}",
            uuid::Uuid::new_v4()
        ));
        let knowledge_index =
            KnowledgeIndex::new(KnowledgeStore::new(database.clone()), &knowledge_dir);
        knowledge_index.sync().unwrap();

        let upstream_status = UpstreamStatus::default();
        let provider = observe_provider(Arc::new(MockProvider), upstream_status.clone());
        AppState {
            config: AppConfig {
                provider: ProviderMode::OpenAi,
                model: "mock-model".to_owned(),
                model_route: crate::provider::types::ModelRoute::parse_config(
                    "mock-model",
                    "LLM_MODEL",
                )
                .unwrap(),
                title_model: None,
                todo_model: None,
                memory_model: None,
                compact_model: None,
                translation_model: None,
                openai_search_model: "mock-search-model".to_owned(),
                openai_api_key: Some("test".to_owned()),
                openai_base_url: None,
                openai_api_mode: crate::config::OpenAiApiMode::Auto,
                deepseek_api_key: None,
                deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
                deepseek_model: "deepseek-chat".to_owned(),
                bigmodel_api_key: None,
                bigmodel_base_url: crate::config::DEFAULT_BIGMODEL_BASE_URL.to_owned(),
                bigmodel_model: "glm-5.2".to_owned(),
                stream: true,
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
                rss_push_message_type: "markdown".to_owned(),
                todo_daily_reminder_enabled: false,
                todo_daily_reminder_time: crate::config::DailyReminderTime { hour: 9, minute: 0 },
                rss_allow_private_urls: true,
                prompt_dir: prompt_dir.to_string_lossy().into_owned(),
                prompt_dir_uses_builtin_defaults: false,
                knowledge_dir: knowledge_dir.to_string_lossy().into_owned(),
                member_id_mapping_file: member_id_mapping_file.to_string_lossy().into_owned(),
                qweather_api_key: "test-qweather-key".to_owned(),
                qweather_api_host: "https://api.qweather.com".to_owned(),
                qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
                web_console_enabled: false,
                web_console_allowed_origins: Vec::new(),
            },
            provider,
            upstream_status,
            query_executor: Arc::new(MockQueryExecutor),
            weather_executor: Arc::new(MockWeatherExecutor),
            train_executor: Arc::new(MockTrainExecutor),
            memory_store: MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: TodoStore::new(database.clone()),
            rss_store: RssStore::new(database),
            rss_fetcher: RssFetcher::new(RssFetchConfig {
                allow_private_networks: true,
                ..RssFetchConfig::default()
            })
            .unwrap(),
            knowledge_index,
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
        request_raw_response_with_origin(state, method, path, value, accept, None).await
    }

    async fn request_raw_response_with_origin(
        state: AppState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
        accept: Option<&str>,
        origin: Option<&str>,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let app = build_router(state);
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(accept) = accept {
            builder = builder.header("accept", accept);
        }
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
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
        assert_eq!(json["upstream"]["state"], "unverified");
        Ok(())
    }

    #[tokio::test]
    async fn healthz_only_reads_status_without_calling_provider() -> Result<(), Infallible> {
        let mut state = test_state();
        let calls = Arc::new(AtomicUsize::new(0));
        let upstream_status = UpstreamStatus::default();
        state.provider = observe_provider(
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
            upstream_status.clone(),
        );
        state.upstream_status = upstream_status;

        let (_status, json) = request_response(state, "GET", "/healthz", None).await;

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(json["upstream"]["state"], "unverified");
        Ok(())
    }

    #[tokio::test]
    async fn console_routes_are_not_registered_by_default() -> Result<(), Infallible> {
        let (console_status, _json) =
            request_response(test_state(), "GET", "/console/", None).await;
        let (render_status, _json) = request_response(
            test_state(),
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown":"# hi"})),
        )
        .await;

        assert_eq!(console_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(render_status, axum::http::StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn console_routes_work_when_enabled_without_wildcard_cors() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let (status, headers, body) =
            request_text_response(state, "GET", "/console/", None, None).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("QQ Maid Bot"));
        assert!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        assert_eq!(
            headers
                .get(axum::http::header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            headers
                .get(axum::http::header::X_FRAME_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("DENY")
        );
        let csp = headers
            .get(axum::http::header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("style-src 'unsafe-inline'"));
        Ok(())
    }

    #[tokio::test]
    async fn markdown_render_endpoint_has_security_headers() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let (_status, headers, _body) = request_raw_response_with_origin(
            state,
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown":"# hi"})),
            Some("application/json"),
            None,
        )
        .await;

        assert_eq!(
            headers
                .get(axum::http::header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            headers
                .get(axum::http::header::X_FRAME_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("DENY")
        );
        Ok(())
    }

    #[tokio::test]
    async fn markdown_render_sanitizes_html_and_keeps_tables_and_tasks() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let markdown = "# hi\n\n- [x] done\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n<script>alert(1)</script>\n[bad](javascript:alert(1))";
        let (_status, json) = request_response(
            state,
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown": markdown})),
        )
        .await;
        let html = json["html"].as_str().unwrap();

        assert_eq!(json["ok"], true);
        assert!(html.contains("<table>"));
        assert!(html.contains("checkbox"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("javascript:"));
        Ok(())
    }

    #[tokio::test]
    async fn markdown_render_rejects_oversized_body() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let markdown = "x".repeat(70 * 1024);
        let (status, _json) = request_response(
            state,
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown": markdown})),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[tokio::test]
    async fn console_cors_allows_only_configured_origins() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        state.config.web_console_allowed_origins = vec!["https://console.example".to_owned()];
        let (_status, headers, _body) = request_raw_response_with_origin(
            state.clone(),
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown":"# hi"})),
            None,
            Some("https://console.example"),
        )
        .await;
        assert_eq!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("https://console.example")
        );

        let (_status, headers, _body) = request_raw_response_with_origin(
            state,
            "POST",
            "/api/v1/markdown/render",
            Some(json!({"markdown":"# hi"})),
            None,
            Some("https://evil.example"),
        )
        .await;
        assert!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn console_cors_preflight_allows_json_post_for_configured_origin()
    -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        state.config.web_console_allowed_origins = vec!["https://console.example".to_owned()];
        let app = build_router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("OPTIONS")
                    .uri("/api/v1/markdown/render")
                    .header(axum::http::header::ORIGIN, "https://console.example")
                    .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(
                        axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "content-type",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let headers = response.headers();

        assert_eq!(response.status(), axum::http::StatusCode::NO_CONTENT);
        assert_eq!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("https://console.example")
        );
        assert_eq!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_METHODS)
                .and_then(|value| value.to_str().ok()),
            Some("POST, OPTIONS")
        );
        assert_eq!(
            headers
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS)
                .and_then(|value| value.to_str().ok()),
            Some("content-type")
        );
        assert_eq!(
            headers
                .get(axum::http::header::VARY)
                .and_then(|value| value.to_str().ok()),
            Some("origin, access-control-request-method, access-control-request-headers")
        );
        Ok(())
    }

    #[tokio::test]
    async fn respond_route_is_not_registered() -> Result<(), Infallible> {
        let respond_path = format!("/{}/respond", "v1");
        let (status, _json) = request_response(
            test_state(),
            "POST",
            &respond_path,
            Some(standard_qq_payload("普通聊天")),
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
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
