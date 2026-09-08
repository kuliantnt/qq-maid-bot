//! 供应商凭证与诊断复用管理员认证、同源、CSRF 及审计边界。
use super::*;

pub(super) fn router() -> Router<OpsHttpState> {
    Router::new()
        .route(
            "/api/v1/console/configuration/providers/model-metadata",
            post(model_metadata),
        )
        .route(
            "/api/v1/console/configuration/providers/model-override",
            patch(model_override),
        )
        .route(
            "/api/v1/console/configuration/providers/credential",
            patch(credential),
        )
        .route("/api/v1/console/configuration/providers/test", post(test))
        .route(
            "/api/v1/console/configuration/providers/models",
            post(models),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRequest {
    id: String,
    expected_agent_revision: String,
    expected_revision: String,
    /// null 明确清除，字符串明确替换；请求结构不可 Debug，避免泄露凭证。
    #[serde(deserialize_with = "credential_value")]
    value: Option<String>,
}

fn credential_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

async fn credential(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<CredentialRequest>,
) -> Response {
    let (_, _, _, actor) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = &state.config_center else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "配置中心不可用",
            ),
        );
    };
    match center.update_connection_credential(
        &payload.id,
        &payload.expected_agent_revision,
        &payload.expected_revision,
        payload.value.as_deref(),
    ) {
        Ok(()) => configuration_success(&state, &headers, actor, "config.connection.credential"),
        Err(error) => configuration_failure(
            &state,
            &headers,
            actor,
            "config.connection.credential",
            error,
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestRequest {
    id: String,
    expected_revision: String,
    model: String,
}

async fn test(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<TestRequest>,
) -> Response {
    let (auth, _, _, actor) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = &state.config_center else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "配置中心不可用",
            ),
        );
    };
    match center
        .test_provider_connection(&payload.id, &payload.expected_revision, &payload.model)
        .await
    {
        Ok(result) => {
            if let Err(error) = auth.audit(Some(actor), "config.connection.test", result.category) {
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
            respond(
                &state,
                &headers,
                Json(json!({"ok": true, "diagnostic": result})).into_response(),
            )
        }
        Err(error) => {
            configuration_failure(&state, &headers, actor, "config.connection.test", error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsRequest {
    id: String,
    expected_revision: String,
}

// 显式 POST 触发只读上游请求，仍要求 Origin / CSRF，避免跨站消耗供应商配额。
async fn models(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<ModelsRequest>,
) -> Response {
    let (auth, _, _, actor) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = &state.config_center else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "配置中心不可用",
            ),
        );
    };
    match center
        .discover_connection_models(&payload.id, &payload.expected_revision)
        .await
    {
        Ok(result) => {
            if let Err(error) = auth.audit(Some(actor), "config.connection.models", result.category)
            {
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
            respond(
                &state,
                &headers,
                Json(json!({"ok": true, "discovery": result, "connection_id": payload.id, "revision": payload.expected_revision})).into_response(),
            )
        }
        Err(error) => {
            configuration_failure(&state, &headers, actor, "config.connection.models", error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataRequest {
    id: String,
}

async fn model_metadata(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<MetadataRequest>,
) -> Response {
    let (_, _, _, actor) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = &state.config_center else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "配置中心不可用",
            ),
        );
    };
    match center.connection_model_metadata(&payload.id) {
        Ok(metadata) => respond(
            &state,
            &headers,
            Json(json!({"ok": true, "metadata": metadata})).into_response(),
        ),
        Err(error) => configuration_failure(&state, &headers, actor, "config.models.read", error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideRequest {
    id: String,
    expected_revision: String,
    model: serde_json::Value,
}

async fn model_override(
    State(state): State<OpsHttpState>,
    headers: HeaderMap,
    Json(payload): Json<OverrideRequest>,
) -> Response {
    let (_, _, _, actor) = match admin_context(&state, &headers, true) {
        Ok(value) => value,
        Err(response) => return respond(&state, &headers, *response),
    };
    let Some(center) = &state.config_center else {
        return respond(
            &state,
            &headers,
            api_error(
                StatusCode::NOT_FOUND,
                "configuration_unavailable",
                "配置中心不可用",
            ),
        );
    };
    match center.update_model_override(&payload.id, &payload.expected_revision, payload.model) {
        Ok(()) => configuration_success(&state, &headers, actor, "config.models.update"),
        Err(error) => configuration_failure(&state, &headers, actor, "config.models.update", error),
    }
}
