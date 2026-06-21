//! QQ gateway 运行域。负责 WebSocket 主循环、事件分发、去重、诊断与回发编排。

pub mod dedupe;
pub mod event;
pub mod logging;
pub mod ping;
pub mod push;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{MissedTickBehavior, interval};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use self::{
    dedupe::MessageDedupe,
    event::{
        C2cMessage, EVENT_C2C_MESSAGE_CREATE, EVENT_GROUP_AT_MESSAGE_CREATE,
        EVENT_GROUP_MESSAGE_CREATE, GatewayEnvelope, GroupEventType, GroupMessage,
        parse_c2c_message, parse_group_message,
    },
    logging::{c2c_message_log_summary, group_message_log_summary, mask_openid},
    ping::{
        GatewayRuntimeStatus, build_c2c_ping_reply_with_check_failure, is_ping_check_command,
        is_ping_command,
    },
    push::{PushServerConfig, run_push_server},
};
use crate::{
    api::{
        C2cReplyTarget, GroupOutboundSender, GroupReplyTarget, OutboundSender, QqApiClient,
        SendFuture, send_group_outbound_with_fallback, send_outbound_with_fallback,
    },
    auth::AccessTokenManager,
    config::{AppConfig, GroupMessageMode},
    markdown::MarkdownPayload,
    render::{OutboundMessage, render_respond_response},
    respond::{
        RespondClient, RespondResponse, RespondStream, RespondStreamEvent, RespondTransport,
        build_group_respond_content, build_respond_content, respond_error_to_qq_text,
        respond_not_ok_to_qq_text, respond_response_error_summary,
    },
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
const DEDUPE_TTL: Duration = Duration::from_secs(10 * 60);
const GROUP_COOLDOWN: Duration = Duration::from_secs(3);
const GROUP_USER_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct GatewayUrlResponse {
    url: String,
}

#[derive(Debug, Default)]
struct ResumeState {
    session_id: Option<String>,
    seq: Option<u64>,
}

type MessageCache = HashMap<String, String>;

#[derive(Debug, Default)]
pub(crate) struct BotOutboundCache {
    message_ids: HashSet<String>,
}

impl BotOutboundCache {
    pub(crate) fn insert(&mut self, message_id: Option<String>) {
        if let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) {
            self.message_ids.insert(message_id);
        }
    }

    pub(crate) fn contains(&self, message_id: &str) -> bool {
        self.message_ids.contains(message_id)
    }
}

#[derive(Debug, Default)]
struct GroupCooldowns {
    groups: HashMap<String, Instant>,
    users: HashMap<String, Instant>,
}

impl GroupCooldowns {
    fn check_and_mark(&mut self, message: &GroupMessage, now: Instant) -> bool {
        self.retain(now);
        let user_key = group_user_key(message);
        if self
            .groups
            .get(&message.group_openid)
            .is_some_and(|last| now.duration_since(*last) < GROUP_COOLDOWN)
            || self
                .users
                .get(&user_key)
                .is_some_and(|last| now.duration_since(*last) < GROUP_USER_COOLDOWN)
        {
            return false;
        }
        self.groups.insert(message.group_openid.clone(), now);
        self.users.insert(user_key, now);
        true
    }

    fn retain(&mut self, now: Instant) {
        self.groups
            .retain(|_, last| now.duration_since(*last) <= GROUP_COOLDOWN);
        self.users
            .retain(|_, last| now.duration_since(*last) <= GROUP_USER_COOLDOWN);
    }
}

/// Signal Layer 只是 gateway 内部的临时语义增强层，不是业务核心。
/// 这里只维护一个短时 `message_id -> content` 缓存，用于 reply.content 本地回填。
/// gateway 不负责 prompt 构建；真正发往 `/v1/respond` 的字符串统一在 respond.rs 的 Egress 层生成。
fn resolve_signals(message: &mut C2cMessage, cache: &mut MessageCache) {
    if !message.message_id.trim().is_empty() {
        cache.insert(message.message_id.clone(), message.content.clone());
    }

    let Some(reply) = message.reply.as_mut() else {
        return;
    };
    if reply.content.is_some() || reply.message_id.trim().is_empty() {
        return;
    }
    if let Some(content) = cache.get(&reply.message_id).cloned() {
        // cache 只用于短时 reply 回填，不在 gateway 内承载更高层业务语义。
        reply.content = Some(content);
    }
}
/// QQ 网关主循环：初始化所有共享组件后，反复获取网关地址并建立 WebSocket 连接。
/// 连接断开或失败后会等待 `RECONNECT_DELAY` 后重连，从而保证长期在线。
pub async fn run(config: AppConfig) -> anyhow::Result<()> {
    let http_client = reqwest::Client::new();
    let auth = AccessTokenManager::new(
        http_client.clone(),
        config.app_id.clone(),
        config.app_secret.clone(),
        config.token_refresh_margin,
    );
    let respond = RespondClient::new(http_client.clone(), config.respond_url.clone());
    let api = QqApiClient::new(http_client.clone(), config.api_base.clone(), auth.clone());
    // 消息去重器，用于防止短时间内重复处理同一条 C2C 消息
    let dedupe = MessageDedupe::new(DEDUPE_TTL);
    // 运行时状态，记录网关连接、收发消息等统计信息，供 /ping 等命令使用
    let runtime = GatewayRuntimeStatus::new();
    let group_outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    if config.push_enabled {
        let push_config = PushServerConfig {
            host: config.push_host.clone(),
            port: config.push_port,
            token: config.push_token.clone(),
        };
        let push_api = api.clone();
        let push_runtime = runtime.clone();
        let push_cache = group_outbound_cache.clone();
        tokio::spawn(async move {
            if let Err(err) = run_push_server(push_config, push_api, push_runtime, push_cache).await
            {
                warn!(error = %err, "gateway internal push server stopped");
            }
        });
    }
    // reply 只需要一个极简 HashMap 缓存，不引入额外抽象层或持久化。
    let mut reply_cache = HashMap::new();
    let mut group_cooldowns = GroupCooldowns::default();
    // 断线续连所需的状态（session_id + seq）
    let mut resume = ResumeState::default();

    loop {
        info!(api_base = %config.api_base, "fetching QQ gateway url");
        // 每次重连前重新获取网关地址，避免 IP/调度发生变化后仍连旧地址
        let gateway_url = match fetch_gateway_url(&http_client, &config, &auth).await {
            Ok(url) => {
                info!("fetched QQ gateway url");
                url
            }
            Err(err) => {
                warn!(error = %err, "failed to fetch QQ gateway url");
                return Err(err).context("fetch QQ gateway url");
            }
        };

        match run_gateway_once(
            &gateway_url,
            &config,
            &auth,
            &respond,
            &api,
            &dedupe,
            &mut reply_cache,
            &group_outbound_cache,
            &mut group_cooldowns,
            &runtime,
            &mut resume,
        )
        .await
        {
            // 正常关闭不算错误，但需要重连
            Ok(()) => warn!("QQ gateway connection closed; reconnecting"),
            // 异常断开也要重连
            Err(err) => warn!(error = %err, "QQ gateway connection failed; reconnecting"),
        }

        // 等待一段时间再重连，避免频繁重试给服务端带来压力
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn fetch_gateway_url(
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
async fn run_gateway_once(
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

// 群消息链路同样需要显式串起 QQ 回复、LLM 调用、去重、冷却和运行状态；
// 这里沿用私聊分支的做法保留展开参数，避免把跨层依赖藏进临时聚合对象。
#[allow(clippy::too_many_arguments)]
async fn handle_group_message(
    message: GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &mut GroupCooldowns,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content = build_group_respond_content(&message);
    if should_ignore_group_message(&message, &respond_content, &masked_group) {
        return Ok(());
    }
    if dedupe.is_duplicate(&message.message_id) {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            "duplicate group message ignored"
        );
        return Ok(());
    }
    if !should_process_group_message(config.group_message_mode, &message, group_outbound_cache) {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            event_type = message.event_type.as_respond_event_type(),
            mode = ?config.group_message_mode,
            "group message ignored by mode policy"
        );
        return Ok(());
    }
    if message.event_type == GroupEventType::GroupMessage
        && !group_cooldowns.check_and_mark(&message, Instant::now())
    {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
            "group message ignored by cooldown"
        );
        return Ok(());
    }

    info!(
        message_id = %message.message_id,
        group = %masked_group,
        "calling respond backend for group"
    );
    let transport = match respond.respond_group(&message, respond_content).await {
        Ok(transport) => {
            runtime.record_respond_success();
            transport
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let qq_text = respond_error_to_qq_text(&err);
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %err.log_summary(),
                local_fallback = true,
                fallback_reason = "respond_error",
                qq_error_text = %qq_text,
                "respond backend call failed; sending local group fallback"
            );
            let sent_message_id = send_group_text_with_status(
                api,
                runtime,
                &message.group_openid,
                Some(&message.message_id),
                &qq_text,
            )
            .await?;
            group_outbound_cache.lock().unwrap().insert(sent_message_id);
            return Ok(());
        }
    };

    match transport {
        RespondTransport::Json(response) => {
            send_group_respond_response(
                api,
                runtime,
                config,
                group_outbound_cache,
                &message,
                &response,
            )
            .await?;
        }
        RespondTransport::Stream(stream) => {
            let response =
                collect_streaming_final_response(&message.message_id, &masked_group, stream).await;
            if let Some(response) = response {
                send_group_respond_response(
                    api,
                    runtime,
                    config,
                    group_outbound_cache,
                    &message,
                    &response,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn send_group_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    response: &RespondResponse,
) -> anyhow::Result<()> {
    if !response.ok {
        let qq_text = respond_not_ok_to_qq_text(response);
        warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            error_summary = %respond_response_error_summary(response),
            qq_error_text = %qq_text,
            "respond backend returned not-ok group response"
        );
        let sent_message_id = send_group_text_with_status(
            api,
            runtime,
            &message.group_openid,
            Some(&message.message_id),
            &qq_text,
        )
        .await?;
        group_outbound_cache.lock().unwrap().insert(sent_message_id);
        return Ok(());
    }
    let Some(outbound) =
        render_respond_response(response, config.enable_markdown, config.enable_image)
    else {
        debug!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "respond backend produced no group reply text"
        );
        return Ok(());
    };
    let sender = RuntimeRecordingGroupSender {
        inner: api,
        runtime,
    };
    let target = GroupReplyTarget {
        group_openid: message.group_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    let sent_message_id = send_group_outbound_with_fallback(&sender, &target, &outbound).await?;
    group_outbound_cache.lock().unwrap().insert(sent_message_id);
    Ok(())
}

async fn collect_streaming_final_response(
    message_id: &str,
    masked_group: &str,
    mut stream: RespondStream,
) -> Option<RespondResponse> {
    let mut buffered_text = String::new();
    while let Some(event) = stream.receiver.recv().await {
        match event {
            RespondStreamEvent::Delta { text } => buffered_text.push_str(&text),
            RespondStreamEvent::Final { response } => {
                return build_streaming_buffered_response(&response, &buffered_text);
            }
        }
    }
    warn!(
        message_id = %message_id,
        group = %masked_group,
        "streaming group respond ended without final response"
    );
    None
}

// 私聊消息处理需要贯穿 QQ 回复、LLM 调用、去重和诊断状态，保持参数显式便于看清跨层依赖。
#[allow(clippy::too_many_arguments)]
async fn handle_c2c_message(
    mut message: C2cMessage,
    config: &AppConfig,
    auth: &AccessTokenManager,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    reply_cache: &mut MessageCache,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    // Ingress 已完成解析；这里固定先走 Signal Layer，再进入 Egress content 构建。
    resolve_signals(&mut message, reply_cache);
    log_c2c_message_received(&message, config.verbose_log);
    runtime.record_c2c_message_received(&message);

    let masked_user = mask_openid(&message.user_openid);
    let respond_content = build_respond_content(&message);
    if respond_content.trim().is_empty() {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "ignoring empty C2C message"
        );
        return Ok(());
    }
    if dedupe.is_duplicate(&message.message_id) {
        info!(
            message_id = %message.message_id,
            user = %masked_user,
            "duplicate C2C message ignored"
        );
        return Ok(());
    }

    if is_ping_command(&message.content) {
        info!(
            message_id = %message.message_id,
            user = %masked_user,
            "local /ping command matched"
        );
        let check_failure = if is_ping_check_command(&message.content) {
            respond.check_upstream().await.err().map(|err| {
                let summary = format!("主动检查失败：{}", err.qq_visible_kind());
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error = %err.log_summary(),
                    "active LLM upstream check request failed"
                );
                summary
            })
        } else {
            None
        };
        let reply = build_c2c_ping_reply_with_check_failure(
            &message,
            config,
            runtime,
            auth,
            check_failure.as_deref(),
        )
        .await;
        let target = C2cReplyTarget {
            user_openid: message.user_openid,
            msg_id: Some(message.message_id),
        };
        let outbound = render_local_ping_reply(reply, config.enable_markdown);
        debug!(
            message_id = target.msg_id.as_deref().unwrap_or(""),
            user = %mask_openid(&target.user_openid),
            reply_len = outbound.fallback_text().chars().count(),
            "preparing local /ping reply"
        );
        let sender = RuntimeRecordingSender {
            inner: api,
            runtime,
        };
        send_outbound_with_fallback(&sender, &target, &outbound)
            .await
            .inspect_err(|err| {
                warn!(
                    message_id = target.msg_id.as_deref().unwrap_or(""),
                    user = %mask_openid(&target.user_openid),
                    error = %err.log_summary(),
                    "local /ping QQ reply send failed"
                );
            })?;
        return Ok(());
    }

    info!(
        message_id = %message.message_id,
        user = %masked_user,
        "calling respond backend"
    );
    let transport = match respond.respond_c2c(&message, respond_content).await {
        Ok(transport) => {
            runtime.record_respond_success();
            transport
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let qq_text = respond_error_to_qq_text(&err);
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                error = %err.log_summary(),
                local_fallback = true,
                fallback_reason = "respond_error",
                qq_error_text = %qq_text,
                "respond backend call failed; sending local QQ fallback"
            );
            send_c2c_text_with_status(
                api,
                runtime,
                &message.user_openid,
                Some(&message.message_id),
                &qq_text,
            )
            .await
            .inspect_err(|send_err| {
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error = %send_err.log_summary(),
                    local_fallback = true,
                    fallback_reason = "respond_error",
                    qq_error_text = %qq_text,
                    "local QQ fallback send failed"
                );
            })?;
            return Ok(());
        }
    };

    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    match transport {
        RespondTransport::Json(response) => {
            if !response.ok {
                let qq_text = respond_not_ok_to_qq_text(&response);
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error_summary = %respond_response_error_summary(&response),
                    qq_error_text = %qq_text,
                    "respond backend returned not-ok response"
                );
                send_c2c_text_with_status(
                    api,
                    runtime,
                    &message.user_openid,
                    Some(&message.message_id),
                    &qq_text,
                )
                .await
                .inspect_err(|send_err| {
                    warn!(
                        message_id = %message.message_id,
                        user = %masked_user,
                        error = %send_err.log_summary(),
                        local_fallback = true,
                        fallback_reason = "respond_not_ok",
                        qq_error_text = %qq_text,
                        "respond not-ok QQ fallback send failed"
                    );
                })?;
                return Ok(());
            }

            let Some(outbound) =
                render_respond_response(&response, config.enable_markdown, config.enable_image)
            else {
                debug!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    "respond backend produced no reply text"
                );
                return Ok(());
            };

            debug!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %mask_openid(&target.user_openid),
                reply_len = outbound.fallback_text().chars().count(),
                "preparing QQ reply"
            );
            let sender = RuntimeRecordingSender {
                inner: api,
                runtime,
            };
            send_outbound_with_fallback(&sender, &target, &outbound)
                .await
                .inspect_err(|err| {
                    warn!(
                        message_id = target.msg_id.as_deref().unwrap_or(""),
                        user = %mask_openid(&target.user_openid),
                        error = %err.log_summary(),
                        "QQ reply send failed"
                    );
                })?;
        }
        RespondTransport::Stream(stream) => {
            handle_streaming_respond_response(api, runtime, &message, &target, config, stream)
                .await?;
        }
    }
    Ok(())
}

fn render_local_ping_reply(reply: String, enable_markdown: bool) -> OutboundMessage {
    if enable_markdown {
        // `/ping` 本地生成的状态报告本身就是 Markdown；发送层复用现有 fallback，
        // 避免 QQ Markdown 权限或平台兼容问题导致诊断消息完全丢失。
        return OutboundMessage::Markdown {
            markdown: MarkdownPayload::new(reply.clone()),
            fallback_text: reply,
        };
    }
    OutboundMessage::Text { text: reply }
}

async fn handle_streaming_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    target: &C2cReplyTarget,
    config: &AppConfig,
    stream: RespondStream,
) -> anyhow::Result<()> {
    // QQ 私聊逐条回发流式 delta 会退化成“一字一条”刷屏。
    // 这里继续保留后端 SSE，以便尽早拿到结果，但 QQ 侧统一等最终文本再发。
    let mut buffered_text = String::new();
    let mut final_response = None;
    let mut stream = stream;
    while let Some(event) = stream.receiver.recv().await {
        match event {
            RespondStreamEvent::Delta { text } => {
                if !text.is_empty() {
                    debug!(
                        message_id = target.msg_id.as_deref().unwrap_or(""),
                        user = %mask_openid(&target.user_openid),
                        delta_len = text.chars().count(),
                        "buffering streaming QQ delta"
                    );
                    buffered_text.push_str(&text);
                }
            }
            RespondStreamEvent::Final { response } => {
                final_response = Some(response);
                break;
            }
        }
    }

    let Some(response) = final_response else {
        warn!(
            message_id = %message.message_id,
            user = %mask_openid(&message.user_openid),
            "streaming respond backend ended without final response"
        );
        return Ok(());
    };

    if !response.ok {
        if let Some(buffered_response) =
            build_streaming_buffered_response(&response, &buffered_text)
        {
            warn!(
                message_id = %message.message_id,
                user = %mask_openid(&message.user_openid),
                error_summary = %respond_response_error_summary(&response),
                reply_len = buffered_response.text.as_deref().map(|text| text.chars().count()).unwrap_or(0),
                "streaming respond finished with error after buffering partial output"
            );
            let Some(outbound) = render_respond_response(
                &buffered_response,
                config.enable_markdown,
                config.enable_image,
            ) else {
                return Ok(());
            };
            let sender = RuntimeRecordingSender {
                inner: api,
                runtime,
            };
            send_outbound_with_fallback(&sender, target, &outbound)
                .await
                .inspect_err(|err| {
                    warn!(
                        message_id = target.msg_id.as_deref().unwrap_or(""),
                        user = %mask_openid(&target.user_openid),
                        error = %err.log_summary(),
                        "streaming buffered QQ reply send failed"
                    );
                })?;
            return Ok(());
        }

        let qq_text = respond_not_ok_to_qq_text(&response);
        warn!(
            message_id = %message.message_id,
            user = %mask_openid(&message.user_openid),
            error_summary = %respond_response_error_summary(&response),
            qq_error_text = %qq_text,
            "streaming respond returned not-ok response"
        );
        send_c2c_text_with_status(
            api,
            runtime,
            &message.user_openid,
            Some(&message.message_id),
            &qq_text,
        )
        .await
        .inspect_err(|send_err| {
            warn!(
                message_id = %message.message_id,
                user = %mask_openid(&message.user_openid),
                error = %send_err.log_summary(),
                local_fallback = true,
                fallback_reason = "streaming_respond_not_ok",
                qq_error_text = %qq_text,
                "streaming respond QQ fallback send failed"
            );
        })?;
        return Ok(());
    }

    let Some(buffered_response) = build_streaming_buffered_response(&response, &buffered_text)
    else {
        debug!(
            message_id = %message.message_id,
            user = %mask_openid(&message.user_openid),
            "streaming respond produced no reply text"
        );
        return Ok(());
    };
    let Some(outbound) = render_respond_response(
        &buffered_response,
        config.enable_markdown,
        config.enable_image,
    ) else {
        debug!(
            message_id = %message.message_id,
            user = %mask_openid(&message.user_openid),
            "streaming respond rendered empty outbound message"
        );
        return Ok(());
    };

    debug!(
        message_id = target.msg_id.as_deref().unwrap_or(""),
        user = %mask_openid(&target.user_openid),
        reply_len = outbound.fallback_text().chars().count(),
        "preparing streaming QQ reply"
    );
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    send_outbound_with_fallback(&sender, target, &outbound)
        .await
        .inspect_err(|err| {
            warn!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %mask_openid(&target.user_openid),
                error = %err.log_summary(),
                "streaming QQ reply send failed"
            );
        })?;
    Ok(())
}

fn should_ignore_group_message(
    message: &GroupMessage,
    respond_content: &str,
    masked_group: &str,
) -> bool {
    if message.author_is_self {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring self group message"
        );
        return true;
    }
    if message.author_is_bot {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring bot group message"
        );
        return true;
    }
    if respond_content.trim().is_empty() {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring empty group message"
        );
        return true;
    }
    false
}

fn should_process_group_message(
    mode: GroupMessageMode,
    message: &GroupMessage,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    if message.event_type == GroupEventType::GroupAtMessage {
        return true;
    }

    match mode {
        GroupMessageMode::Off => false,
        GroupMessageMode::Command => is_group_command(&message.content),
        GroupMessageMode::Mention => {
            is_group_command(&message.content)
                || contains_bot_mention(&message.content)
                || is_reply_to_bot(message, bot_outbound_cache)
        }
        GroupMessageMode::Active => true,
    }
}

fn is_group_command(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with('/') || trimmed.starts_with('／')
}

fn contains_bot_mention(content: &str) -> bool {
    content.contains("CQ:at") || content.contains("<@") || content.contains("@机器人")
}

fn is_reply_to_bot(
    message: &GroupMessage,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    message.reply.as_ref().is_some_and(|reply| {
        bot_outbound_cache
            .lock()
            .unwrap()
            .contains(&reply.message_id)
    })
}

fn group_user_key(message: &GroupMessage) -> String {
    let member = message.member_openid.as_deref().unwrap_or("unknown");
    format!("{}:{member}", message.group_openid)
}

fn gateway_intents(group_message_mode: GroupMessageMode) -> u64 {
    match group_message_mode {
        GroupMessageMode::Off => C2C_MESSAGE_INTENTS,
        GroupMessageMode::Command | GroupMessageMode::Mention | GroupMessageMode::Active => {
            C2C_MESSAGE_INTENTS | GROUP_MESSAGE_INTENTS
        }
    }
}

fn build_streaming_buffered_response(
    response: &RespondResponse,
    buffered_text: &str,
) -> Option<RespondResponse> {
    let text = response
        .text
        .as_ref()
        .filter(|text| !text.trim().is_empty())
        .cloned()
        .or_else(|| (!buffered_text.trim().is_empty()).then_some(buffered_text.to_owned()))?;
    let mut response = response.clone();
    response.text = Some(text);
    Some(response)
}

async fn send_c2c_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    user_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> crate::api::SendResult {
    let result = api.send_c2c_text(user_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

async fn send_group_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    group_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> crate::api::SendResult {
    let result = api.send_group_text(group_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

pub(crate) fn record_qq_send_result(
    runtime: &GatewayRuntimeStatus,
    result: &crate::api::SendResult,
) {
    match result {
        Ok(_) => runtime.record_qq_send_success(),
        Err(err) => runtime.record_qq_send_failure(err.log_summary()),
    }
}

struct RuntimeRecordingSender<'a> {
    inner: &'a QqApiClient,
    runtime: &'a GatewayRuntimeStatus,
}

struct RuntimeRecordingGroupSender<'a> {
    inner: &'a QqApiClient,
    runtime: &'a GatewayRuntimeStatus,
}

impl OutboundSender for RuntimeRecordingSender<'_> {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_text(&target.user_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a crate::markdown::MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown(&target.user_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a crate::media::ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_image(&target.user_openid, target.msg_id.as_deref(), image)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

impl GroupOutboundSender for RuntimeRecordingGroupSender<'_> {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_text(&target.group_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a GroupReplyTarget,
        markdown: &'a crate::markdown::MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_markdown(&target.group_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

fn log_c2c_message_received(message: &C2cMessage, verbose_log: bool) {
    let summary = c2c_message_log_summary(message, verbose_log);
    if let Some(extracted_content) = summary.extracted_content.as_deref() {
        info!(
            message_id = %summary.message_id,
            user = %summary.masked_user,
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            extracted_content = %extracted_content,
            "received C2C message"
        );
    } else {
        info!(
            message_id = %summary.message_id,
            user = %summary.masked_user,
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            "received C2C message"
        );
    }
}

fn log_group_message_received(message: &GroupMessage, verbose_log: bool) {
    let summary = group_message_log_summary(message, verbose_log);
    if let Some(extracted_content) = summary.extracted_content.as_deref() {
        info!(
            message_id = %summary.message_id,
            group = %summary.masked_group,
            member = %summary.masked_member.as_deref().unwrap_or(""),
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            extracted_content = %extracted_content,
            "received group message"
        );
    } else {
        info!(
            message_id = %summary.message_id,
            group = %summary.masked_group,
            member = %summary.masked_member.as_deref().unwrap_or(""),
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            "received group message"
        );
    }
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
        match message? {
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
    use super::event::{C2cMessage, MessageReply};
    use super::*;
    use crate::api::ApiError;

    #[test]
    fn build_streaming_buffered_response_prefers_final_text() {
        let response = RespondResponse {
            ok: true,
            text: Some("最终完整回复".to_owned()),
            markdown: Some("# 最终完整回复".to_owned()),
            handled: Some(true),
            session_id: Some("sess-1".to_owned()),
            command: Some("web_search".to_owned()),
            diagnostics: None,
            error: None,
        };

        let buffered =
            build_streaming_buffered_response(&response, "中间增量").expect("buffered response");

        assert_eq!(buffered.text.as_deref(), Some("最终完整回复"));
        assert_eq!(buffered.markdown.as_deref(), Some("# 最终完整回复"));
        assert_eq!(buffered.command.as_deref(), Some("web_search"));
    }

    #[test]
    fn build_streaming_buffered_response_falls_back_to_buffered_delta_text() {
        let response = RespondResponse {
            ok: true,
            text: None,
            markdown: Some("# 结构化最终回复".to_owned()),
            handled: Some(true),
            session_id: Some("sess-1".to_owned()),
            command: Some("web_search".to_owned()),
            diagnostics: None,
            error: None,
        };

        let buffered = build_streaming_buffered_response(&response, "我会按北京时间整理今天新闻")
            .expect("buffered response");

        assert_eq!(buffered.text.as_deref(), Some("我会按北京时间整理今天新闻"));
        assert_eq!(buffered.markdown.as_deref(), Some("# 结构化最终回复"));
    }

    #[test]
    fn local_ping_reply_respects_markdown_config() {
        let markdown = render_local_ping_reply("# 状态\n\n| A | B |".to_owned(), true);
        assert_eq!(
            markdown,
            OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("# 状态\n\n| A | B |"),
                fallback_text: "# 状态\n\n| A | B |".to_owned(),
            }
        );

        let text = render_local_ping_reply("# 状态".to_owned(), false);
        assert_eq!(
            text,
            OutboundMessage::Text {
                text: "# 状态".to_owned(),
            }
        );
    }

    #[test]
    fn record_qq_send_result_updates_runtime_status() {
        let runtime = GatewayRuntimeStatus::new();
        let success: crate::api::SendResult = Ok(None);

        record_qq_send_result(&runtime, &success);
        let snapshot = runtime.snapshot();
        assert!(snapshot.last_qq_send_success_at.is_some());
        assert_eq!(snapshot.last_qq_send_failure_at, None);

        let failure: crate::api::SendResult = Err(ApiError::Unsupported("text"));
        record_qq_send_result(&runtime, &failure);
        let snapshot = runtime.snapshot();

        assert!(snapshot.last_qq_send_failure_at.is_some());
        assert_eq!(
            snapshot.last_qq_send_failure_summary.as_deref(),
            Some("text sending is unsupported")
        );
    }

    fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("member-1".to_owned()),
            content: content.to_owned(),
            reply: None,
            timestamp: None,
            attachments: Vec::new(),
            event_type,
            author_is_bot: false,
            author_is_self: false,
        }
    }

    #[test]
    fn group_message_mode_policy_matches_triggers() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ordinary = group_message("hello", GroupEventType::GroupMessage);
        let command = group_message("/rss", GroupEventType::GroupMessage);
        let mention = group_message("[CQ:at,qq=123] hello", GroupEventType::GroupMessage);
        let at_event = group_message("hello", GroupEventType::GroupAtMessage);

        assert!(!should_process_group_message(
            GroupMessageMode::Off,
            &ordinary,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Off,
            &at_event,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Command,
            &command,
            &cache
        ));
        assert!(!should_process_group_message(
            GroupMessageMode::Command,
            &mention,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &mention,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Active,
            &ordinary,
            &cache
        ));
    }

    #[test]
    fn reply_to_cached_bot_message_triggers_mention_mode() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        cache.lock().unwrap().insert(Some("bot-msg-1".to_owned()));
        let mut message = group_message("继续", GroupEventType::GroupMessage);
        message.reply = Some(MessageReply {
            message_id: "bot-msg-1".to_owned(),
            content: None,
        });

        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &message,
            &cache
        ));
    }

    #[test]
    fn group_cooldown_blocks_same_group_temporarily() {
        let mut cooldowns = GroupCooldowns::default();
        let message = group_message("hello", GroupEventType::GroupMessage);
        let now = Instant::now();

        assert!(cooldowns.check_and_mark(&message, now));
        assert!(!cooldowns.check_and_mark(&message, now + Duration::from_secs(1)));
        assert!(
            cooldowns.check_and_mark(&message, now + GROUP_USER_COOLDOWN + Duration::from_secs(1))
        );
    }

    #[test]
    fn gateway_intents_include_group_when_mode_enabled() {
        assert_eq!(gateway_intents(GroupMessageMode::Off), C2C_MESSAGE_INTENTS);
        assert_eq!(
            gateway_intents(GroupMessageMode::Command),
            C2C_MESSAGE_INTENTS | GROUP_MESSAGE_INTENTS
        );
    }

    #[tokio::test]
    async fn resolve_signals_fills_known_reply_content() {
        let mut cache = HashMap::new();
        cache.insert("quoted-1".to_owned(), "上一条消息".to_owned());
        let mut message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: None,
            }),
            timestamp: None,
            attachments: Vec::new(),
        };

        resolve_signals(&mut message, &mut cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: Some("上一条消息".to_owned()),
            })
        );
        assert_eq!(cache.get("msg-1").map(String::as_str), Some("你好"));
    }

    #[test]
    fn resolve_signals_keeps_reply_content_none_on_cache_miss() {
        let mut cache = HashMap::new();
        let mut message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: Some(MessageReply {
                message_id: "quoted-missing".to_owned(),
                content: None,
            }),
            timestamp: None,
            attachments: Vec::new(),
        };

        resolve_signals(&mut message, &mut cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-missing".to_owned(),
                content: None,
            })
        );
        assert_eq!(cache.get("msg-1").map(String::as_str), Some("你好"));
    }
}
