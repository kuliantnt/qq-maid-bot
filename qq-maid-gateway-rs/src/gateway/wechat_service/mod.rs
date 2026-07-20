//! 微信服务号 HTTP 回调入口。
//!
//! 当前实现 text-only 同步回复和长任务客服消息补发；模板消息、素材和媒体消息均留给后续任务。

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use qq_maid_core::service::CoreRespondOutput;
use serde::Deserialize;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::{WechatServiceConfig, WechatServiceEncryptionMode},
    gateway::{
        command::{GatewayCommandContext, GatewayCommandConversation, GatewayCommandService},
        dedupe::MessageDedupe,
        outbound::ReplyCapability,
        ping::GatewayRuntimeStatus,
        platform::{
            self,
            wechat_service::{
                WechatInboundMessage, WechatMessageCrypto, WechatTextMessage,
                inbound_from_text_message, parse_encrypted_message_xml, parse_message_xml,
                random_callback_nonce, render_encrypted_reply_xml, render_text_reply_xml,
                verify_signature,
            },
        },
    },
    logging::mask_openid,
    render::render_respond_response_for_profile,
    respond::{RespondClient, RespondError, respond_error_to_qq_text},
};

mod customer;

use customer::{WechatCustomerMessenger, build_customer_messenger};

const FALLBACK_ERROR_TEXT: &str = "服务暂时不可用，请稍后再试。";
const SLOW_SYNC_FALLBACK_TEXT: &str = "这次处理需要更久一点，已收到请求，请稍后查看回复。";
const WECHAT_SUCCESS_BODY: &str = "success";

#[derive(Clone)]
struct WechatServiceState {
    config: WechatServiceConfig,
    message_crypto: Option<Arc<WechatMessageCrypto>>,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    customer_messenger: Option<Arc<dyn WechatCustomerMessenger>>,
    commands: Option<GatewayCommandService>,
}

#[derive(Debug, Deserialize)]
struct VerifyQuery {
    signature: Option<String>,
    msg_signature: Option<String>,
    timestamp: Option<String>,
    nonce: Option<String>,
    echostr: Option<String>,
    encrypt_type: Option<String>,
}

pub(super) async fn spawn_callback_server(
    config: WechatServiceConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    runtime: GatewayRuntimeStatus,
    commands: GatewayCommandService,
    shutdown_token: CancellationToken,
) -> anyhow::Result<JoinHandle<anyhow::Result<()>>> {
    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port)
        .parse()
        .context("parse wechat service callback bind addr")?;
    let listener = TcpListener::bind(addr)
        .await
        .context("bind wechat service callback listener")?;
    let path = config.callback_path.clone();
    let message_crypto = build_message_crypto(&config)?;
    let state = WechatServiceState {
        customer_messenger: build_customer_messenger(&config),
        message_crypto,
        config,
        respond,
        dedupe,
        commands: Some(commands),
    };
    let app = Router::new()
        .route(&path, get(verify_url).post(handle_message))
        .with_state(state);

    info!(%addr, path = %path, "wechat service callback listening");
    runtime.record_wechat_service_listening();
    Ok(tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_token.cancelled().await;
            })
            .await
            .context("serve wechat service callback");
        runtime.record_wechat_service_stopped();
        result
    }))
}

async fn verify_url(
    State(state): State<WechatServiceState>,
    Query(query): Query<VerifyQuery>,
) -> Response {
    let Some(echostr) = query.echostr.as_deref() else {
        return plain(StatusCode::BAD_REQUEST, "missing echostr");
    };
    match state.config.encryption_mode {
        WechatServiceEncryptionMode::Plaintext => {
            if !verify_plaintext_query_signature(&state, &query) {
                return plain(StatusCode::FORBIDDEN, "invalid signature");
            }
            plain(StatusCode::OK, echostr)
        }
        WechatServiceEncryptionMode::Aes => verify_encrypted_url(&state, &query, echostr),
    }
}

async fn handle_message(
    State(state): State<WechatServiceState>,
    Query(query): Query<VerifyQuery>,
    body: Bytes,
) -> Response {
    let body = match decode_callback_body(&state, &query, &body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let message = match parse_message_xml(&body) {
        Ok(WechatInboundMessage::Text(message)) => message,
        Ok(WechatInboundMessage::Unsupported { msg_type }) => {
            debug!(msg_type = %msg_type, "wechat service message type is not supported");
            return plain(StatusCode::OK, "");
        }
        Err(error) => return plain(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if message.content.trim().is_empty() {
        // 微信服务号允许同步返回空串表示本轮不回复；空 Content 不进入 Core，避免制造无意义会话。
        return plain(StatusCode::OK, "");
    }
    let inbound = inbound_from_text_message(&message);
    let reservation = match reserve_wechat_message(&state, &inbound, &message.msg_id) {
        Ok(reservation) => reservation,
        Err(()) => return plain(StatusCode::OK, ""),
    };
    if let Some(commands) = state.commands.as_ref() {
        let context = GatewayCommandContext {
            platform_name: "微信服务号",
            platform_code: "wechat_service",
            event_name: "text_message",
            conversation: GatewayCommandConversation::ServiceAccount,
            user_id: Some(message.from_user_name.clone()),
            group_id: None,
            message_id: Some(message.msg_id.clone()),
            timestamp: message.create_time.clone(),
            attachment_count: 0,
        };
        if let Some(output) = commands.try_handle(&message.content, &context).await {
            if let Some(reservation) = reservation {
                reservation.commit();
            }
            let capability = ReplyCapability::wechat_service_text_sync(state.config.reply_timeout);
            let reply = output.render(&capability).fallback_text().to_owned();
            return render_sync_reply(&state, &message, reply);
        }
    }

    let reply_timeout = state.config.reply_timeout;
    let (reply_tx, reply_rx) = oneshot::channel();
    tokio::spawn(run_response_job(
        state.clone(),
        message.clone(),
        inbound,
        reservation,
        reply_tx,
    ));

    let reply = match tokio::time::timeout(reply_timeout, reply_rx).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) => FALLBACK_ERROR_TEXT.to_owned(),
        Err(_) => {
            warn!(
                message_id = %message.msg_id,
                timeout_ms = reply_timeout.as_millis(),
                customer_message_enabled = state.customer_messenger.is_some(),
                "wechat service respond exceeded sync budget"
            );
            if state.customer_messenger.is_some() {
                return plain(StatusCode::OK, WECHAT_SUCCESS_BODY);
            }
            SLOW_SYNC_FALLBACK_TEXT.to_owned()
        }
    };
    if reply.trim().is_empty() {
        return plain(StatusCode::OK, "");
    }

    render_sync_reply(&state, &message, reply)
}

async fn run_response_job(
    state: WechatServiceState,
    message: WechatTextMessage,
    inbound: platform::InboundMessage,
    reservation: Option<crate::gateway::dedupe::MessageReservation>,
    reply_tx: oneshot::Sender<String>,
) {
    let reply = build_reply_text(
        &state.respond,
        &message,
        &inbound,
        state.config.reply_timeout,
    )
    .await;
    let needs_async_follow_up = reply_tx.send(reply.clone()).is_err();
    if let Some(reservation) = reservation {
        // 慢请求补发会等待外部微信 API；去重 reservation 只保护 Core 处理阶段，避免外部发送卡住时永久占住 MsgId。
        reservation.commit();
    }
    if needs_async_follow_up {
        handle_slow_job_completion(&state, &message, &reply).await;
    }
}

async fn handle_slow_job_completion(
    state: &WechatServiceState,
    message: &WechatTextMessage,
    reply: &str,
) {
    let Some(messenger) = state.customer_messenger.as_ref() else {
        info!(
            message_id = %message.msg_id,
            user = %mask_openid(&message.from_user_name),
            reply_len = reply.chars().count(),
            "wechat service slow response completed without customer-message capability"
        );
        return;
    };
    if reply.trim().is_empty() {
        info!(
            message_id = %message.msg_id,
            user = %mask_openid(&message.from_user_name),
            "wechat service slow response completed with empty reply; customer message skipped"
        );
        return;
    }
    match messenger.send_text(&message.from_user_name, reply).await {
        Ok(()) => {
            info!(
                message_id = %message.msg_id,
                user = %mask_openid(&message.from_user_name),
                reply_len = reply.chars().count(),
                "wechat customer text message sent"
            );
        }
        Err(error) => {
            warn!(
                message_id = %message.msg_id,
                user = %mask_openid(&message.from_user_name),
                reply_len = reply.chars().count(),
                error = %error.log_summary(),
                "wechat customer text message failed"
            );
        }
    }
}

fn reserve_wechat_message(
    state: &WechatServiceState,
    inbound: &platform::InboundMessage,
    message_id: &str,
) -> Result<Option<crate::gateway::dedupe::MessageReservation>, ()> {
    let Some(key) = inbound.dedupe_message_key() else {
        return Ok(None);
    };
    match state.dedupe.reserve_many([key], Instant::now()) {
        Ok(reservation) => Ok(Some(reservation)),
        Err(_) => {
            info!(
                message_id = %message_id,
                "wechat service duplicate message retry ignored"
            );
            Err(())
        }
    }
}

async fn build_reply_text(
    respond: &RespondClient,
    message: &WechatTextMessage,
    inbound: &platform::InboundMessage,
    reply_timeout: Duration,
) -> String {
    let content = platform::render_text_for_core(inbound);
    let capability = ReplyCapability::wechat_service_text_sync(reply_timeout);
    let response = match respond.respond_inbound(inbound, content).await {
        Ok(CoreRespondOutput::Complete(response)) => Some(response),
        Ok(CoreRespondOutput::Stream(mut stream)) => {
            // 微信服务号同步回复不支持流式；这里只消费到 Completed，超时由外层统一处理。
            let mut completed = None;
            while let Some(event) = stream.recv().await {
                match event {
                    qq_maid_core::service::CoreResponseEvent::Completed(response) => {
                        completed = Some(response);
                        break;
                    }
                    qq_maid_core::service::CoreResponseEvent::Failed(failure) => {
                        warn!(
                            message_id = %message.msg_id,
                            kind = ?failure.kind,
                            "wechat service core stream failed"
                        );
                        return FALLBACK_ERROR_TEXT.to_owned();
                    }
                    _ => {}
                }
            }
            completed
        }
        Err(error) => {
            warn!(
                message_id = %message.msg_id,
                error = %error.log_summary(),
                "wechat service respond failed"
            );
            return respond_error_to_wechat_text(&error);
        }
    };

    let Some(response) = response else {
        return FALLBACK_ERROR_TEXT.to_owned();
    };
    render_respond_response_for_profile(&response, &capability.render)
        .map(|outbound| outbound.fallback_text().to_owned())
        .unwrap_or_default()
}

fn build_message_crypto(
    config: &WechatServiceConfig,
) -> anyhow::Result<Option<Arc<WechatMessageCrypto>>> {
    if config.encryption_mode == WechatServiceEncryptionMode::Plaintext {
        return Ok(None);
    }
    let token = config
        .token
        .as_deref()
        .context("wechat AES mode requires token")?;
    let app_id = config
        .app_id
        .as_deref()
        .context("wechat AES mode requires app id")?;
    let encoding_aes_key = config
        .encoding_aes_key
        .as_deref()
        .context("wechat AES mode requires EncodingAESKey")?;
    let crypto = WechatMessageCrypto::new(token, app_id, encoding_aes_key)
        .context("initialize wechat AES message crypto")?;
    Ok(Some(Arc::new(crypto)))
}

fn verify_plaintext_query_signature(state: &WechatServiceState, query: &VerifyQuery) -> bool {
    let Some(token) = state.config.token.as_deref() else {
        return false;
    };
    let (Some(signature), Some(timestamp), Some(nonce)) = (
        query.signature.as_deref(),
        query.timestamp.as_deref(),
        query.nonce.as_deref(),
    ) else {
        return false;
    };
    verify_signature(token, timestamp, nonce, signature)
}

fn verify_encrypted_url(
    state: &WechatServiceState,
    query: &VerifyQuery,
    encrypted_echo: &str,
) -> Response {
    if query
        .encrypt_type
        .as_deref()
        .is_some_and(|mode| mode != "aes")
    {
        return plain(StatusCode::BAD_REQUEST, "invalid encrypted callback mode");
    }
    let (Some(crypto), Some(signature), Some(timestamp), Some(nonce)) = (
        state.message_crypto.as_deref(),
        query.msg_signature.as_deref(),
        query.timestamp.as_deref(),
        query.nonce.as_deref(),
    ) else {
        return plain(
            StatusCode::BAD_REQUEST,
            "missing encrypted callback parameters",
        );
    };
    if !crypto.verify_message_signature(timestamp, nonce, encrypted_echo, signature) {
        return plain(StatusCode::FORBIDDEN, "invalid msg_signature");
    }
    match crypto.decrypt(encrypted_echo) {
        Ok(echo) => plain(StatusCode::OK, &echo),
        Err(error) => {
            warn!(error = %error, "wechat encrypted URL verification failed");
            plain(StatusCode::BAD_REQUEST, "invalid encrypted echostr")
        }
    }
}

fn decode_callback_body(
    state: &WechatServiceState,
    query: &VerifyQuery,
    body: &[u8],
) -> Result<String, Box<Response>> {
    let body = std::str::from_utf8(body)
        .map_err(|_| Box::new(plain(StatusCode::BAD_REQUEST, "invalid utf-8 xml")))?;
    match state.config.encryption_mode {
        WechatServiceEncryptionMode::Plaintext => {
            if query.encrypt_type.as_deref() == Some("aes") {
                return Err(Box::new(plain(
                    StatusCode::BAD_REQUEST,
                    "encrypted callback is not configured",
                )));
            }
            if !verify_plaintext_query_signature(state, query) {
                return Err(Box::new(plain(StatusCode::FORBIDDEN, "invalid signature")));
            }
            Ok(body.to_owned())
        }
        WechatServiceEncryptionMode::Aes => {
            if query.encrypt_type.as_deref() != Some("aes") {
                return Err(Box::new(plain(
                    StatusCode::BAD_REQUEST,
                    "encrypted callback required",
                )));
            }
            let encrypted = parse_encrypted_message_xml(body)
                .map_err(|_| Box::new(plain(StatusCode::BAD_REQUEST, "invalid encrypted xml")))?;
            let (Some(crypto), Some(signature), Some(timestamp), Some(nonce)) = (
                state.message_crypto.as_deref(),
                query.msg_signature.as_deref(),
                query.timestamp.as_deref(),
                query.nonce.as_deref(),
            ) else {
                return Err(Box::new(plain(
                    StatusCode::BAD_REQUEST,
                    "missing encrypted callback parameters",
                )));
            };
            if !crypto.verify_message_signature(timestamp, nonce, &encrypted, signature) {
                return Err(Box::new(plain(
                    StatusCode::FORBIDDEN,
                    "invalid msg_signature",
                )));
            }
            crypto.decrypt(&encrypted).map_err(|error| {
                warn!(error = %error, "wechat encrypted callback decryption failed");
                Box::new(plain(StatusCode::BAD_REQUEST, "invalid encrypted message"))
            })
        }
    }
}

fn render_sync_reply(
    state: &WechatServiceState,
    message: &WechatTextMessage,
    reply: String,
) -> Response {
    let now = now_unix_seconds();
    let inner_xml = render_text_reply_xml(
        message,
        &crate::render::OutboundMessage::Text { text: reply },
        now,
    );
    if state.config.encryption_mode == WechatServiceEncryptionMode::Plaintext {
        return xml_response(inner_xml);
    }

    let Some(crypto) = state.message_crypto.as_deref() else {
        return plain(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wechat encryption unavailable",
        );
    };
    let timestamp = now.to_string();
    let result = (|| {
        let encrypted = crypto.encrypt(&inner_xml)?;
        let nonce = random_callback_nonce()?;
        let signature = crypto.message_signature(&timestamp, &nonce, &encrypted);
        Ok::<_, crate::gateway::platform::wechat_service::WechatCryptoError>(
            render_encrypted_reply_xml(&encrypted, &signature, &timestamp, &nonce),
        )
    })();
    match result {
        Ok(xml) => xml_response(xml),
        Err(error) => {
            warn!(error = %error, "wechat encrypted reply generation failed");
            plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "wechat reply encryption failed",
            )
        }
    }
}

fn respond_error_to_wechat_text(error: &RespondError) -> String {
    let text = respond_error_to_qq_text(error);
    if text.trim().is_empty() {
        FALLBACK_ERROR_TEXT.to_owned()
    } else {
        text
    }
}

fn plain(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_owned(),
    )
        .into_response()
}

fn xml_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
