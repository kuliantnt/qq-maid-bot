//! 反向 WebSocket 监听器、鉴权和单连接事件循环。

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket, close_code},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::Value;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    config::{AppConfig, OneBot11Config},
    gateway::{
        dedupe::MessageDedupe,
        logging::mask_identifier,
        ping::GatewayRuntimeStatus,
        platform::onebot11::{OneBotInboundOutcome, inbound_from_event},
    },
    respond::RespondClient,
};

use super::{
    connection::{OneBotConnectionContext, Registration, RegistrationError},
    dispatch::OneBotInboundDispatcher,
    protocol::{ActionResponse, OneBotEvent, OneBotId},
    scope_dispatcher::{OneBotEnqueueError, OneBotEnqueueOutcome, OneBotScopeDispatcher},
    sender::OneBotSender,
};

const AUTHORIZATION: &str = "authorization";
const X_SELF_ID: &str = "x-self-id";
const OUTBOUND_QUEUE_CAPACITY: usize = 64;

pub struct OneBotServerHandle {
    pub local_addr: SocketAddr,
    pub connection: OneBotConnectionContext,
    pub task: JoinHandle<anyhow::Result<()>>,
}

#[derive(Clone)]
struct ServerState {
    config: OneBot11Config,
    connection: OneBotConnectionContext,
    runtime: GatewayRuntimeStatus,
    shutdown: CancellationToken,
    dispatcher: OneBotScopeDispatcher,
}

/// 先完成 bind 再返回，调用方可把 task 纳入 Gateway 顶层监督；客户端连接错误由 Axum
/// 独立 handler 隔离，不会让监听器或其它平台渠道退出。
pub async fn spawn_reverse_websocket_server(
    app_config: AppConfig,
    respond: RespondClient,
    dedupe: Arc<MessageDedupe>,
    runtime: GatewayRuntimeStatus,
    shutdown: CancellationToken,
) -> anyhow::Result<OneBotServerHandle> {
    let config = app_config.onebot11.clone();
    if !config.enabled {
        bail!("OneBot 11 reverse WebSocket server is disabled");
    }
    if config.access_token.is_none() {
        bail!("OneBot 11 access token is required when enabled");
    }
    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port)
        .parse()
        .context("parse OneBot 11 reverse WebSocket bind addr")?;
    let listener = TcpListener::bind(addr)
        .await
        .context("bind OneBot 11 reverse WebSocket listener")?;
    let local_addr = listener
        .local_addr()
        .context("read OneBot 11 listener address")?;
    let path = config.websocket_path.clone();
    let connection = OneBotConnectionContext::new(config.request_timeout);
    let dispatcher = OneBotScopeDispatcher::new(
        &app_config,
        OneBotInboundDispatcher::new(
            respond,
            OneBotSender::new(connection.clone()),
            app_config.bot_display_name().to_owned(),
        ),
        dedupe,
        &shutdown,
    );
    let dispatcher_shutdown = dispatcher.clone();
    let state = ServerState {
        config,
        connection: connection.clone(),
        runtime: runtime.clone(),
        shutdown: shutdown.clone(),
        dispatcher,
    };
    let app = Router::new()
        .route(&path, get(upgrade_websocket))
        .with_state(state);

    info!(%local_addr, path = %path, "OneBot 11 reverse WebSocket listening");
    runtime.record_onebot_listening();
    let task = tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
            })
            .await
            .context("serve OneBot 11 reverse WebSocket");
        dispatcher_shutdown.shutdown().await;
        runtime.record_onebot_stopped();
        result
    });

    Ok(OneBotServerHandle {
        local_addr,
        connection,
        task,
    })
}

async fn upgrade_websocket(
    State(state): State<ServerState>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Response {
    if !authorized(&state.config, &headers) {
        warn!("rejected unauthorized OneBot 11 WebSocket connection");
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let header_self_id = match headers.get(X_SELF_ID) {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| OneBotId::new(value.to_owned()).ok())
        {
            Some(self_id) => Some(self_id),
            None => return (StatusCode::BAD_REQUEST, "invalid x-self-id").into_response(),
        },
        None => None,
    };
    let max_message_bytes = state.config.max_message_bytes;
    websocket
        .max_message_size(max_message_bytes)
        .max_frame_size(max_message_bytes)
        .on_upgrade(move |socket| handle_socket(socket, state, header_self_id))
}

fn authorized(config: &OneBot11Config, headers: &HeaderMap) -> bool {
    let Some(expected) = config.access_token.as_deref() else {
        return false;
    };
    let expected = format!("Bearer {expected}");
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected)
}

async fn handle_socket(
    mut socket: WebSocket,
    state: ServerState,
    header_self_id: Option<OneBotId>,
) {
    let (outbound_tx, mut outbound_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
    let replaced = CancellationToken::new();
    let mut registration = None;
    let mut identity_deadline = Some(Instant::now() + state.config.request_timeout);
    // OneBot 11 heartbeat 在当前接入中是可选能力：连接只上报 self_id 也可长期存活；
    // 仅在收到首个 heartbeat 后，才按客户端声明的 interval 启动连续心跳监督。
    let mut heartbeat_deadline = None;
    let mut disconnect_summary = "client disconnected";

    if let Some(self_id) = header_self_id {
        match register_connection(&state, self_id, outbound_tx.clone(), replaced.clone()) {
            Ok(registered) => {
                registration = Some(registered);
                identity_deadline = None;
            }
            Err(summary) => {
                close_socket(&mut socket, close_code::POLICY, summary).await;
                return;
            }
        }
    }

    loop {
        let next_deadline = identity_deadline.or(heartbeat_deadline);
        tokio::select! {
            biased;
            _ = state.shutdown.cancelled() => {
                disconnect_summary = "gateway shutdown";
                close_socket(&mut socket, close_code::AWAY, "gateway shutdown").await;
                break;
            }
            _ = replaced.cancelled() => {
                disconnect_summary = "replaced by newer connection";
                close_socket(&mut socket, close_code::POLICY, "replaced by newer connection").await;
                break;
            }
            _ = wait_for_deadline(next_deadline), if next_deadline.is_some() => {
                disconnect_summary = if registration.is_some() {
                    "heartbeat timed out"
                } else {
                    "self_id report timed out"
                };
                close_socket(&mut socket, close_code::POLICY, disconnect_summary).await;
                break;
            }
            outbound = outbound_rx.recv() => {
                let Some(payload) = outbound else {
                    disconnect_summary = "outbound channel closed";
                    break;
                };
                if socket.send(Message::Text(payload.into())).await.is_err() {
                    disconnect_summary = "WebSocket send failed";
                    break;
                }
            }
            incoming = socket.recv() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(error) => {
                        // tungstenite 会在分配受限消息体前拒绝超限帧；这里只将错误归类成固定摘要，
                        // 不把可能携带连接细节的底层错误文本写入运行状态。
                        disconnect_summary = if error
                            .to_string()
                            .to_ascii_lowercase()
                            .contains("message too long")
                        {
                            "message too large"
                        } else {
                            "WebSocket receive failed"
                        };
                        break;
                    }
                };
                match handle_message(
                    &mut socket,
                    &state,
                    message,
                    &outbound_tx,
                    &replaced,
                    &mut registration,
                    &mut identity_deadline,
                    &mut heartbeat_deadline,
                ).await {
                    MessageOutcome::Continue => {}
                    MessageOutcome::Disconnect(summary) => {
                        disconnect_summary = summary;
                        break;
                    }
                }
            }
        }
    }

    if let Some(registration) = registration
        && state.connection.unregister(registration.generation)
    {
        state.runtime.record_onebot_disconnected(disconnect_summary);
        info!(reason = disconnect_summary, "OneBot 11 client disconnected");
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    socket: &mut WebSocket,
    state: &ServerState,
    message: Message,
    outbound: &mpsc::Sender<String>,
    replaced: &CancellationToken,
    registration: &mut Option<Registration>,
    identity_deadline: &mut Option<Instant>,
    heartbeat_deadline: &mut Option<Instant>,
) -> MessageOutcome {
    let text = match message {
        Message::Text(text) => text,
        Message::Ping(payload) => {
            if socket.send(Message::Pong(payload)).await.is_err() {
                return MessageOutcome::Disconnect("WebSocket pong failed");
            }
            return MessageOutcome::Continue;
        }
        Message::Pong(_) => return MessageOutcome::Continue,
        Message::Close(_) => return MessageOutcome::Disconnect("client closed connection"),
        Message::Binary(_) => {
            close_socket(socket, close_code::UNSUPPORTED, "text frames required").await;
            return MessageOutcome::Disconnect("unsupported binary frame");
        }
    };
    if text.len() > state.config.max_message_bytes {
        close_socket(socket, close_code::SIZE, "message too large").await;
        return MessageOutcome::Disconnect("message too large");
    }
    let value: Value = match serde_json::from_str(text.as_str()) {
        Ok(value) => value,
        Err(_) => {
            warn!("closing OneBot 11 connection after invalid JSON");
            close_socket(socket, close_code::INVALID, "invalid JSON").await;
            return MessageOutcome::Disconnect("invalid JSON");
        }
    };
    if value.get("echo").is_some() && value.get("retcode").is_some() {
        let response = match serde_json::from_value::<ActionResponse>(value) {
            Ok(response) => response,
            Err(_) => {
                close_socket(socket, close_code::INVALID, "invalid API response").await;
                return MessageOutcome::Disconnect("invalid API response");
            }
        };
        let Some(registration) = registration.as_ref() else {
            debug!("ignoring OneBot API response before self_id registration");
            return MessageOutcome::Continue;
        };
        state
            .connection
            .dispatch_response(registration.generation, response);
        return MessageOutcome::Continue;
    }
    let event = match serde_json::from_value::<OneBotEvent>(value) {
        Ok(event) => event,
        Err(_) => {
            close_socket(socket, close_code::INVALID, "invalid OneBot event").await;
            return MessageOutcome::Disconnect("invalid OneBot event");
        }
    };

    if registration.is_none() {
        match register_connection(
            state,
            event.self_id.clone(),
            outbound.clone(),
            replaced.clone(),
        ) {
            Ok(registered) => {
                *registration = Some(registered);
                *identity_deadline = None;
            }
            Err(summary) => {
                close_socket(socket, close_code::POLICY, summary).await;
                return MessageOutcome::Disconnect(summary);
            }
        }
    } else if state.connection.connected_self_id().as_ref() != Some(&event.self_id) {
        close_socket(socket, close_code::POLICY, "self_id changed").await;
        return MessageOutcome::Disconnect("self_id changed");
    }

    if event.is_heartbeat() {
        state.runtime.record_onebot_heartbeat();
        if let Some(interval_ms) = event.interval {
            let heartbeat_budget = Duration::from_millis(interval_ms.saturating_mul(2))
                .max(state.config.request_timeout);
            *heartbeat_deadline = Some(Instant::now() + heartbeat_budget);
        }
    } else if event.is_lifecycle() {
        debug!(sub_type = ?event.sub_type, "received OneBot 11 lifecycle event");
    } else {
        match inbound_from_event(&event) {
            OneBotInboundOutcome::Message(inbound) => {
                debug!(
                    conversation = inbound.conversation.kind(),
                    text_parts = inbound.input_parts.len(),
                    mentions = inbound.mentions.len(),
                    "adapted OneBot 11 inbound message"
                );
                // 这里只做去重和有界 `try_send`。Core / sender 在 scope worker 中执行，
                // 因而 action echo 仍可由当前读循环继续接收和分派，不会形成自锁。
                match state.dispatcher.enqueue(*inbound) {
                    Ok(OneBotEnqueueOutcome::Accepted) => {}
                    Ok(OneBotEnqueueOutcome::Duplicate) => {}
                    Err(OneBotEnqueueError::Shutdown) => {
                        debug!("ignored OneBot 11 inbound message during gateway shutdown");
                    }
                    Err(error) => {
                        // scope dispatcher 已记录脱敏的容量原因；这里保留协议入口级结果，
                        // 不打印消息正文、message_id 或原始平台标识。
                        warn!(error = %error, "OneBot 11 inbound enqueue failed");
                    }
                }
            }
            OneBotInboundOutcome::Ignored(reason) => debug!(
                reason = reason.as_str(),
                "ignored OneBot 11 event before core dispatch"
            ),
        }
    }
    MessageOutcome::Continue
}

fn register_connection(
    state: &ServerState,
    self_id: OneBotId,
    outbound: mpsc::Sender<String>,
    replaced: CancellationToken,
) -> Result<Registration, &'static str> {
    let masked = mask_identifier(self_id.as_str());
    match state.connection.register(self_id, outbound, replaced) {
        Ok(registration) => {
            state
                .runtime
                .record_onebot_connected(masked, registration.replaced_existing);
            info!(
                self_id = %state.runtime.snapshot().onebot_self_id_summary.as_deref().unwrap_or("unknown"),
                replaced_existing = registration.replaced_existing,
                "OneBot 11 client connected"
            );
            Ok(registration)
        }
        Err(RegistrationError::AccountMismatch { expected }) => {
            warn!(
                expected = %mask_identifier(expected.as_str()),
                received = %masked,
                "rejected OneBot 11 connection for a different account"
            );
            Err("different self_id is not allowed")
        }
        Err(RegistrationError::StateUnavailable) => Err("connection state unavailable"),
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn close_socket(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

enum MessageOutcome {
    Continue,
    Disconnect(&'static str),
}
