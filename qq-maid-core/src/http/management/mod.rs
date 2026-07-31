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

use crate::config::{
    ChatScene,
    agent::{
        AgentProfileConfig, AgentSceneConfig, KnowledgeEmbeddingConfig, KnowledgeRetrievalMode,
    },
    center::{
        AgentConfigChange, AgentProviderUpdate, ConfigCenterError, ManagedConfigChange,
        SecretConfigChange,
    },
};

use super::{
    api::common::{authenticate_admin_request, error_response as api_error},
    console_routes::with_console_cors,
    routes::OpsHttpState,
};

mod auth_routes;

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
    SetProvider {
        id: String,
        provider: AgentProviderUpdate,
    },
    RemoveProvider {
        id: String,
    },
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
    SetWebSearch {
        backend: String,
        max_results: u8,
        search_depth: String,
        topic: String,
        time_range: Option<String>,
        connect_timeout_seconds: u64,
        first_response_timeout_seconds: u64,
        total_timeout_seconds: u64,
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
    let authenticated = authenticate_admin_request(state, headers, require_csrf)
        .map_err(|error| Box::new(error.into_response()))?;
    Ok((
        authenticated.auth,
        authenticated.cookie,
        authenticated.csrf,
        authenticated.actor_id,
    ))
}

fn agent_change(change: AgentChangeRequest) -> Result<AgentConfigChange, BoxedResponse> {
    Ok(match change {
        AgentChangeRequest::SetProvider { id, provider } => {
            AgentConfigChange::SetProvider { id, provider }
        }
        AgentChangeRequest::RemoveProvider { id } => AgentConfigChange::RemoveProvider { id },
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
        AgentChangeRequest::SetWebSearch {
            backend,
            max_results,
            search_depth,
            topic,
            time_range,
            connect_timeout_seconds,
            first_response_timeout_seconds,
            total_timeout_seconds,
        } => AgentConfigChange::SetWebSearch {
            backend,
            max_results,
            search_depth,
            topic,
            time_range,
            connect_timeout_seconds,
            first_response_timeout_seconds,
            total_timeout_seconds,
        },
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

fn respond(state: &OpsHttpState, headers: &HeaderMap, response: Response) -> Response {
    with_console_cors(response, state, headers)
}
