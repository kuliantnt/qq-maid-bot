use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use qq_maid_llm::provider::{
    ChatOutcome, LlmProvider,
    status::{UpstreamStatus, observe_provider},
    types::{ChatRequest, TokenUsage},
};
use tower::ServiceExt;

use crate::{
    error::LlmError,
    http::routes::{OpsHttpConfig, OpsHttpState, build_router},
    management::{AdminAuth, SESSION_COOKIE_NAME},
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
    util::metrics::LlmMetrics,
};

use super::{dto::ListFilesRequest, handlers::content_disposition};

#[derive(Clone)]
struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        Ok(ChatOutcome {
            reply: String::new(),
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
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
        "mock"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

fn test_state() -> (OpsHttpState, String, String) {
    let (database, directory) =
        SqliteDatabase::open_temp_directory("knowledge-file-api", APP_MIGRATIONS).unwrap();
    let token_file = directory.join("config/secrets/bootstrap.token");
    let auth = AdminAuth::open_silent(database, token_file.clone()).unwrap();
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
            "knowledge-admin",
            "correct horse battery staple",
        )
        .unwrap();
    let upstream = UpstreamStatus::default();
    let mut state = OpsHttpState::from_parts(
        OpsHttpConfig {
            web_console_enabled: true,
            web_console_allowed_origins: Vec::new(),
            web_console_trusted_proxy_ips: Vec::new(),
            web_console_secure_cookies: false,
            knowledge_max_file_bytes: crate::config::DEFAULT_KNOWLEDGE_MAX_FILE_BYTES,
        },
        observe_provider(Arc::new(MockProvider), upstream.clone()),
        upstream,
    );
    state.admin_auth = Some(auth);
    (state, issued.cookie_value, issued.session.csrf_token)
}

fn request(cookie: Option<&str>, csrf: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v1/console/knowledge/files/list")
        .header("host", "localhost")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("{SESSION_COOKIE_NAME}={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    builder.body(Body::from("{}")).unwrap()
}

#[test]
fn list_defaults_to_updated_descending_and_rejects_unknown_status() {
    let request: ListFilesRequest = serde_json::from_value(serde_json::json!({})).unwrap();
    let (_, query) = request.into_query().unwrap();
    assert!(query.descending);

    let request: ListFilesRequest =
        serde_json::from_value(serde_json::json!({"status": "unknown"})).unwrap();
    assert!(request.into_query().is_err());
}

#[test]
fn download_filename_uses_safe_ascii_fallback_and_utf8_parameter() {
    let disposition = content_disposition("知识库 指南.md");
    assert!(disposition.starts_with("attachment; filename=\""));
    assert!(disposition.contains("filename*=UTF-8''"));
    assert!(disposition.contains("%E7%9F%A5"));
}

#[tokio::test]
async fn knowledge_management_routes_require_session_and_csrf() {
    let (state, cookie, csrf) = test_state();

    let response = build_router(state.clone())
        .oneshot(request(None, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = build_router(state.clone())
        .oneshot(request(Some(&cookie), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // 认证和 CSRF 通过后才会进入领域装配检查；测试状态故意不注入 worker/service，
    // 用 503 区分“已通过安全边界”和“被认证层拒绝”。
    let response = build_router(state)
        .oneshot(request(Some(&cookie), Some(&csrf)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
