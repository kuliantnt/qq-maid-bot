//! gateway WebSocket 协议与 envelope 分发层。
//!
//! 这里保留连接生命周期、鉴权、心跳和平台事件分发；
//! 具体的 C2C / Group 业务处理继续交给上层消息处理函数。

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{MissedTickBehavior, interval};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::{
    BotOutboundCache, GroupCooldowns, MessageCache, handle_c2c_message, handle_group_message,
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::QqApiClient,
    auth::AccessTokenManager,
    config::{AppConfig, GroupMessageMode},
    dedupe::MessageDedupe,
    event::{
        EVENT_C2C_MESSAGE_CREATE, EVENT_GROUP_AT_MESSAGE_CREATE, EVENT_GROUP_MESSAGE_CREATE,
        GatewayEnvelope, parse_c2c_message, parse_group_message,
    },
    respond::RespondClient,
};

const OP_DISPATCH: u64 = 0;
const OP_HEARTBEAT: u64 = 1;
const OP_IDENTIFY: u64 = 2;
const OP_RESUME: u64 = 6;
const OP_RECONNECT: u64 = 7;
const OP_INVALID_SESSION: u64 = 9;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;

const C2C_MESSAGE_INTENTS: u64 = 1 << 25;
const GROUP_MESSAGE_INTENTS: u64 = 1 << 28;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
struct GatewayUrlResponse {
    url: String,
}

#[derive(Debug, Default)]
pub(super) struct ResumeState {
    pub(super) session_id: Option<String>,
    pub(super) seq: Option<u64>,
}

pub(super) fn reconnect_delay() -> Duration {
    RECONNECT_DELAY
}

pub(super) async fn fetch_gateway_url(
    client: &reqwest::Client,
    config: &AppConfig,
    auth: &AccessTokenManager,
) -> anyhow::Result<String> {
    let response = client
        .get(format!("{}/gateway", config.api_base))
        .header("Authorization", auth.authorization_header().await?)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("QQ gateway endpoint returned {status}"));
    }

    let gateway = response.json::<GatewayUrlResponse>().await?;
    if gateway.url.trim().is_empty() {
        return Err(anyhow!("QQ gateway endpoint returned empty url"));
    }
    Ok(gateway.url)
}

// Gateway 主循环需要同时持有配置、鉴权、API 客户端、去重、缓存和恢复状态；
// 这些对象生命周期不同，保持显式参数可以避免把运行期状态装进含糊的大结构。
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_gateway_once(
    gateway_url: &str,
    config: &AppConfig,
    auth: &AccessTokenManager,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    reply_cache: &mut MessageCache,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &mut GroupCooldowns,
    runtime: &GatewayRuntimeStatus,
    resume: &mut ResumeState,
) -> anyhow::Result<()> {
    info!(
        resume = resume.session_id.is_some() && resume.seq.is_some(),
        "connecting QQ gateway websocket"
    );
    let (stream, _) = connect_async(gateway_url).await?;
    info!("QQ gateway websocket connected");
    runtime.record_gateway_connected();
    let (mut write, mut read) = stream.split();

    let hello = read_next_envelope(&mut read)
        .await?
        .ok_or_else(|| anyhow!("gateway closed before hello"))?;
    if hello.op != OP_HELLO {
        return Err(anyhow!(
            "expected gateway hello op {OP_HELLO}, got {}",
            hello.op
        ));
    }
    let heartbeat_interval = hello
        .d
        .get("heartbeat_interval")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(45));
    debug!(
        heartbeat_interval_ms = heartbeat_interval.as_millis(),
        "QQ gateway hello received"
    );

    send_identify_or_resume(&mut write, auth, config, resume).await?;
    let mut heartbeat = interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let payload = json!({"op": OP_HEARTBEAT, "d": resume.seq});
                send_json(&mut write, &payload).await?;
            }
            message = read.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let message = message?;
                match message {
                    Message::Text(text) => {
                        let envelope = serde_json::from_str::<GatewayEnvelope>(&text)?;
                        handle_envelope(
                            envelope,
                            config,
                            auth,
                            respond,
                            api,
                            dedupe,
                            reply_cache,
                            group_outbound_cache,
                            group_cooldowns,
                            runtime,
                            resume,
                            &mut write,
                        )
                        .await?;
                    }
                    Message::Binary(bytes) => {
                        let envelope = serde_json::from_slice::<GatewayEnvelope>(&bytes)?;
                        handle_envelope(
                            envelope,
                            config,
                            auth,
                            respond,
                            api,
                            dedupe,
                            reply_cache,
                            group_outbound_cache,
                            group_cooldowns,
                            runtime,
                            resume,
                            &mut write,
                        )
                        .await?;
                    }
                    Message::Ping(payload) => {
                        write.send(Message::Pong(payload)).await?;
                    }
                    Message::Close(frame) => {
                        debug!(?frame, "gateway sent close frame");
                        return Ok(());
                    }
                    Message::Pong(_) => {}
                    Message::Frame(_) => {}
                }
            }
        }
    }
}

// envelope 分发层直接承接 websocket 写端和 gateway 运行状态，参数较多但职责仍局限在平台事件分发。
#[allow(clippy::too_many_arguments)]
async fn handle_envelope<S>(
    envelope: GatewayEnvelope,
    config: &AppConfig,
    auth: &AccessTokenManager,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    reply_cache: &mut MessageCache,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &mut GroupCooldowns,
    runtime: &GatewayRuntimeStatus,
    resume: &mut ResumeState,
    write: &mut S,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    if let Some(seq) = envelope.s {
        resume.seq = Some(seq);
    }

    match envelope.op {
        OP_DISPATCH => {
            if envelope.t.as_deref() == Some("READY") {
                resume.session_id = envelope
                    .d
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                info!(
                    session_id_present = resume.session_id.is_some(),
                    "QQ gateway ready"
                );
                runtime.record_ready();
                return Ok(());
            }

            if envelope.t.as_deref() == Some("RESUMED") {
                info!(seq = ?resume.seq, "QQ gateway session resumed");
                runtime.record_resumed();
                return Ok(());
            }

            if envelope.t.as_deref() == Some(EVENT_C2C_MESSAGE_CREATE) {
                match parse_c2c_message(&envelope) {
                    Ok(Some(message)) => {
                        if let Err(err) = handle_c2c_message(
                            message,
                            config,
                            auth,
                            respond,
                            api,
                            dedupe,
                            reply_cache,
                            runtime,
                        )
                        .await
                        {
                            warn!(error = %err, "failed to handle C2C message");
                        }
                    }
                    Ok(None) => {}
                    Err(err) => warn!(error = %err, "failed to parse C2C event"),
                }
            } else if matches!(
                envelope.t.as_deref(),
                Some(EVENT_GROUP_AT_MESSAGE_CREATE | EVENT_GROUP_MESSAGE_CREATE)
            ) {
                match parse_group_message(&envelope) {
                    Ok(Some(message)) => {
                        if let Err(err) = handle_group_message(
                            message,
                            config,
                            respond,
                            api,
                            dedupe,
                            reply_cache,
                            group_outbound_cache,
                            group_cooldowns,
                            runtime,
                        )
                        .await
                        {
                            warn!(error = %err, "failed to handle group message");
                        }
                    }
                    Ok(None) => {}
                    Err(err) => warn!(error = %err, "failed to parse group event"),
                }
            } else {
                debug!(
                    event = envelope.t.as_deref().unwrap_or("unknown"),
                    "ignoring gateway dispatch event"
                );
            }
        }
        OP_RECONNECT => {
            warn!("gateway requested reconnect");
            runtime.record_reconnect();
            return Err(anyhow!("gateway requested reconnect"));
        }
        OP_INVALID_SESSION => {
            let can_resume = envelope.d.as_bool().unwrap_or(false);
            runtime.record_invalid_session(can_resume);
            if !can_resume {
                resume.session_id = None;
                resume.seq = None;
            }
            warn!(can_resume, "gateway invalid session");
            send_identify_or_resume(write, auth, config, resume).await?;
        }
        OP_HELLO => {
            debug!("received gateway hello after initial handshake");
        }
        OP_HEARTBEAT_ACK => {
            debug!("gateway heartbeat ack");
            runtime.record_heartbeat_ack();
        }
        _ => {
            debug!(op = envelope.op, "ignoring gateway opcode");
        }
    }

    Ok(())
}

async fn send_identify_or_resume<S>(
    write: &mut S,
    auth: &AccessTokenManager,
    config: &AppConfig,
    resume: &ResumeState,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let token = auth.authorization_header().await?;
    let payload = match (resume.session_id.as_deref(), resume.seq) {
        (Some(session_id), Some(seq)) => {
            info!(seq = seq, "sending QQ gateway resume");
            json!({"op": OP_RESUME, "d": {"token": token, "session_id": session_id, "seq": seq}})
        }
        _ => {
            let intents = gateway_intents(config.group_message_mode);
            info!(intents, "sending QQ gateway identify");
            json!({
                "op": OP_IDENTIFY,
                "d": {
                    "token": token,
                    "intents": intents,
                    "shard": [0, 1],
                    "properties": {
                        "$os": std::env::consts::OS,
                        "$browser": "qq-maid-gateway-rs",
                        "$device": "qq-maid-gateway-rs"
                    }
                }
            })
        }
    };
    send_json(write, &payload).await
}

fn gateway_intents(group_message_mode: GroupMessageMode) -> u64 {
    match group_message_mode {
        GroupMessageMode::Off => C2C_MESSAGE_INTENTS,
        GroupMessageMode::Command | GroupMessageMode::Mention | GroupMessageMode::Active => {
            C2C_MESSAGE_INTENTS | GROUP_MESSAGE_INTENTS
        }
    }
}

async fn send_json<S>(write: &mut S, payload: &Value) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let text = serde_json::to_string(payload)?;
    write.send(Message::Text(text.into())).await?;
    Ok(())
}

async fn read_next_envelope<R>(read: &mut R) -> anyhow::Result<Option<GatewayEnvelope>>
where
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(message) = read.next().await {
        match message.context("read QQ gateway envelope")? {
            Message::Text(text) => return Ok(Some(serde_json::from_str(&text)?)),
            Message::Binary(bytes) => return Ok(Some(serde_json::from_slice(&bytes)?)),
            Message::Close(_) => return Ok(None),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_intents_include_group_when_mode_enabled() {
        assert_eq!(gateway_intents(GroupMessageMode::Off), C2C_MESSAGE_INTENTS);
        assert_eq!(
            gateway_intents(GroupMessageMode::Command),
            C2C_MESSAGE_INTENTS | GROUP_MESSAGE_INTENTS
        );
    }
}
