//! 部署管理员认证与配置写 API。
//!
//! HTTP handler 只负责认证、CSRF、参数解析和真实领域结果映射；配置校验、revision
//! 冲突、TOML 原子写入和 secret 加密继续由 `ConfigCenter` 负责。

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::{convert::Infallible, net::SocketAddr, time::Duration};

use crate::{
    config::{
        ChatScene,
        agent::{AgentProfileConfig, AgentSceneConfig},
        center::{AgentConfigChange, ConfigCenterError, ManagedConfigChange, SecretConfigChange},
    },
    management::{
        AdminAuthError, PREAUTH_COOKIE_NAME, SECURE_PREAUTH_COOKIE_NAME,
        SECURE_SESSION_COOKIE_NAME, SESSION_COOKIE_NAME,
    },
};

use super::routes::{OpsHttpState, with_console_cors};

const CSRF_HEADER: &str = "x-csrf-token";
const COOKIE_MAX_AGE_SECONDS: i64 = 12 * 60 * 60;
pub(super) type BoxedResponse = Box<Response>;

struct OptionalPeer(Option<SocketAddr>);

impl<S> FromRequestParts<S> for OptionalPeer
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(address)| *address),
        ))
    }
}

pub(super) fn management_router() -> Router<OpsHttpState> {
    Router::new()
        .route("/api/v1/console/auth/bootstrap", get(auth_bootstrap))
        .route("/api/v1/console/auth/preauth", post(auth_preauth))
        .route("/api/v1/console/auth/initialize", post(auth_initialize))
        .route("/api/v1/console/auth/login", post(auth_login))
        .route("/api/v1/console/auth/logout", post(auth_logout))
        .route("/api/v1/console/session", get(console_session))
        .route(
            "/api/v1/console/configuration/runtime",
            patch(update_runtime_configuration),
        )
        .route(
            "/api/v1/console/configuration/secrets",
            patch(update_secret_configuration),
        )
        .route(
            "/api/v1/console/configuration/agent",
            patch(update_agent_configuration),
        )
        .route(
            "/api/v1/console/configuration/validate",
            post(validate_configuration),
        )
        .route(
            "/api/v1/console/configuration/test-connection",
            post(test_provider_connection),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
}

async fn auth_bootstrap(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let source = client_source(&state, &headers, peer.0);
    if let Err(error) = auth.check_bootstrap_rate_limit(&source) {
        return respond(&state, &headers, auth_error(error));
    }
    let status = match auth.bootstrap_status() {
        Ok(value) => value,
        Err(error) => return respond(&state, &headers, auth_error(error)),
    };
    respond(
        &state,
        &headers,
        Json(json!({"ok": true, "bootstrap": status})).into_response(),
    )
}

async fn auth_preauth(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    if !preauth_request_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "pre-authentication requires a same-origin browser request",
            ),
        );
    }
    let source = client_source(&state, &headers, peer.0);
    let issued = match auth.issue_preauth_for(&source) {
        Ok(value) => value,
        Err(error) => return respond(&state, &headers, auth_error(error)),
    };
    let mut response = Json(json!({
        "ok": true,
        "csrf_token": issued.session.csrf_token,
    }))
    .into_response();
    set_preauth_cookie(
        &mut response,
        &issued.cookie_value,
        10 * 60,
        state.config.web_console_secure_cookies,
    );
    respond(&state, &headers, response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeRequest {
    username: String,
    password: String,
    bootstrap_token: String,
}

async fn auth_initialize(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
    Json(payload): Json<InitializeRequest>,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.clone() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    let result = tokio::task::spawn_blocking(move || {
        auth.initialize_for(
            &cookie,
            &csrf,
            &payload.bootstrap_token,
            &payload.username,
            &payload.password,
            &source,
        )
    })
    .await;
    let issued = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return respond(&state, &headers, auth_error(error)),
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_internal_error",
                    "administrator initialization task failed",
                ),
            );
        }
    };
    let mut response = Json(json!({"ok": true, "session": issued.session})).into_response();
    set_session_cookie(
        &mut response,
        &issued.cookie_value,
        COOKIE_MAX_AGE_SECONDS,
        state.config.web_console_secure_cookies,
    );
    clear_preauth_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    State(state): State<OpsHttpState>,
    peer: OptionalPeer,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Response {
    if !origin_allowed(&headers) {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "origin_denied",
                "request origin is not allowed",
            ),
        );
    }
    let Some(auth) = state.admin_auth.clone() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = preauth_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "pre-authentication session is missing",
            ),
        );
    };
    let Some(csrf) = csrf_token(&headers) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::FORBIDDEN,
                "csrf_failed",
                "CSRF token is missing",
            ),
        );
    };
    let source = client_source(&state, &headers, peer.0);
    let result = tokio::task::spawn_blocking(move || {
        auth.login_for(
            &cookie,
            &csrf,
            &payload.username,
            &payload.password,
            &source,
        )
    })
    .await;
    let issued = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return respond(&state, &headers, auth_error(error)),
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth_internal_error",
                    "administrator login task failed",
                ),
            );
        }
    };
    let mut response = Json(json!({"ok": true, "session": issued.session})).into_response();
    set_session_cookie(
        &mut response,
        &issued.cookie_value,
        COOKIE_MAX_AGE_SECONDS,
        state.config.web_console_secure_cookies,
    );
    clear_preauth_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

async fn console_session(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let Some(auth) = state.admin_auth.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "administrator authentication is unavailable",
            ),
        );
    };
    let Some(cookie) = session_cookie(&headers, state.config.web_console_secure_cookies) else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "administrator session is missing",
            ),
        );
    };
    match auth.refresh_admin_session(&cookie) {
        Ok(session) => respond(
            &state,
            &headers,
            Json(json!({"ok": true, "session": session})).into_response(),
        ),
        Err(error) => respond(&state, &headers, auth_error(error)),
    }
}

async fn auth_logout(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let (auth, cookie, csrf, _) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    if let Err(error) = auth.logout(&cookie, csrf.as_deref().unwrap_or_default()) {
        return respond(&state, &headers, auth_error(error));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    clear_session_cookie(&mut response, state.config.web_console_secure_cookies);
    respond(&state, &headers, response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeUpdateRequest {
    expected_revision: String,
    changes: Vec<RuntimeChangeRequest>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum RuntimeChangeRequest {
    Set { key: String, value: JsonValue },
    Remove { key: String },
}

async fn update_runtime_configuration(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<RuntimeUpdateRequest>,
) -> Response {
    let (_, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    let changes = match payload
        .changes
        .into_iter()
        .map(|change| match change {
            RuntimeChangeRequest::Set { key, value } => Ok(ManagedConfigChange::Set {
                key,
                value: json_to_toml(value)?,
            }),
            RuntimeChangeRequest::Remove { key } => Ok(ManagedConfigChange::Remove { key }),
        })
        .collect::<Result<Vec<_>, BoxedResponse>>()
    {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    match center.update_managed(&payload.expected_revision, &changes) {
        Ok(_) => configuration_success(&state, &headers, actor_id, "config.runtime.update"),
        Err(error) => {
            configuration_failure(&state, &headers, actor_id, "config.runtime.update", error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretUpdateRequest {
    changes: Vec<SecretChangeRequest>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum SecretChangeRequest {
    Replace {
        key: String,
        value: String,
        expected_revision: String,
    },
    Clear {
        key: String,
        expected_revision: String,
    },
}

async fn update_secret_configuration(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<SecretUpdateRequest>,
) -> Response {
    let (_, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    let changes = payload
        .changes
        .into_iter()
        .map(|change| match change {
            SecretChangeRequest::Replace {
                key,
                value,
                expected_revision,
            } => SecretConfigChange::Replace {
                key,
                value,
                expected_revision,
            },
            SecretChangeRequest::Clear {
                key,
                expected_revision,
            } => SecretConfigChange::Clear {
                key,
                expected_revision,
            },
        })
        .collect::<Vec<_>>();
    match center.update_secrets(&changes) {
        Ok(_) => configuration_success(&state, &headers, actor_id, "config.secret.update"),
        Err(error) => {
            configuration_failure(&state, &headers, actor_id, "config.secret.update", error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentUpdateRequest {
    expected_revision: String,
    changes: Vec<AgentChangeRequest>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum AgentChangeRequest {
    SetModelRoute {
        name: String,
        candidates: Vec<String>,
    },
    RemoveModelRoute {
        name: String,
    },
    SetSearchRoute {
        name: String,
        model: String,
    },
    RemoveSearchRoute {
        name: String,
    },
    SetProfile {
        name: String,
        profile: AgentProfileConfig,
    },
    RemoveProfile {
        name: String,
    },
    SetScene {
        scene: String,
        config: AgentSceneConfig,
    },
}

async fn update_agent_configuration(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<AgentUpdateRequest>,
) -> Response {
    let (_, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    let changes = match payload
        .changes
        .into_iter()
        .map(agent_change)
        .collect::<Result<Vec<_>, BoxedResponse>>()
    {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    match center.update_agent(&payload.expected_revision, &changes) {
        Ok(_) => configuration_success(&state, &headers, actor_id, "config.agent.update"),
        Err(error) => {
            configuration_failure(&state, &headers, actor_id, "config.agent.update", error)
        }
    }
}

async fn validate_configuration(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let (_, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    match center.current_snapshot() {
        Ok(snapshot) => {
            let valid = snapshot.fields.iter().all(|field| field.valid);
            let _ = state.admin_auth.as_ref().and_then(|auth| {
                auth.audit(
                    Some(actor_id),
                    "config.validate",
                    if valid { "success" } else { "invalid" },
                )
                .ok()
            });
            respond(
                &state,
                &headers,
                Json(json!({
                    "ok": valid,
                    "validation": {
                        "valid": valid,
                        "network_tested": false,
                        "message": if valid {
                            "配置通过与正式启动一致的本地预检；未执行外部网络请求"
                        } else {
                            "配置未通过正式启动预检，未保存任何变更"
                        }
                    }
                }))
                .into_response(),
            )
        }
        Err(error) => configuration_failure(&state, &headers, actor_id, "config.validate", error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionTestRequest {
    target: String,
}

async fn test_provider_connection(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<ConnectionTestRequest>,
) -> Response {
    let (_, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    let environment = match center.current_resolved_environment() {
        Ok(value) => value,
        Err(error) => {
            return configuration_failure(
                &state,
                &headers,
                actor_id,
                "config.connection_test",
                error,
            );
        }
    };
    let (url, api_key) = match connection_test_target(&payload.target, &environment) {
        Ok(value) => value,
        Err(response) => {
            let _ = state.admin_auth.as_ref().and_then(|auth| {
                auth.audit(Some(actor_id), "config.connection_test", "denied")
                    .ok()
            });
            return respond(&state, &headers, *response);
        }
    };
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(value) => value,
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "connection_test_unavailable",
                    "connection test client could not be initialized",
                ),
            );
        }
    };
    let result = match client.get(url).bearer_auth(api_key).send().await {
        Ok(response) => classify_connection_status(response.status()),
        Err(error) if error.is_timeout() => (false, "timeout", "连接超时；未修改任何配置"),
        Err(error) if error.is_connect() => {
            (false, "connect_failed", "无法连接 Provider；未修改任何配置")
        }
        Err(_) => (
            false,
            "transport_error",
            "Provider 连接发生传输错误；未修改任何配置",
        ),
    };
    let _ = state.admin_auth.as_ref().and_then(|auth| {
        auth.audit(
            Some(actor_id),
            "config.connection_test",
            if result.0 { "success" } else { "failed" },
        )
        .ok()
    });
    respond(
        &state,
        &headers,
        Json(json!({
            "ok": true,
            "connection": {
                "success": result.0,
                "classification": result.1,
                "message": result.2,
                "side_effect_free": true,
            }
        }))
        .into_response(),
    )
}

fn connection_test_target(
    target: &str,
    environment: &std::collections::HashMap<String, String>,
) -> Result<(url::Url, String), BoxedResponse> {
    let (base_url, api_key_env, allowed_host) = match target {
        "openai" => (
            environment
                .get("OPENAI_BASE_URLS")
                .and_then(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .find(|value| !value.is_empty())
                })
                .unwrap_or("https://api.openai.com/v1"),
            "OPENAI_API_KEY",
            "api.openai.com",
        ),
        "deepseek" => (
            environment
                .get("DEEPSEEK_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://api.deepseek.com"),
            "DEEPSEEK_API_KEY",
            "api.deepseek.com",
        ),
        "bigmodel" => (
            environment
                .get("BIGMODEL_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://open.bigmodel.cn/api/paas/v4"),
            "BIGMODEL_API_KEY",
            "open.bigmodel.cn",
        ),
        "gemini" => (
            environment
                .get("GEMINI_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://generativelanguage.googleapis.com/v1beta/openai"),
            "GEMINI_API_KEY",
            "generativelanguage.googleapis.com",
        ),
        "mimo" => (
            "https://api.xiaomimimo.com/v1",
            "MIMO_API_KEY",
            "api.xiaomimimo.com",
        ),
        _ => {
            return Err(Box::new(api_error(
                StatusCode::BAD_REQUEST,
                "unsupported_connection_target",
                "connection target is not supported",
            )));
        }
    };
    let api_key = environment
        .get(api_key_env)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Box::new(api_error(
                StatusCode::BAD_REQUEST,
                "provider_not_configured",
                "selected Provider API key is not configured",
            ))
        })?
        .to_owned();
    let mut url = url::Url::parse(base_url.trim()).map_err(|_| {
        Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "unsupported_connection_target",
            "selected Provider URL is invalid",
        ))
    })?;
    // 首版只探测官方 HTTPS 主机，避免把管理 API 变成任意 URL/内网 SSRF 入口。
    // 自定义 OpenAI-compatible 地址仍可保存并参加启动预检，但不从 WebUI 发起网络探测。
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(allowed_host))
    {
        return Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "custom_endpoint_not_testable",
            "custom Provider endpoints are not eligible for Web connection tests",
        )));
    }
    let path = format!("{}/models", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok((url, api_key))
}

fn classify_connection_status(status: reqwest::StatusCode) -> (bool, &'static str, &'static str) {
    match status.as_u16() {
        200..=299 => (true, "available", "Provider 认证与模型列表端点可用"),
        401 | 403 => (
            false,
            "authentication_failed",
            "Provider 拒绝凭据；未修改任何配置",
        ),
        404 | 405 => (
            false,
            "endpoint_unsupported",
            "Provider 可连接，但不支持受控模型列表探测；未修改任何配置",
        ),
        429 => (
            false,
            "upstream_rate_limited",
            "Provider 已限流；未修改任何配置",
        ),
        500..=599 => (
            false,
            "upstream_error",
            "Provider 返回服务端错误；未修改任何配置",
        ),
        _ => (
            false,
            "unexpected_status",
            "Provider 返回非预期状态；未修改任何配置",
        ),
    }
}

pub(super) fn require_admin(
    state: &OpsHttpState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Result<i64, BoxedResponse> {
    admin_context(state, headers, require_csrf).map(|(_, _, _, id)| id)
}

fn admin_context(
    state: &OpsHttpState,
    headers: &HeaderMap,
    require_csrf: bool,
) -> Result<(crate::management::AdminAuth, String, Option<String>, i64), BoxedResponse> {
    if !origin_allowed(headers) {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "origin_denied",
            "request origin is not allowed",
        )));
    }
    let auth = state.admin_auth.clone().ok_or_else(|| {
        Box::new(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth_unavailable",
            "administrator authentication is unavailable",
        ))
    })?;
    let cookie =
        session_cookie(headers, state.config.web_console_secure_cookies).ok_or_else(|| {
            Box::new(api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "administrator session is missing",
            ))
        })?;
    let csrf = csrf_token(headers);
    if require_csrf && csrf.is_none() {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "csrf_failed",
            "CSRF token is missing",
        )));
    }
    let (id, _) = auth
        .authorize_admin(
            &cookie,
            require_csrf.then_some(csrf.as_deref().unwrap_or_default()),
        )
        .map_err(|error| Box::new(auth_error(error)))?;
    if require_csrf {
        auth.check_management_rate_limit(id)
            .map_err(|error| Box::new(auth_error(error)))?;
    }
    Ok((auth, cookie, csrf, id))
}

fn agent_change(change: AgentChangeRequest) -> Result<AgentConfigChange, BoxedResponse> {
    Ok(match change {
        AgentChangeRequest::SetModelRoute { name, candidates } => {
            AgentConfigChange::SetModelRoute { name, candidates }
        }
        AgentChangeRequest::RemoveModelRoute { name } => {
            AgentConfigChange::RemoveModelRoute { name }
        }
        AgentChangeRequest::SetSearchRoute { name, model } => {
            AgentConfigChange::SetSearchRoute { name, model }
        }
        AgentChangeRequest::RemoveSearchRoute { name } => {
            AgentConfigChange::RemoveSearchRoute { name }
        }
        AgentChangeRequest::SetProfile { name, profile } => {
            AgentConfigChange::SetProfile { name, profile }
        }
        AgentChangeRequest::RemoveProfile { name } => AgentConfigChange::RemoveProfile { name },
        AgentChangeRequest::SetScene { scene, config } => AgentConfigChange::SetScene {
            scene: match scene.as_str() {
                "private" => ChatScene::Private,
                "group" => ChatScene::Group,
                _ => {
                    return Err(Box::new(api_error(
                        StatusCode::BAD_REQUEST,
                        "validation_error",
                        "agent scene must be private or group",
                    )));
                }
            },
            config,
        },
    })
}

fn configuration_success(
    state: &OpsHttpState,
    headers: &HeaderMap,
    actor_id: i64,
    event: &str,
) -> Response {
    let Some(center) = state.config_center.as_ref() else {
        return respond(
            state,
            headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "configuration center is unavailable",
            ),
        );
    };
    let snapshot = match center.current_snapshot() {
        Ok(value) => value,
        Err(error) => return configuration_failure(state, headers, actor_id, event, error),
    };
    if let Some(auth) = state.admin_auth.as_ref()
        && let Err(error) = auth.audit(Some(actor_id), event, "success")
    {
        return respond(
            state,
            headers,
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "ok": false,
                    "persisted": true,
                    "error": {"code": error.code(), "message": error.message()},
                })),
            )
                .into_response(),
        );
    }
    respond(
        state,
        headers,
        Json(json!({
            "ok": true,
            "persisted": true,
            "configuration": snapshot,
        }))
        .into_response(),
    )
}

fn configuration_failure(
    state: &OpsHttpState,
    headers: &HeaderMap,
    actor_id: i64,
    event: &str,
    error: ConfigCenterError,
) -> Response {
    if let Some(auth) = state.admin_auth.as_ref() {
        let _ = auth.audit(Some(actor_id), event, "failed");
    }
    respond(state, headers, config_error(error))
}

fn json_to_toml(value: JsonValue) -> Result<toml::Value, BoxedResponse> {
    match value {
        JsonValue::String(value) => Ok(toml::Value::String(value)),
        JsonValue::Bool(value) => Ok(toml::Value::Boolean(value)),
        JsonValue::Number(value) => value.as_i64().map(toml::Value::Integer).ok_or_else(|| {
            Box::new(api_error(
                StatusCode::BAD_REQUEST,
                "validation_error",
                "configuration number must be an integer",
            ))
        }),
        JsonValue::Array(values) => values
            .into_iter()
            .map(|value| match value {
                JsonValue::String(value) => Ok(toml::Value::String(value)),
                _ => Err(Box::new(api_error(
                    StatusCode::BAD_REQUEST,
                    "validation_error",
                    "configuration list items must be strings",
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        _ => Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "unsupported configuration value",
        ))),
    }
}

fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    url::Url::parse(origin)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|value| (value.to_owned(), url.port_or_known_default()))
        })
        .is_some_and(|(origin_host, origin_port)| {
            let mut parts = host.rsplitn(2, ':');
            let port_or_host = parts.next().unwrap_or_default();
            let maybe_host = parts.next();
            let (host_name, host_port) = match maybe_host {
                Some(name) if port_or_host.parse::<u16>().is_ok() => {
                    (name, port_or_host.parse::<u16>().ok())
                }
                _ => (host, None),
            };
            origin_host.eq_ignore_ascii_case(host_name)
                && (host_port.is_none() || host_port == origin_port)
        })
}

fn preauth_request_allowed(headers: &HeaderMap) -> bool {
    if !origin_allowed(headers) {
        return false;
    }
    headers.contains_key(header::ORIGIN)
        || headers
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
}

fn client_source(state: &OpsHttpState, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let peer_ip = peer.map(|address| address.ip());
    if peer_ip.is_some_and(|ip| state.config.web_console_trusted_proxy_ips.contains(&ip))
        && let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.contains(','))
            .and_then(|value| value.parse::<std::net::IpAddr>().ok())
    {
        return forwarded.to_string();
    }
    peer_ip
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|item| item.trim().split_once('='))
        .find_map(|(candidate, value)| (candidate == name).then(|| value.to_owned()))
}

fn session_cookie(headers: &HeaderMap, secure: bool) -> Option<String> {
    cookie_value(
        headers,
        if secure {
            SECURE_SESSION_COOKIE_NAME
        } else {
            SESSION_COOKIE_NAME
        },
    )
}

fn preauth_cookie(headers: &HeaderMap, secure: bool) -> Option<String> {
    cookie_value(
        headers,
        if secure {
            SECURE_PREAUTH_COOKIE_NAME
        } else {
            PREAUTH_COOKIE_NAME
        },
    )
}

fn csrf_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: i64, secure: bool) {
    let secure_attribute = if secure { "; Secure" } else { "" };
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure_attribute}"
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn set_session_cookie(response: &mut Response, value: &str, max_age: i64, secure: bool) {
    let name = if secure {
        SECURE_SESSION_COOKIE_NAME
    } else {
        SESSION_COOKIE_NAME
    };
    set_cookie(response, name, value, max_age, secure);
}

fn set_preauth_cookie(response: &mut Response, value: &str, max_age: i64, secure: bool) {
    let name = if secure {
        SECURE_PREAUTH_COOKIE_NAME
    } else {
        PREAUTH_COOKIE_NAME
    };
    set_cookie(response, name, value, max_age, secure);
}

fn clear_cookie(response: &mut Response, name: &str, secure: bool) {
    set_cookie(response, name, "", 0, secure);
}

fn clear_session_cookie(response: &mut Response, secure: bool) {
    clear_cookie(
        response,
        if secure {
            SECURE_SESSION_COOKIE_NAME
        } else {
            SESSION_COOKIE_NAME
        },
        secure,
    );
}

fn clear_preauth_cookie(response: &mut Response, secure: bool) {
    clear_cookie(
        response,
        if secure {
            SECURE_PREAUTH_COOKIE_NAME
        } else {
            PREAUTH_COOKIE_NAME
        },
        secure,
    );
}

fn auth_error(error: AdminAuthError) -> Response {
    let status = match error.code() {
        "unauthenticated" | "invalid_credentials" => StatusCode::UNAUTHORIZED,
        "csrf_failed" | "invalid_bootstrap_token" | "already_initialized" => StatusCode::FORBIDDEN,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "validation_error" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.code(), error.message())
}

fn config_error(error: ConfigCenterError) -> Response {
    let status = match error.code() {
        "config_conflict" => StatusCode::CONFLICT,
        "invalid_config" => StatusCode::UNPROCESSABLE_ENTITY,
        "config_io_error" | "secret_storage_error" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    api_error(status, error.code(), error.message())
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": {"code": code, "message": message},
        })),
    )
        .into_response()
}

fn respond(state: &OpsHttpState, headers: &HeaderMap, response: Response) -> Response {
    with_console_cors(response, state, headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_target_is_limited_to_registered_official_https_hosts() {
        let environment = std::collections::HashMap::from([
            ("OPENAI_API_KEY".to_owned(), "secret-value".to_owned()),
            (
                "OPENAI_BASE_URLS".to_owned(),
                "https://api.openai.com/v1".to_owned(),
            ),
        ]);
        let (url, key) = connection_test_target("openai", &environment).unwrap();
        assert_eq!(url.as_str(), "https://api.openai.com/v1/models");
        assert_eq!(key, "secret-value");

        let mut custom = environment;
        custom.insert(
            "OPENAI_BASE_URLS".to_owned(),
            "http://127.0.0.1:8080/v1".to_owned(),
        );
        assert!(connection_test_target("openai", &custom).is_err());
    }

    #[test]
    fn connection_status_has_stable_safe_classifications() {
        assert!(classify_connection_status(reqwest::StatusCode::OK).0);
        assert_eq!(
            classify_connection_status(reqwest::StatusCode::UNAUTHORIZED).1,
            "authentication_failed"
        );
        assert_eq!(
            classify_connection_status(reqwest::StatusCode::TOO_MANY_REQUESTS).1,
            "upstream_rate_limited"
        );
    }
}
