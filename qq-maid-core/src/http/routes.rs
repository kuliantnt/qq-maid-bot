//! HTTP 路由和请求处理器。
//!
//! 定义进程级 `/healthz`、控制台和 Markdown 预览接口。
//!
//! Gateway 与 Core 之间的业务调用已经改为进程内 `CoreService`，这里不再公开
//! 内部 respond 或 SSE 传入口，避免同进程组件保留长期双轨。

use qq_maid_llm::provider::{DynLlmProvider, status::UpstreamStatus};
#[cfg(test)]
use serde_json::json;
use std::{sync::Arc, time::Instant};

use crate::{
    config::{AppConfig, center::ConfigCenter},
    http::console::{
        ConsoleCoreSummary, ConsoleRestartController, ConsoleStatusSource, ConsoleToolMetadata,
        DynConsoleStatusSource, EmptyConsoleStatusSource,
    },
    management::AdminAuth,
};

pub use super::router_builder::build_router;
/// 运维 HTTP 接口需要的最小配置。
#[derive(Clone)]
pub struct OpsHttpConfig {
    pub web_console_enabled: bool,
    pub web_console_allowed_origins: Vec<String>,
    pub web_console_trusted_proxy_ips: Vec<std::net::IpAddr>,
    pub web_console_secure_cookies: bool,
}

impl From<&AppConfig> for OpsHttpConfig {
    fn from(value: &AppConfig) -> Self {
        Self {
            web_console_enabled: value.web_console_enabled,
            web_console_allowed_origins: value.web_console_allowed_origins.clone(),
            web_console_trusted_proxy_ips: value.web_console_trusted_proxy_ips.clone(),
            web_console_secure_cookies: value.web_console_secure_cookies,
        }
    }
}

/// 运维 HTTP 全局状态，通过 Axum 的 State 注入到各处理器中。
#[derive(Clone)]
pub struct OpsHttpState {
    pub config: OpsHttpConfig,
    /// LLM 提供商（可为主备模式）。
    pub provider: Option<DynLlmProvider>,
    /// 最近一次真实上游调用的脱敏状态。
    pub upstream_status: UpstreamStatus,
    /// Core 自身的安全配置与启动时刻摘要。
    pub core_summary: ConsoleCoreSummary,
    /// Gateway 等接入层提供的只读运行态；不得在 snapshot 中执行外部探测。
    pub console_status_source: DynConsoleStatusSource,
    /// 配置中心领域能力；HTTP 读写都必须先通过部署管理员认证。
    pub config_center: Option<ConfigCenter>,
    /// 配置 WebUI 与后续 Memory WebUI 统一复用的部署管理员安全边界。
    pub admin_auth: Option<AdminAuth>,
    /// 当前进程真实注册的 Tool 元数据，供 WebUI 动态展示白名单选项。
    pub registered_tools: Arc<Vec<ConsoleToolMetadata>>,
    /// 仅复用部署目录中的受控 botctl 脚本，不直接操作 systemd 或 Docker。
    pub restart_controller: ConsoleRestartController,
    /// 缺少 Provider 或平台入口时仍开放管理恢复入口，但不能伪报机器人已经就绪。
    pub setup_required: bool,
}

impl OpsHttpState {
    pub fn with_registered_tools(mut self, tools: Vec<ConsoleToolMetadata>) -> Self {
        self.registered_tools = Arc::new(tools);
        self
    }

    pub fn from_parts(
        config: OpsHttpConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
    ) -> Self {
        Self {
            config,
            provider: Some(provider),
            upstream_status,
            core_summary: ConsoleCoreSummary {
                application_version: "test-version".to_owned(),
                started_at: "unix:0".to_owned(),
                started_instant: Instant::now(),
                listen_summary: "127.0.0.1:8787".to_owned(),
                database_path: "data/storage/app.db".to_owned(),
                provider_configured: true,
                rss_enabled: true,
                tool_calling_enabled: true,
            },
            console_status_source: Arc::new(EmptyConsoleStatusSource),
            config_center: None,
            admin_auth: None,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::default(),
            setup_required: false,
        }
    }

    pub fn from_config(
        config: &AppConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
        console_status_source: Arc<dyn ConsoleStatusSource>,
        application_version: &str,
    ) -> Self {
        Self::from_config_with_center(
            config,
            provider,
            upstream_status,
            console_status_source,
            application_version,
            None,
            None,
        )
    }

    pub fn from_config_with_center(
        config: &AppConfig,
        provider: DynLlmProvider,
        upstream_status: UpstreamStatus,
        console_status_source: Arc<dyn ConsoleStatusSource>,
        application_version: &str,
        config_center: Option<ConfigCenter>,
        admin_auth: Option<AdminAuth>,
    ) -> Self {
        Self {
            config: config.into(),
            provider: Some(provider),
            upstream_status,
            core_summary: ConsoleCoreSummary::from_config(config, application_version),
            console_status_source,
            config_center,
            admin_auth,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::from_current_dir(),
            setup_required: false,
        }
    }

    pub fn setup_required(
        config: OpsHttpConfig,
        core_summary: ConsoleCoreSummary,
        config_center: ConfigCenter,
        admin_auth: Option<AdminAuth>,
    ) -> Self {
        Self {
            config,
            provider: None,
            upstream_status: UpstreamStatus::default(),
            core_summary,
            console_status_source: Arc::new(EmptyConsoleStatusSource),
            config_center: Some(config_center),
            admin_auth,
            registered_tools: Arc::new(Vec::new()),
            restart_controller: ConsoleRestartController::from_current_dir(),
            setup_required: true,
        }
    }
}

// 控制台静态资源、状态和安全响应头由 console_routes 模块负责。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{AgentRuntimeConfig, center::ConfigCenterPaths, managed_config_fields},
        error::LlmError,
        management::AdminAuth,
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
        util::metrics::LlmMetrics,
    };
    use async_trait::async_trait;
    use axum::{body::Body, http::StatusCode};
    use http_body_util::BodyExt;
    use qq_maid_llm::provider::{
        ChatOutcome, LlmProvider,
        status::{UpstreamStatus, observe_provider},
        types::{ChatRequest, TokenUsage},
    };
    use std::{
        collections::HashMap,
        convert::Infallible,
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

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            Ok(ChatOutcome {
                reply: "# 标题\n- hello".to_owned(),
                output_parts: Vec::new(),
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
                agent: Default::default(),
            })
        }

        fn name(&self) -> &str {
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

        fn name(&self) -> &str {
            "counting-mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            false
        }
    }

    fn test_state() -> OpsHttpState {
        let upstream_status = UpstreamStatus::default();
        let provider = observe_provider(Arc::new(MockProvider), upstream_status.clone());
        OpsHttpState::from_parts(
            OpsHttpConfig {
                web_console_enabled: false,
                web_console_allowed_origins: Vec::new(),
                web_console_trusted_proxy_ips: Vec::new(),
                web_console_secure_cookies: false,
            },
            provider,
            upstream_status,
        )
    }

    async fn request_raw_response(
        state: OpsHttpState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
        accept: Option<&str>,
    ) -> (axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>) {
        request_raw_response_with_origin(state, method, path, value, accept, None).await
    }

    async fn request_raw_response_with_origin(
        state: OpsHttpState,
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
            .header("content-type", "application/json")
            .header("host", "localhost");
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
        state: OpsHttpState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let (status, _headers, body) = request_raw_response(state, method, path, value, None).await;
        let json = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        (status, json)
    }

    async fn request_response_with_cookie(
        state: OpsHttpState,
        method: &str,
        path: &str,
        value: Option<serde_json::Value>,
        cookie: &str,
        csrf: Option<&str>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let app = build_router(state);
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header(
                "cookie",
                format!("{}={cookie}", crate::management::SESSION_COOKIE_NAME),
            );
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        let body = value
            .map(|value| Body::from(value.to_string()))
            .unwrap_or_else(Body::empty);
        let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        (status, json)
    }

    fn initialize_test_admin(
        database: SqliteDatabase,
        directory: &std::path::Path,
    ) -> (AdminAuth, String, String) {
        let token_file = directory.join("config/secrets/bootstrap.token");
        let auth = AdminAuth::open(database, token_file.clone()).unwrap();
        let token = std::fs::read_to_string(token_file)
            .unwrap()
            .trim()
            .splitn(3, ':')
            .nth(2)
            .unwrap()
            .to_owned();
        let preauth = auth.issue_preauth().unwrap();
        let issued = auth
            .initialize(
                &preauth.cookie_value,
                &preauth.session.csrf_token,
                &token,
                "admin",
                "correct horse battery staple",
            )
            .unwrap();
        (auth, issued.cookie_value, issued.session.csrf_token)
    }

    async fn request_text_response(
        state: OpsHttpState,
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
        state.provider = Some(observe_provider(
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
            upstream_status.clone(),
        ));
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
        let (css_status, _) =
            request_response(test_state(), "GET", "/console/styles.css", None).await;
        let (js_status, _) = request_response(test_state(), "GET", "/console/app.js", None).await;
        let (agent_tools_status, _) =
            request_response(test_state(), "GET", "/console/agent-tools.js", None).await;
        let (status_api, _) =
            request_response(test_state(), "GET", "/api/v1/console/status", None).await;

        assert_eq!(console_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(render_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(css_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(js_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(agent_tools_status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(status_api, axum::http::StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn console_routes_work_when_enabled_without_wildcard_cors() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let (status, headers, body) =
            request_text_response(state, "GET", "/console/", None, None).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("部署管理控制台"));
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
        assert!(csp.contains("style-src 'self'"));
        assert!(csp.contains("script-src 'self'"));
        assert!(!csp.contains("unsafe-inline"));

        let mut state = test_state();
        state.config.web_console_enabled = true;
        for (path, expected_content_type) in [
            ("/console/styles.css", "text/css; charset=utf-8"),
            ("/console/app.js", "text/javascript; charset=utf-8"),
            ("/console/agent-tools.js", "text/javascript; charset=utf-8"),
        ] {
            let (status, headers, body) =
                request_text_response(state.clone(), "GET", path, None, None).await;
            assert_eq!(status, axum::http::StatusCode::OK);
            assert!(!body.is_empty());
            assert_eq!(
                headers
                    .get(axum::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some(expected_content_type)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_status_is_read_only_and_preauth_uses_a_separate_cookie() {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let (database, directory) = SqliteDatabase::open_temp_directory(
            "qq-maid-bootstrap-cookie-separation",
            APP_MIGRATIONS,
        )
        .unwrap();
        state.admin_auth = Some(
            AdminAuth::open(database, directory.join("config/secrets/bootstrap.token")).unwrap(),
        );

        let (status, headers, body) = request_raw_response_with_origin(
            state.clone(),
            "GET",
            "/api/v1/console/auth/bootstrap",
            None,
            Some("application/json"),
            Some("http://localhost"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get(axum::http::header::SET_COOKIE).is_none());
        let status_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(status_json.get("csrf_token").is_none());

        let (status, headers, _body) = request_raw_response_with_origin(
            state,
            "POST",
            "/api/v1/console/auth/preauth",
            None,
            Some("application/json"),
            Some("http://localhost"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let set_cookie = headers
            .get(axum::http::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(set_cookie.starts_with("qq_maid_console_preauth="));
        assert!(!set_cookie.contains("qq_maid_console_session="));
    }

    #[tokio::test]
    async fn secure_console_cookies_use_secure_host_prefix() {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        state.config.web_console_secure_cookies = true;
        let (database, directory) =
            SqliteDatabase::open_temp_directory("qq-maid-secure-console-cookie", APP_MIGRATIONS)
                .unwrap();
        state.admin_auth = Some(
            AdminAuth::open(database, directory.join("config/secrets/bootstrap.token")).unwrap(),
        );
        let (_status, headers, _body) = request_raw_response_with_origin(
            state,
            "POST",
            "/api/v1/console/auth/preauth",
            None,
            Some("application/json"),
            Some("http://localhost"),
        )
        .await;
        let set_cookie = headers
            .get(axum::http::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert!(set_cookie.starts_with("__Host-qq_maid_console_preauth="));
        assert!(set_cookie.contains("; Secure"));
        assert!(set_cookie.contains("; Path=/"));
    }

    #[tokio::test]
    async fn console_status_is_read_only_valid_and_secret_free() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let calls = Arc::new(AtomicUsize::new(0));
        let upstream_status = UpstreamStatus::default();
        state.provider = Some(observe_provider(
            Arc::new(CountingProvider {
                calls: calls.clone(),
            }),
            upstream_status.clone(),
        ));
        state.upstream_status = upstream_status;

        let (status, json) = request_response(state, "GET", "/api/v1/console/status", None).await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(json["runtime"]["ok"], true);
        assert_eq!(json["provider"]["upstream"]["state"], "unverified");
        assert_eq!(json["storage"][1]["state"], "not_available");
        let serialized = json.to_string().to_ascii_lowercase();
        for forbidden in ["token", "secret", "api_key", "cookie", "authorization"] {
            assert!(
                !serialized.contains(forbidden),
                "unexpected {forbidden} field"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn configuration_snapshot_never_returns_secret_plaintext() -> Result<(), Infallible> {
        let mut state = test_state();
        state.config.web_console_enabled = true;
        let (database, directory) =
            SqliteDatabase::open_temp_directory("qq-maid-config-http", APP_MIGRATIONS).unwrap();
        let external = HashMap::from([
            (
                "OPENAI_API_KEY".to_owned(),
                "must-never-reach-response".to_owned(),
            ),
            (
                "TAVILY_API_KEY".to_owned(),
                "tavily-must-never-reach-response".to_owned(),
            ),
        ]);
        let agent_path = directory.join("config/agent.toml");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        std::fs::write(
            &agent_path,
            include_str!("../../../runtime/config/agent.toml"),
        )
        .unwrap();
        let agent_environment = HashMap::from([(
            crate::config::agent::AGENT_CONFIG_FILE_ENV.to_owned(),
            agent_path.to_string_lossy().into_owned(),
        )]);
        let running_agent = AgentRuntimeConfig::load_from_environment(&agent_environment).unwrap();
        state.registered_tools = Arc::new(vec![ConsoleToolMetadata {
            name: "web_search".to_owned(),
            description: "受控搜索".to_owned(),
        }]);
        state.config_center = Some(
            ConfigCenter::open(
                managed_config_fields(),
                ConfigCenterPaths {
                    managed_config_file: directory.join("config/runtime.toml"),
                    master_key_file: directory.join("config/secrets/master.key"),
                },
                database.clone(),
            )
            .unwrap()
            .with_external_environment(external)
            .with_running_agent_config(running_agent)
            .unwrap(),
        );
        let (auth, cookie, csrf) = initialize_test_admin(database, &directory);
        state.admin_auth = Some(auth);
        let (unauthenticated_status, _) =
            request_response(state.clone(), "GET", "/api/v1/console/configuration", None).await;
        assert_eq!(unauthenticated_status, StatusCode::UNAUTHORIZED);

        let (status, json) = request_response_with_cookie(
            state.clone(),
            "GET",
            "/api/v1/console/configuration",
            None,
            &cookie,
            None,
        )
        .await;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(json["ok"], true);
        let serialized = json.to_string();
        assert!(!serialized.contains("must-never-reach-response"));
        assert!(!serialized.contains("tavily-must-never-reach-response"));
        let secret = json["configuration"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["key"] == "provider.openai.api_key")
            .unwrap();
        assert_eq!(secret["configured"], true);
        assert_eq!(secret["source"], "environment");
        assert!(secret["effective_value"].is_null());
        let tavily_secret = json["configuration"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["key"] == "tools.web_search.tavily.api_key")
            .unwrap();
        assert_eq!(tavily_secret["configured"], true);
        assert!(tavily_secret["effective_value"].is_null());
        assert_eq!(json["configuration"]["agent"]["source"], "agent_toml");
        assert_eq!(json["configuration"]["agent"]["pending_restart"], false);
        assert_eq!(json["registered_tools"][0]["name"], "web_search");
        assert_eq!(json["registered_tools"][0]["description"], "受控搜索");
        assert_eq!(json["restart"]["available"], false);

        let agent_revision = json["configuration"]["agent"]["revision"].as_str().unwrap();
        let agent_mutation = json!({
            "expected_revision": agent_revision,
            "changes": [{
                "action": "set_web_search",
                "backend": "disabled",
                "max_results": 8,
                "search_depth": "advanced",
                "topic": "news",
                "time_range": "week",
                "connect_timeout_seconds": 5,
                "first_response_timeout_seconds": 15,
                "total_timeout_seconds": 45
            }]
        });
        let (agent_saved, agent_saved_json) = request_response_with_cookie(
            state.clone(),
            "PATCH",
            "/api/v1/console/configuration/agent",
            Some(agent_mutation),
            &cookie,
            Some(&csrf),
        )
        .await;
        assert_eq!(agent_saved, StatusCode::OK);
        assert_eq!(agent_saved_json["persisted"], true);
        assert_eq!(
            agent_saved_json["configuration"]["agent"]["saved_value"]["tools"]["web_search"]["backend"],
            "disabled"
        );
        assert!(
            agent_saved_json["configuration"]["agent"]["saved_value"]["tools"]
                ["web_search"]["routes"]["private_search"]
                .is_object()
        );
        assert!(
            !agent_saved_json
                .to_string()
                .contains("tavily-must-never-reach-response")
        );

        let revision = json["configuration"]["revision"].as_str().unwrap();
        let mutation = json!({
            "expected_revision": revision,
            "changes": [{
                "action": "set",
                "key": "features.rss.enabled",
                "value": false
            }]
        });
        let (missing_csrf, _) = request_response_with_cookie(
            state.clone(),
            "PATCH",
            "/api/v1/console/configuration/runtime",
            Some(mutation.clone()),
            &cookie,
            None,
        )
        .await;
        assert_eq!(missing_csrf, StatusCode::FORBIDDEN);
        let (restart_missing_csrf, _) = request_response_with_cookie(
            state.clone(),
            "POST",
            "/api/v1/console/restart",
            Some(json!({})),
            &cookie,
            None,
        )
        .await;
        assert_eq!(restart_missing_csrf, StatusCode::FORBIDDEN);
        let (restart_unavailable, restart_json) = request_response_with_cookie(
            state.clone(),
            "POST",
            "/api/v1/console/restart",
            Some(json!({})),
            &cookie,
            Some(&csrf),
        )
        .await;
        assert_eq!(restart_unavailable, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(restart_json["error"]["code"], "restart_unavailable");
        let (saved, saved_json) = request_response_with_cookie(
            state.clone(),
            "PATCH",
            "/api/v1/console/configuration/runtime",
            Some(mutation),
            &cookie,
            Some(&csrf),
        )
        .await;
        assert_eq!(saved, StatusCode::OK);
        assert_eq!(saved_json["persisted"], true);

        // 两个标签页顺序刷新会话后都保留同一稳定 CSRF，可继续完成写请求。
        let tab_two_csrf = state
            .admin_auth
            .as_ref()
            .unwrap()
            .refresh_admin_session(&cookie)
            .unwrap()
            .csrf_token;
        let second_revision = saved_json["configuration"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let second_mutation = json!({
            "expected_revision": second_revision,
            "changes": [{
                "action": "set",
                "key": "features.rss.enabled",
                "value": true
            }]
        });
        let (saved_again, saved_again_json) = request_response_with_cookie(
            state,
            "PATCH",
            "/api/v1/console/configuration/runtime",
            Some(second_mutation),
            &cookie,
            Some(&tab_two_csrf),
        )
        .await;
        assert_eq!(saved_again, StatusCode::OK);
        assert_eq!(saved_again_json["persisted"], true);
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
