//! gateway 内部主动推送入口。
//!
//! 该 HTTP 服务默认只监听 127.0.0.1，供本机 LLM RSS 调度调用。
//! QQ token 和平台 payload 仍由 gateway 统一处理，避免业务服务绕过网关直连 QQ OpenAPI。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use crate::{
    api::{QqApiClient, build_c2c_text_payload, build_group_text_payload},
    gateway::{
        BotOutboundCache, logging::mask_identifier, outbound::record_qq_send_result,
        ping::GatewayRuntimeStatus,
    },
    markdown::MarkdownPayload,
};

#[derive(Debug, Clone)]
pub struct PushServerConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
struct PushState {
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    token: Option<String>,
    group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
}

#[derive(Debug, Deserialize)]
struct PushRequest {
    target_type: String,
    target_id: String,
    #[serde(default)]
    message_type: Option<String>,
    text: String,
    #[serde(default)]
    fallback_text: Option<String>,
}

pub(crate) async fn run_push_server(
    config: PushServerConfig,
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
) -> anyhow::Result<()> {
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let state = PushState {
        api,
        runtime,
        token: config
            .token
            .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_owned())),
        group_outbound_cache,
    };
    let router = Router::new()
        .route("/internal/push", post(push_message))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "gateway internal push server listening");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn push_message(
    State(state): State<PushState>,
    headers: HeaderMap,
    Json(req): Json<PushRequest>,
) -> Response {
    if !authorized(&headers, state.token.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "error": "unauthorized"})),
        )
            .into_response();
    }
    let target_id = req.target_id.trim();
    let text = req.text.trim();
    if target_id.is_empty() || text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "target_id and text are required"})),
        )
            .into_response();
    }

    let fallback_text = req
        .fallback_text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(text);
    let message_type = req.message_type.as_deref().unwrap_or("text").trim();
    let result = match req.target_type.trim() {
        "private" => {
            send_private_push(&state.api, target_id, message_type, text, fallback_text).await
        }
        "group" => send_group_push(&state.api, target_id, message_type, text, fallback_text).await,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "unsupported target_type"})),
            )
                .into_response();
        }
    };
    record_qq_send_result(&state.runtime, &result);
    // 群推送成功后，把返回的 message_id 写入共享的 BotOutboundCache，
    // 确保 mention 模式下用户回复该消息能被 is_reply_to_bot 识别。
    if req.target_type.trim() == "group"
        && let Ok(Some(message_id)) = result.as_ref()
    {
        state
            .group_outbound_cache
            .lock()
            .unwrap()
            .insert(Some(message_id.clone()));
    }
    match result {
        Ok(_) => {
            info!(
                target_type = %req.target_type,
                target = %mask_identifier(target_id),
                "gateway internal push sent"
            );
            Json(json!({"ok": true})).into_response()
        }
        Err(err) => {
            warn!(
                target_type = %req.target_type,
                target = %mask_identifier(target_id),
                error = %err.log_summary(),
                "gateway internal push failed"
            );
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"ok": false, "error": err.log_summary()})),
            )
                .into_response()
        }
    }
}

async fn send_private_push(
    api: &QqApiClient,
    target_id: &str,
    message_type: &str,
    text: &str,
    fallback_text: &str,
) -> crate::api::SendResult {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match api.send_c2c_markdown(target_id, None, &markdown).await {
                Ok(message_id) => Ok(message_id),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "internal markdown push failed; falling back to text"
                    );
                    api.send_c2c_text(target_id, None, fallback_text).await
                }
            }
        }
        "text" | "" => {
            // 主动推送没有原始 QQ msg_id，因此只发送 content/msg_type/msg_seq。
            let _shape = build_c2c_text_payload(text, None, 1);
            api.send_c2c_text(target_id, None, text).await
        }
        _ => Err(crate::api::ApiError::Unsupported("message_type")),
    }
}

async fn send_group_push(
    api: &QqApiClient,
    target_id: &str,
    message_type: &str,
    text: &str,
    fallback_text: &str,
) -> crate::api::SendResult {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match api.send_group_markdown(target_id, None, &markdown).await {
                Ok(message_id) => Ok(message_id),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "internal group markdown push failed; falling back to text"
                    );
                    api.send_group_text(target_id, None, fallback_text).await
                }
            }
        }
        "text" | "" => {
            // QQ 群 openid 主动消息使用 /v2/groups/{group_openid}/messages。
            let _shape = build_group_text_payload(text, None, 1);
            api.send_group_text(target_id, None, text).await
        }
        _ => Err(crate::api::ApiError::Unsupported("message_type")),
    }
}

fn authorized(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    headers
        .get("X-QQ-Maid-Push-Token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_auth_is_required_when_configured() {
        let headers = HeaderMap::new();
        assert!(!authorized(&headers, Some("secret")));
        assert!(authorized(&headers, None));
    }
}
