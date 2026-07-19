//! 部署管理员认证与配置写 API。
//!
//! HTTP handler 只负责认证、CSRF、参数解析和真实领域结果映射；配置校验、revision
//! 冲突、TOML 原子写入和 secret 加密继续由 `ConfigCenter` 负责。

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{patch, post},
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::{net::IpAddr, time::Duration};
use tokio::net::lookup_host;

use crate::config::{
    ChatScene,
    agent::{
        AgentProfileConfig, AgentSceneConfig, KnowledgeEmbeddingConfig, KnowledgeRetrievalMode,
    },
    center::{AgentConfigChange, ConfigCenterError, ManagedConfigChange, SecretConfigChange},
};

use super::{console_routes::with_console_cors, routes::OpsHttpState};

mod auth_routes;

use auth_routes::{auth_error, csrf_token, origin_allowed, session_cookie};

pub(super) type BoxedResponse = Box<Response>;

pub(super) fn management_router() -> Router<OpsHttpState> {
    Router::new()
        .merge(auth_routes::router())
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
        .route("/api/v1/console/restart", post(restart_process))
        .layer(DefaultBodyLimit::max(256 * 1024))
}

async fn restart_process(State(state): State<OpsHttpState>, headers: HeaderMap) -> Response {
    let (auth, _, _, actor_id) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    if !state.restart_controller.available() {
        let _ = auth.audit(Some(actor_id), "process.restart", "unavailable");
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "restart_unavailable",
                "当前运行目录没有可用的受控重启脚本",
            ),
        );
    }
    // 这里只记录管理员请求已被接受，不能把异步命令提交等同于进程重启成功。
    if let Err(error) = auth.audit(Some(actor_id), "process.restart", "accepted") {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.code(),
                error.message(),
            ),
        );
    }
    match state.restart_controller.schedule() {
        Ok(()) => respond(
            &state,
            &headers,
            Json(json!({
                "ok": true,
                "restart_scheduled": true,
                "message": "重启命令已提交，服务会短暂离线",
            }))
            .into_response(),
        ),
        Err(message) => {
            let _ = auth.audit(Some(actor_id), "process.restart", "unavailable");
            respond(
                &state,
                &headers,
                api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "restart_unavailable",
                    message,
                ),
            )
        }
    }
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
    SetKnowledge {
        mode: KnowledgeRetrievalMode,
        embedding: KnowledgeEmbeddingConfig,
    },
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
    let (host, addresses) = match resolve_public_connection_target(&url).await {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let client_builder = match qq_maid_common::http_client::try_builder() {
        Ok(value) => value,
        Err(_) => {
            return respond(
                &state,
                &headers,
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "connection_test_unavailable",
                    "connection test TLS could not be initialized",
                ),
            );
        }
    };
    let client = match client_builder
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        // 禁止环境代理重新解析目标；请求固定使用上一步校验过的公网地址，避免 DNS rebinding。
        .no_proxy()
        .resolve_to_addrs(&host, &addresses)
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
    let (base_url, api_key_env) = match target {
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
        ),
        "deepseek" => (
            environment
                .get("DEEPSEEK_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://api.deepseek.com"),
            "DEEPSEEK_API_KEY",
        ),
        "bigmodel" => (
            environment
                .get("BIGMODEL_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://open.bigmodel.cn/api/paas/v4"),
            "BIGMODEL_API_KEY",
        ),
        "gemini" => (
            environment
                .get("GEMINI_BASE_URL")
                .map(String::as_str)
                .unwrap_or("https://generativelanguage.googleapis.com/v1beta/openai"),
            "GEMINI_API_KEY",
        ),
        "mimo" => ("https://api.xiaomimimo.com/v1", "MIMO_API_KEY"),
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
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "custom_endpoint_not_testable",
            "Provider 连接测试只支持不含用户信息的 HTTPS 地址",
        )));
    }
    let path = format!("{}/models", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok((url, api_key))
}

async fn resolve_public_connection_target(
    url: &url::Url,
) -> Result<(String, Vec<std::net::SocketAddr>), BoxedResponse> {
    let host = url.host_str().ok_or_else(|| {
        Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "custom_endpoint_not_testable",
            "Provider 地址缺少主机名",
        ))
    })?;
    if is_blocked_connection_hostname(host) {
        return Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "unsafe_connection_target",
            "Provider 连接测试不允许访问本机或内网地址",
        )));
    }
    let port = url.port_or_known_default().ok_or_else(|| {
        Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "custom_endpoint_not_testable",
            "Provider 地址缺少有效端口",
        ))
    })?;
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| {
            Box::new(api_error(
                StatusCode::BAD_GATEWAY,
                "connection_dns_failed",
                "无法解析 Provider 主机名",
            ))
        })?
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_non_public_ip(address.ip()))
    {
        return Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "unsafe_connection_target",
            "Provider 连接测试不允许访问本机或内网地址",
        )));
    }
    Ok((host.to_owned(), addresses))
}

fn is_blocked_connection_hostname(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "metadata"
        || host == "metadata.google.internal"
}

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a == 10
                || (a == 100 && (64..=127).contains(&b))
                || a == 127
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && matches!(c, 0 | 2))
                || (a == 192 && b == 168)
                || (a == 198 && matches!(b, 18 | 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_non_public_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xffc0) == 0xfec0
                || (segments[0] == 0x0064 && segments[1] == 0xff9b && matches!(segments[2], 0 | 1))
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
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
        AgentChangeRequest::SetKnowledge { mode, embedding } => {
            AgentConfigChange::SetKnowledge { mode, embedding }
        }
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
            "registered_tools": state.registered_tools.as_ref(),
            "restart": {"available": state.restart_controller.available()},
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
    fn connection_target_accepts_configured_custom_https_hosts() {
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
            "https://provider.example.com/openai/v1".to_owned(),
        );
        let (url, _) = connection_test_target("openai", &custom).unwrap();
        assert_eq!(
            url.as_str(),
            "https://provider.example.com/openai/v1/models"
        );

        custom.insert(
            "OPENAI_BASE_URLS".to_owned(),
            "http://127.0.0.1:8080/v1".to_owned(),
        );
        assert!(connection_test_target("openai", &custom).is_err());
    }

    #[test]
    fn connection_target_rejects_non_public_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.0.1",
            "198.18.0.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "64:ff9b::a00:1",
            "2001:db8::1",
        ] {
            assert!(is_non_public_ip(value.parse().unwrap()), "{value}");
        }
        for value in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_non_public_ip(value.parse().unwrap()), "{value}");
        }
        assert!(is_blocked_connection_hostname("metadata.google.internal"));
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
