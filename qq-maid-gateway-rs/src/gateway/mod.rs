//! QQ gateway 运行域。负责 WebSocket 主循环、事件分发、去重、诊断与回发编排。

mod aggregator;
pub mod dedupe;
mod dispatcher;
pub mod event;
mod group_filter;
pub mod logging;
mod outbound;
pub mod ping;
mod protocol;
pub mod push;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aggregator::MessageAggregator;
use anyhow::Context;
use dispatcher::MessageDispatcher;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use self::{
    dedupe::MessageDedupe,
    event::{C2cMessage, GroupEventType, GroupMessage},
    group_filter::{GroupCooldowns, should_ignore_group_message, should_process_group_message},
    logging::{c2c_message_log_summary, group_message_log_summary, mask_identifier, mask_openid},
    outbound::{
        RuntimeRecordingGroupSender, RuntimeRecordingSender, record_qq_send_result,
        send_c2c_text_with_status, send_group_text_with_status,
    },
    ping::{
        GatewayRuntimeStatus, build_c2c_ping_reply_with_check_failure, is_ping_check_command,
        is_ping_command,
    },
    protocol::ResumeState,
    push::GatewayPushSink,
};
use crate::{
    api::{
        C2cReplyTarget, C2cStreamState, GroupReplyTarget, OutboundSender, QqApiClient,
        StreamSendResult, send_group_outbound_with_fallback, send_outbound_with_fallback,
    },
    auth::AccessTokenManager,
    config::AppConfig,
    markdown::MarkdownPayload,
    render::{OutboundMessage, render_respond_response},
    respond::{
        RespondClient, RespondEvent, RespondResponse, RespondTransport,
        build_group_respond_content, build_respond_content, respond_error_to_qq_text,
    },
};

const DEDUPE_TTL: Duration = Duration::from_secs(10 * 60);

type ReplyCache = Arc<Mutex<HashMap<ReplyCacheKey, String>>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ReplyCacheKey {
    scope_key: String,
    message_id: String,
}

impl ReplyCacheKey {
    fn new(scope_key: String, message_id: impl Into<String>) -> Self {
        Self {
            scope_key,
            message_id: message_id.into(),
        }
    }
}

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

/// Signal Layer 只是 gateway 内部的临时语义增强层，不是业务核心。
/// 这里只维护一个短时 `message_id -> content` 缓存，用于 reply.content 本地回填。
/// gateway 不负责 prompt 构建；真正交给 CoreService 的字符串统一在 respond.rs 的 Egress 层生成。
pub(super) fn resolve_signals(message: &mut C2cMessage, cache: &ReplyCache) {
    let scope_key = crate::respond::scope_key_from_c2c_message(message);
    if !message.message_id.trim().is_empty() {
        cache.lock().unwrap().insert(
            ReplyCacheKey::new(scope_key.clone(), message.message_id.clone()),
            message.content.clone(),
        );
    }

    let Some(reply) = message.reply.as_mut() else {
        return;
    };
    if reply.content.is_some() || reply.message_id.trim().is_empty() {
        return;
    }
    if let Some(content) = cache
        .lock()
        .unwrap()
        .get(&ReplyCacheKey::new(scope_key, reply.message_id.clone()))
        .cloned()
    {
        // cache 只用于短时 reply 回填，不在 gateway 内承载更高层业务语义。
        reply.content = Some(content);
    }
}

fn group_reply_mention_prefix(message: &GroupMessage) -> Option<String> {
    // 只有用户显式 @ 机器人触发的官方群 at 事件，才在回复正文里 @ 回发起人；
    // 普通群命令、关键词触发和回复机器人消息继续只挂原消息 msg_id，避免额外打扰。
    if message.event_type != GroupEventType::GroupAtMessage {
        return None;
    }
    message
        .member_openid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|member_openid| format!("<@{member_openid}>"))
}

fn prefix_group_reply_text(message: &GroupMessage, text: &str) -> String {
    let Some(prefix) = group_reply_mention_prefix(message) else {
        return text.to_owned();
    };
    if text.trim().is_empty() {
        prefix
    } else {
        format!("{prefix}\n{text}")
    }
}

fn prefix_group_reply_outbound(
    message: &GroupMessage,
    outbound: OutboundMessage,
) -> OutboundMessage {
    let Some(prefix) = group_reply_mention_prefix(message) else {
        return outbound;
    };
    outbound.prefix_text(&prefix)
}

fn group_respond_error_texts(
    message: &GroupMessage,
    err: &crate::respond::RespondError,
) -> (String, String) {
    let log_text = respond_error_to_qq_text(err);
    // 群 at fallback 的实际 QQ 文本需要保留 <@openid>，但日志字段只能使用未加前缀的安全文案。
    let qq_text = prefix_group_reply_text(message, &log_text);
    (qq_text, log_text)
}
/// QQ 网关主循环：初始化所有共享组件后，反复获取网关地址并建立 WebSocket 连接。
/// 连接断开或失败后会等待 `RECONNECT_DELAY` 后重连，从而保证长期在线。
pub async fn run(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
    shutdown_token: CancellationToken,
) -> anyhow::Result<()> {
    let http_client = reqwest::Client::new();
    let auth = AccessTokenManager::new(
        http_client.clone(),
        config.app_id.clone(),
        config.app_secret.clone(),
        config.token_refresh_margin,
    );
    let api = QqApiClient::new(http_client.clone(), config.api_base.clone(), auth.clone());
    // 消息去重器，用于防止短时间内重复处理同一条 C2C 消息
    let dedupe = Arc::new(MessageDedupe::new(DEDUPE_TTL));
    // 运行时状态，记录网关连接、收发消息等统计信息，供 /ping 等命令使用
    let runtime = GatewayRuntimeStatus::new();
    let group_outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    // 主动推送已经进程内化；Core 通过 PushSink 进入这里，仍由 Gateway 负责 QQ 发送。
    push_sink.bind(api.clone(), runtime.clone(), group_outbound_cache.clone());
    // reply 只需要一个极简 HashMap 缓存，不引入额外抽象层或持久化。
    let reply_cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
    let group_cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
    // 断线续连所需的状态（session_id + seq）
    let mut resume = ResumeState::default();
    // 聚合器必须先 flush 到 Dispatcher，不能让全局 shutdown 同时取消两者。
    // 顶层 run 负责在停止接收新 Gateway 入站后，按 aggregator -> dispatcher 的顺序关闭。
    let dispatcher_shutdown = CancellationToken::new();
    let aggregator_shutdown = CancellationToken::new();
    let dispatcher = MessageDispatcher::new(
        config.clone(),
        auth.clone(),
        respond.clone(),
        api.clone(),
        dedupe.clone(),
        reply_cache.clone(),
        group_outbound_cache.clone(),
        group_cooldowns.clone(),
        runtime.clone(),
        dispatcher_shutdown,
    );
    let dispatcher_handle = dispatcher.handle();
    let aggregator = MessageAggregator::new(
        config.clone(),
        respond.clone(),
        dispatcher_handle,
        dedupe.clone(),
        reply_cache.clone(),
        aggregator_shutdown,
    );
    let aggregator_handle = aggregator.handle();

    loop {
        if shutdown_token.is_cancelled() {
            break;
        }
        info!(api_base = %config.api_base, "fetching QQ gateway url");
        // 每次重连前重新获取网关地址，避免 IP/调度发生变化后仍连旧地址
        let gateway_url = match tokio::select! {
            _ = shutdown_token.cancelled() => break,
            result = protocol::fetch_gateway_url(&http_client, &config, &auth) => result,
        } {
            Ok(url) => {
                info!("fetched QQ gateway url");
                url
            }
            Err(err) => {
                warn!(error = %err, "failed to fetch QQ gateway url");
                return Err(err).context("fetch QQ gateway url");
            }
        };

        match protocol::run_gateway_once(
            &gateway_url,
            &config,
            &auth,
            &runtime,
            &mut resume,
            aggregator_handle.clone(),
            shutdown_token.clone(),
        )
        .await
        {
            // 正常关闭不算错误，但需要重连
            Ok(()) => warn!("QQ gateway connection closed; reconnecting"),
            // 异常断开也要重连
            Err(err) => warn!(error = %err, "QQ gateway connection failed; reconnecting"),
        }

        // 等待一段时间再重连，避免频繁重试给服务端带来压力
        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            _ = tokio::time::sleep(protocol::reconnect_delay()) => {}
        }
    }

    aggregator.shutdown().await;
    dispatcher.shutdown().await;
    Ok(())
}

// 群消息链路同样需要显式串起 QQ 回复、LLM 调用、去重、冷却和运行状态；
// 这里沿用私聊分支的做法保留展开参数，避免把跨层依赖藏进临时聚合对象。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_group_message(
    message: GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &Arc<Mutex<GroupCooldowns>>,
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
    if !should_process_group_message(
        config.group_message_mode,
        &config.group_active_keywords,
        &message,
        group_outbound_cache,
    ) {
        let active_keyword_count = config.group_active_keywords.len();
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            event_type = message.event_type.as_respond_event_type(),
            mode = ?config.group_message_mode,
            active_keyword_count,
            "group message ignored by mode policy"
        );
        return Ok(());
    }
    if message.event_type == GroupEventType::GroupMessage
        && !group_cooldowns
            .lock()
            .unwrap()
            .check_and_mark(&message, Instant::now())
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
        Ok(response) => {
            runtime.record_respond_success();
            response
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let (qq_text, log_text) = group_respond_error_texts(&message, &err);
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %err.log_summary(),
                local_fallback = true,
                fallback_reason = "respond_error",
                qq_error_text = %log_text,
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
        RespondTransport::Complete(response) => {
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
            if let Some(response) = consume_respond_stream(stream).await {
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

pub(super) async fn send_group_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    response: &RespondResponse,
) -> anyhow::Result<()> {
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
    let outbound = prefix_group_reply_outbound(message, outbound);
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

pub(super) async fn consume_respond_stream(
    mut stream: qq_maid_core::service::CoreResponseStream,
) -> Option<RespondResponse> {
    while let Some(event) = stream.recv().await {
        match event {
            RespondEvent::TextDelta(_) => {}
            RespondEvent::Completed(response) => return Some(response),
            RespondEvent::Failed(failure) => {
                warn!(
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    "core respond stream failed"
                );
                return None;
            }
        }
    }
    None
}

/// QQ C2C 流式发送的节流间隔（毫秒）。
///
/// 避免每个 LLM delta 都请求一次 QQ API，减少接口压力。
const STREAM_THROTTLE_MS: u64 = 500;

type RespondEventFuture<'a> = Pin<Box<dyn Future<Output = Option<RespondEvent>> + Send + 'a>>;
type StreamSendFuture<'a> = Pin<Box<dyn Future<Output = StreamSendResult> + Send + 'a>>;

/// Core 流事件来源抽象，仅用于把 C2C 流式状态机与真实 Core channel 解耦，便于覆盖异常分支。
trait RespondEventStream: Send {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a>;
}

impl RespondEventStream for qq_maid_core::service::CoreResponseStream {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
        Box::pin(async move { self.recv().await })
    }
}

/// C2C 流式发送抽象；普通消息能力复用 `OutboundSender`，确保 Pending fallback 走同一发送链路。
trait C2cStreamSender: OutboundSender {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a>;
}

impl C2cStreamSender for RuntimeRecordingSender<'_> {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown_stream(
                    user_openid,
                    msg_id,
                    markdown,
                    stream_state,
                    stream_state_value,
                    reset,
                )
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

#[derive(Debug)]
enum C2cStreamingPhase {
    Pending(C2cStreamState),
    Active(C2cStreamState),
    BrokenActive(C2cStreamState),
    Completed,
}

impl C2cStreamingPhase {
    fn name(&self) -> &'static str {
        match self {
            Self::Pending(_) => "pending",
            Self::Active(_) => "active",
            Self::BrokenActive(_) => "broken_active",
            Self::Completed => "completed",
        }
    }
}

/// 发送 C2C 普通（非流式）回复消息，供真实网关入口调用。
async fn send_c2c_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    send_c2c_respond_response_with_sender(&sender, message, response, config).await
}

/// 普通 C2C 回复发送的共享实现。
///
/// 流式 fallback 必须走这里，才能保留 Markdown、文本 fallback、图片开关、reply target
/// 以及发送状态记录等既有语义。
async fn send_c2c_respond_response_with_sender<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let masked_user = mask_openid(&message.user_openid);
    let Some(outbound) =
        render_respond_response(response, config.enable_markdown, config.enable_image)
    else {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "respond backend produced no reply text"
        );
        return Ok(());
    };

    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    debug!(
        message_id = target.msg_id.as_deref().unwrap_or(""),
        user = %masked_user,
        reply_len = outbound.fallback_text().chars().count(),
        "preparing QQ reply"
    );
    send_outbound_with_fallback(sender, &target, &outbound)
        .await
        .inspect_err(|err| {
            warn!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %masked_user,
                error = %err.log_summary(),
                "QQ reply send failed"
            );
        })?;
    Ok(())
}

/// QQ C2C 流式响应处理。
///
/// 在 QQ 官方机器人 C2C 私聊中，将 Core 流式响应接入 QQ 流式消息接口，
/// 让同一条消息在生成过程中持续更新。
///
/// # Fallback 行为
///
/// - `Pending` 首帧尚未成功时，Completed 后最多发送一次普通 C2C 回复。
/// - 一旦首帧成功进入 `Active`，本轮用户可见回复只归流式发送器所有；中间帧或最终帧失败只记录错误并保持 `BrokenActive`，禁止再发完整普通正文。
async fn stream_respond_c2c(
    stream: qq_maid_core::service::CoreResponseStream,
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    stream_respond_c2c_with_sender(stream, &sender, message, config).await
}

async fn stream_respond_c2c_with_sender<E, S>(
    mut stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<()>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    let user_openid = &message.user_openid;
    let masked_user = mask_openid(user_openid);
    let reply_msg_id = &message.message_id;
    let masked_reply_msg_id = mask_identifier(reply_msg_id);
    let mut phase = C2cStreamingPhase::Pending(C2cStreamState {
        stream_id: None,
        index: 0,
    });
    let mut accumulated = String::new();
    // QQ stream 的 reset=false 是“续接本次 Markdown content”，不能反复提交全文。
    // 结束包以 done=true + state=10 关闭流；项目内未发现终包 reset/index 的协议依据，因此不额外携带。
    let mut pending_delta = String::new();
    let mut last_send_at = Instant::now();
    let mut stream_first_attempted = false;

    while let Some(event) = stream.recv_event().await {
        match event {
            RespondEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                accumulated.push_str(&delta);
                pending_delta.push_str(&delta);

                match phase {
                    C2cStreamingPhase::Pending(mut stream_state) => {
                        if stream_first_attempted {
                            phase = C2cStreamingPhase::Pending(stream_state);
                            continue;
                        }
                        stream_first_attempted = true;
                        let index = stream_state.index;
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_chunk(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            &pending_delta,
                            &mut stream_state,
                            1,
                            false,
                        )
                        .await
                        {
                            Ok(Some(_)) => {
                                let content_chars = pending_delta.chars().count();
                                pending_delta.clear();
                                last_send_at = Instant::now();
                                info!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "active",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    content_chars,
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send succeeded"
                                );
                                phase = C2cStreamingPhase::Active(stream_state);
                            }
                            Ok(None) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = false,
                                    content_chars = pending_delta.chars().count(),
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send returned no stream id; ordinary reply remains allowed on Completed"
                                );
                                phase = C2cStreamingPhase::Pending(stream_state);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    content_chars = pending_delta.chars().count(),
                                    error = %err.log_summary(),
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send failed; ordinary reply remains allowed on Completed"
                                );
                                phase = C2cStreamingPhase::Pending(stream_state);
                            }
                        }
                    }
                    C2cStreamingPhase::Active(mut stream_state) => {
                        let elapsed = last_send_at.elapsed();
                        if elapsed >= Duration::from_millis(STREAM_THROTTLE_MS)
                            && !pending_delta.is_empty()
                        {
                            let chunk = pending_delta.clone();
                            let index = stream_state.index;
                            let had_stream_id = stream_state.stream_id.is_some();
                            match send_stream_chunk(
                                sender,
                                user_openid,
                                Some(reply_msg_id),
                                &chunk,
                                &mut stream_state,
                                1,
                                false,
                            )
                            .await
                            {
                                Ok(_) => {
                                    pending_delta.clear();
                                    last_send_at = Instant::now();
                                    debug!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "middle_chunk",
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        sent_len = accumulated.len(),
                                        chunk_chars = chunk.chars().count(),
                                        "QQ stream middle send succeeded"
                                    );
                                    phase = C2cStreamingPhase::Active(stream_state);
                                }
                                Err(err) => {
                                    warn!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "middle_chunk",
                                        stream_state = "broken_active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        content_chars = chunk.chars().count(),
                                        error = %err.log_summary(),
                                        accumulated_chars = accumulated.chars().count(),
                                        "QQ stream middle send failed; ordinary fallback is disabled after stream id was created"
                                    );
                                    phase = C2cStreamingPhase::BrokenActive(stream_state);
                                }
                            }
                        } else {
                            phase = C2cStreamingPhase::Active(stream_state);
                        }
                    }
                    C2cStreamingPhase::BrokenActive(stream_state) => {
                        phase = C2cStreamingPhase::BrokenActive(stream_state);
                    }
                    C2cStreamingPhase::Completed => {}
                }
            }
            RespondEvent::Completed(response) => {
                let final_content = completed_response_content(&response).unwrap_or(&accumulated);
                let final_chars = final_content.chars().count();
                match phase {
                    C2cStreamingPhase::Active(mut stream_state) => {
                        // Active 表示 QQ 已创建流式气泡，Completed 只能继续使用同一个 stream id。
                        if !pending_delta.is_empty() {
                            let chunk = pending_delta.clone();
                            let index = stream_state.index;
                            let had_stream_id = stream_state.stream_id.is_some();
                            match send_stream_chunk(
                                sender,
                                user_openid,
                                Some(reply_msg_id),
                                &chunk,
                                &mut stream_state,
                                1,
                                false,
                            )
                            .await
                            {
                                Ok(_) => {
                                    pending_delta.clear();
                                    info!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "completed_flush",
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        content_chars = chunk.chars().count(),
                                        final_chars,
                                        "QQ stream pending delta flushed before final"
                                    );
                                }
                                Err(err) => {
                                    warn!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "completed_flush",
                                        stream_state = "broken_active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        content_chars = chunk.chars().count(),
                                        error = %err.log_summary(),
                                        final_chars,
                                        "QQ stream pending delta flush failed; ordinary fallback is disabled"
                                    );
                                    match send_stream_end(
                                        sender,
                                        user_openid,
                                        Some(reply_msg_id),
                                        "",
                                        &mut stream_state,
                                    )
                                    .await
                                    {
                                        Ok(()) => info!(
                                            user = %masked_user,
                                            reply_msg_id = %masked_reply_msg_id,
                                            phase = "completed_flush_final_chunk",
                                            stream_state = C2cStreamingPhase::Completed.name(),
                                            stream_state_value = 10_u8,
                                            done = true,
                                            reset_present = false,
                                            index_present = false,
                                            has_stream_id_before_send = stream_state.stream_id.is_some(),
                                            has_stream_id_after_send = stream_state.stream_id.is_some(),
                                            content_chars = 0_usize,
                                            final_chars,
                                            "QQ stream end after pending delta flush failure succeeded"
                                        ),
                                        Err(end_err) => warn!(
                                            user = %masked_user,
                                            reply_msg_id = %masked_reply_msg_id,
                                            phase = "completed_flush_final_chunk",
                                            stream_state = "broken_active",
                                            stream_state_value = 10_u8,
                                            done = true,
                                            reset_present = false,
                                            index_present = false,
                                            has_stream_id_before_send = stream_state.stream_id.is_some(),
                                            content_chars = 0_usize,
                                            error = %end_err.log_summary(),
                                            final_chars,
                                            "QQ stream end after pending delta flush failure failed"
                                        ),
                                    }
                                    return Ok(());
                                }
                            }
                        }
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_end(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            "",
                            &mut stream_state,
                        )
                        .await
                        {
                            Ok(()) => {
                                info!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "final_chunk",
                                    stream_state = C2cStreamingPhase::Completed.name(),
                                    stream_state_value = 10_u8,
                                    done = true,
                                    reset_present = false,
                                    index_present = false,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    content_chars = 0_usize,
                                    final_chars,
                                    "QQ stream final send succeeded"
                                );
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "final_chunk",
                                    stream_state = "broken_active",
                                    stream_state_value = 10_u8,
                                    done = true,
                                    reset_present = false,
                                    index_present = false,
                                    has_stream_id_before_send = had_stream_id,
                                    content_chars = 0_usize,
                                    error = %err.log_summary(),
                                    final_chars,
                                    "QQ stream final send failed; ordinary fallback is disabled after stream id was created"
                                );
                            }
                        }
                    }
                    C2cStreamingPhase::BrokenActive(mut stream_state) => {
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_end(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            "",
                            &mut stream_state,
                        )
                        .await
                        {
                            Ok(()) => info!(
                                user = %masked_user,
                                reply_msg_id = %masked_reply_msg_id,
                                phase = "broken_active_final_chunk",
                                stream_state = C2cStreamingPhase::Completed.name(),
                                stream_state_value = 10_u8,
                                done = true,
                                reset_present = false,
                                index_present = false,
                                has_stream_id_before_send = had_stream_id,
                                has_stream_id_after_send = stream_state.stream_id.is_some(),
                                content_chars = 0_usize,
                                final_chars,
                                "QQ stream end after broken active succeeded"
                            ),
                            Err(err) => warn!(
                                user = %masked_user,
                                reply_msg_id = %masked_reply_msg_id,
                                phase = "broken_active_final_chunk",
                                stream_state = "broken_active",
                                stream_state_value = 10_u8,
                                done = true,
                                reset_present = false,
                                index_present = false,
                                has_stream_id_before_send = had_stream_id,
                                content_chars = 0_usize,
                                error = %err.log_summary(),
                                final_chars,
                                "QQ stream end after broken active failed; ordinary fallback is disabled"
                            ),
                        }
                    }
                    C2cStreamingPhase::Completed => {}
                    C2cStreamingPhase::Pending(_) => {
                        let stream_state_name = phase.name();
                        send_c2c_respond_response_with_sender(sender, message, &response, config)
                            .await
                            .inspect(|_| {
                                info!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "ordinary_fallback_on_completed",
                                    stream_state = stream_state_name,
                                    final_chars,
                                    "QQ ordinary fallback send succeeded"
                                );
                            })
                            .inspect_err(|fallback_err| {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "ordinary_fallback_on_completed",
                                    stream_state = stream_state_name,
                                    error = %fallback_err,
                                    final_chars,
                                    "QQ ordinary fallback send failed"
                                );
                            })?;
                    }
                }
                return Ok(());
            }
            RespondEvent::Failed(failure) => {
                warn!(
                    user = %masked_user,
                    reply_msg_id = %masked_reply_msg_id,
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    stream_state = phase.name(),
                    accumulated_chars = accumulated.chars().count(),
                    "core respond stream failed"
                );
                if let C2cStreamingPhase::Active(mut stream_state)
                | C2cStreamingPhase::BrokenActive(mut stream_state) = phase
                {
                    send_stream_end(
                        sender,
                        user_openid,
                        Some(reply_msg_id),
                        "",
                        &mut stream_state,
                    )
                    .await
                    .inspect_err(|err| {
                        warn!(
                            user = %masked_user,
                            reply_msg_id = %masked_reply_msg_id,
                            phase = "failed_final_chunk",
                            stream_state_value = 10_u8,
                            done = true,
                            reset_present = false,
                            index_present = false,
                            has_stream_id = stream_state.stream_id.is_some(),
                            content_chars = 0_usize,
                            error = %err.log_summary(),
                            accumulated_chars = accumulated.chars().count(),
                            "QQ stream finalization after core failure failed"
                        );
                    })?;
                }
                return Err(anyhow::anyhow!(
                    "core respond stream failed before Completed: kind={:?}, retryable={}",
                    failure.kind,
                    failure.retryable
                ));
            }
        }
    }

    let accumulated_chars = accumulated.chars().count();
    warn!(
        user = %masked_user,
        reply_msg_id = %masked_reply_msg_id,
        stream_state = phase.name(),
        accumulated_chars,
        "core respond stream closed before Completed"
    );
    match phase {
        C2cStreamingPhase::Active(mut stream_state)
        | C2cStreamingPhase::BrokenActive(mut stream_state) => {
            send_stream_end(
                sender,
                user_openid,
                Some(reply_msg_id),
                "",
                &mut stream_state,
            )
            .await?;
        }
        C2cStreamingPhase::Pending(_) if !accumulated.is_empty() && !stream_first_attempted => {
            let response = response_from_incomplete_stream_text(&accumulated);
            send_c2c_respond_response_with_sender(sender, message, &response, config).await?;
        }
        C2cStreamingPhase::Pending(_) | C2cStreamingPhase::Completed => {}
    }
    Err(anyhow::anyhow!(
        "core respond stream closed before Completed; accumulated_chars={accumulated_chars}"
    ))
}

fn completed_response_content(response: &RespondResponse) -> Option<&str> {
    response.markdown.as_deref().or(response.text.as_deref())
}

fn response_from_incomplete_stream_text(content: &str) -> RespondResponse {
    RespondResponse {
        text: Some(content.to_owned()),
        markdown: Some(content.to_owned()),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
    }
}

/// 发送流式消息分片到 QQ。
///
/// `reset=false` 时 QQ 会把本次 `markdown.content` 追加到现有流式消息后面，
/// 因此这里传入的 content 必须是尚未发送过的增量。
/// 首帧只有拿到 stream id 才能进入 Active；后续帧即使 QQ 返回新的消息 id，
/// 也必须保留首帧 id，避免最终帧的 id/index 序列被 QQ 判定为无效。
async fn send_stream_chunk<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    content: &str,
    stream_state: &mut C2cStreamState,
    stream_state_value: u8,
    reset: bool,
) -> StreamSendResult {
    let markdown = MarkdownPayload::new(content);
    let result = sender
        .send_stream_markdown(
            user_openid,
            msg_id,
            &markdown,
            stream_state,
            stream_state_value,
            Some(reset),
        )
        .await?;
    if stream_state.stream_id.is_none()
        && let Some(id) = result.as_deref().filter(|id| !id.trim().is_empty())
    {
        // QQ 流式续接 id 以首帧返回值为准；中间帧返回的是消息 id，不应覆盖，
        // 否则后续 index 会相对于错误 id 递增，最终帧可能报 stream.index 无效。
        stream_state.stream_id = Some(id.to_owned());
    }
    stream_state.index += 1;
    Ok(result)
}

/// 发送流式结束帧（state=10, done=true）。
///
/// 结束包只表达关闭语义：保留 stream id，但不再凭空携带 index/reset，也不重复携带正文。
/// 首帧成功后不会回退成第二条普通消息，保持流式气泡的唯一发送所有权。
async fn send_stream_end<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    _content: &str,
    stream_state: &mut C2cStreamState,
) -> Result<(), crate::api::ApiError> {
    // 终包的正文在真实环境协议中没有依据，强制传空内容，最终序列化层也会省略 markdown 字段。
    let markdown = MarkdownPayload::new("");
    let result = sender
        .send_stream_markdown(user_openid, msg_id, &markdown, stream_state, 10, None)
        .await?;
    if stream_state.stream_id.is_none()
        && let Some(id) = result.as_deref().filter(|id| !id.trim().is_empty())
    {
        // 正常收尾前已经有首帧 id；这里只兼容“直接最终帧”或异常状态下的空 id。
        stream_state.stream_id = Some(id.to_owned());
    }
    Ok(())
}

// 私聊消息处理需要贯穿 QQ 回复、LLM 调用、去重和诊断状态，保持参数显式便于看清跨层依赖。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_c2c_message(
    mut message: C2cMessage,
    config: &AppConfig,
    auth: &AccessTokenManager,
    respond: &RespondClient,
    api: &QqApiClient,
    _dedupe: &MessageDedupe,
    reply_cache: &ReplyCache,
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
    // C2C message/event ID 已在 Aggregator 入口原子 reservation；这里不能再按逻辑批次任意 source ID 命中丢弃整批。
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
            &respond.health_snapshot(),
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
        Ok(response) => {
            runtime.record_respond_success();
            response
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

    match transport {
        RespondTransport::Complete(response) => {
            send_c2c_respond_response(api, runtime, &message, &response, config).await?;
        }
        RespondTransport::Stream(stream) => {
            stream_respond_c2c(stream, api, runtime, &message, config).await?;
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

#[cfg(test)]
mod tests {
    use super::event::{C2cMessage, MessageReply};
    use super::*;
    use crate::{
        api::{ApiError, SendFuture},
        config::{
            DEFAULT_CONVERSATION_QUEUE_CAPACITY, DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS, DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
            DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES, DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
            DEFAULT_MESSAGE_AGGREGATION_QUIET_MS, GroupMessageMode, MessageAggregationConfig,
        },
        media::ImagePayload,
    };
    use qq_maid_core::service::{CoreFailureKind, CoreRespondFailure};
    use std::collections::VecDeque;

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

    #[derive(Debug)]
    struct FakeEventStream {
        events: VecDeque<(Duration, RespondEvent)>,
    }

    impl FakeEventStream {
        fn new(events: impl IntoIterator<Item = RespondEvent>) -> Self {
            Self {
                events: events
                    .into_iter()
                    .map(|event| (Duration::ZERO, event))
                    .collect(),
            }
        }

        fn with_delays(events: impl IntoIterator<Item = (Duration, RespondEvent)>) -> Self {
            Self {
                events: events.into_iter().collect(),
            }
        }
    }

    impl RespondEventStream for FakeEventStream {
        fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
            Box::pin(async move {
                let (delay, event) = self.events.pop_front()?;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Some(event)
            })
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeCall {
        Stream {
            content: String,
            msg_id: Option<String>,
            stream_id: Option<String>,
            index: u32,
            stream_state_value: u8,
            reset: Option<bool>,
        },
        Markdown {
            content: String,
            msg_id: Option<String>,
        },
        Text {
            content: String,
            msg_id: Option<String>,
        },
        Image,
    }

    #[derive(Debug)]
    struct FakeStreamSender {
        stream_results: Mutex<VecDeque<StreamSendResult>>,
        calls: Mutex<Vec<FakeCall>>,
    }

    impl FakeStreamSender {
        fn new(stream_results: impl IntoIterator<Item = StreamSendResult>) -> Self {
            Self {
                stream_results: Mutex::new(stream_results.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<FakeCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OutboundSender for FakeStreamSender {
        fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Text {
                    content: text.to_owned(),
                    msg_id: target.msg_id.clone(),
                });
                Ok(Some("ordinary-text-id".to_owned()))
            })
        }

        fn send_markdown<'a>(
            &'a self,
            target: &'a C2cReplyTarget,
            markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Markdown {
                    content: markdown.content.clone(),
                    msg_id: target.msg_id.clone(),
                });
                Ok(Some("ordinary-markdown-id".to_owned()))
            })
        }

        fn send_image<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _image: &'a ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Image);
                Err(ApiError::Unsupported("image"))
            })
        }
    }

    impl C2cStreamSender for FakeStreamSender {
        fn send_stream_markdown<'a>(
            &'a self,
            _user_openid: &'a str,
            msg_id: Option<&'a str>,
            markdown: &'a MarkdownPayload,
            stream_state: &'a C2cStreamState,
            stream_state_value: u8,
            reset: Option<bool>,
        ) -> StreamSendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Stream {
                    content: markdown.content.clone(),
                    msg_id: msg_id.map(str::to_owned),
                    stream_id: stream_state.stream_id.clone(),
                    index: stream_state.index,
                    stream_state_value,
                    reset,
                });
                self.stream_results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Ok(None))
            })
        }
    }

    fn c2c_message() -> C2cMessage {
        C2cMessage {
            message_id: "msg-1".to_owned(),
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "晚上好".to_owned(),
            reply: None,
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        }
    }

    fn respond_response(text: &str) -> RespondResponse {
        RespondResponse {
            text: Some(text.to_owned()),
            markdown: Some(text.to_owned()),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
        }
    }

    fn test_config() -> AppConfig {
        AppConfig {
            app_id: "app".to_owned(),
            app_secret: "secret".to_owned(),
            sandbox: false,
            api_base: "https://example.test".to_owned(),
            token_refresh_margin: Duration::from_secs(60),
            enable_markdown: true,
            enable_image: false,
            enable_group_messages: false,
            verbose_log: false,
            group_message_mode: GroupMessageMode::Mention,
            group_active_keywords: vec!["小女仆".to_owned()],
            conversation_queue_capacity: DEFAULT_CONVERSATION_QUEUE_CAPACITY,
            max_active_conversation_workers: DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            conversation_worker_idle_timeout: Duration::from_secs(300),
            message_aggregation: MessageAggregationConfig {
                private_enabled: true,
                group_enabled: false,
                quiet: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_QUIET_MS),
                max_wait: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS),
                max_messages: DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
                max_chars: DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
                max_active_keys: DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
            },
        }
    }

    #[tokio::test]
    async fn stream_first_send_error_falls_back_to_completed_response() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Markdown {
                    content: "晚上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_first_send_without_id_falls_back_to_completed_response() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Markdown {
                    content: "晚上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_single_content_packet_then_final_keeps_stream_id() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("测试成功".to_owned()),
            RespondEvent::Completed(respond_response("测试成功")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "测试成功".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_active_path_reuses_id_and_increments_content_index() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚上".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("好".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上好")),
            ),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_empty_delta_does_not_consume_index() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta(String::new()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("好")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_middle_returned_id_does_not_replace_first_stream_id() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("上".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上")),
            ),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Ok(Some("middle-message-id".to_owned())),
            Ok(None),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_middle_chunks_coalesce_only_unsent_delta() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (Duration::ZERO, RespondEvent::TextDelta("上".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("好".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上好")),
            ),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_final_failure_does_not_send_ordinary_fallback_after_active() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Err(ApiError::Unsupported("stream")),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_completed_flushes_pending_delta_before_final() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚".to_owned()),
            RespondEvent::TextDelta("上".to_owned()),
            RespondEvent::Completed(respond_response("晚上")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_completed_without_delta_uses_ordinary_reply_path() {
        let events = FakeEventStream::new([RespondEvent::Completed(respond_response("晚上好"))]);
        let sender = FakeStreamSender::new([]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![FakeCall::Markdown {
                content: "晚上好".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn stream_completed_sends_single_final_chunk() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("好")),
            RespondEvent::Completed(respond_response("好")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        let final_count = sender
            .calls()
            .into_iter()
            .filter(|call| {
                matches!(
                    call,
                    FakeCall::Stream {
                        stream_state_value: 10,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(final_count, 1);
    }

    #[tokio::test]
    async fn stream_chunk_failure_does_not_advance_next_index() {
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);
        let mut stream_state = C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 1,
        };

        let result = send_stream_chunk(
            &sender,
            "user-1",
            Some("msg-1"),
            "失败分片",
            &mut stream_state,
            1,
            false,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(stream_state.index, 1);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: "失败分片".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 1,
                reset: Some(false),
            }]
        );
    }

    #[tokio::test]
    async fn stream_final_success_does_not_commit_extra_next_index() {
        let sender = FakeStreamSender::new([Ok(None)]);
        let mut stream_state = C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 2,
        };

        send_stream_end(
            &sender,
            "user-1",
            Some("msg-1"),
            "最终正文",
            &mut stream_state,
        )
        .await
        .unwrap();

        assert_eq!(stream_state.index, 2);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: String::new(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 2,
                stream_state_value: 10,
                reset: None,
            }]
        );
    }

    #[tokio::test]
    async fn stream_closed_before_completed_is_not_silent_success() {
        let events = FakeEventStream::new([RespondEvent::TextDelta("晚上".to_owned())]);
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);

        let result =
            stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

        assert!(result.is_err());
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: "晚上".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: None,
                index: 0,
                stream_state_value: 1,
                reset: Some(false),
            }]
        );
    }

    #[tokio::test]
    async fn stream_middle_failure_does_not_send_ordinary_fallback_on_completed() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("上".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上")),
            ),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Err(ApiError::Unsupported("stream")),
            Ok(None),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: String::new(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn core_failed_event_is_returned_as_observable_error() {
        let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
            kind: CoreFailureKind::Internal,
            message: "boom".to_owned(),
            retryable: false,
        })]);
        let sender = FakeStreamSender::new([]);

        let result =
            stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

        assert!(result.is_err());
        assert!(sender.calls().is_empty());
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
    fn group_at_reply_text_mentions_sender_when_member_openid_exists() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);

        assert_eq!(
            prefix_group_reply_text(&message, "回复正文"),
            "<@member-1>\n回复正文"
        );
    }

    #[test]
    fn group_at_respond_error_log_text_keeps_member_openid_out() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let error = crate::respond::RespondError::Core(qq_maid_core::service::CoreError::new(
            "internal_error",
            "respond",
            "backend down",
        ));

        let (qq_text, log_text) = group_respond_error_texts(&message, &error);

        assert!(qq_text.starts_with("<@member-1>\n"));
        assert!(!log_text.contains("member-1"));
        assert!(!log_text.contains("<@"));
    }

    #[test]
    fn group_reply_text_skips_mention_for_plain_group_message() {
        let message = group_message("hello", GroupEventType::GroupMessage);

        assert_eq!(prefix_group_reply_text(&message, "回复正文"), "回复正文");
    }

    #[test]
    fn group_at_reply_text_skips_mention_without_member_openid() {
        let mut message = group_message("hello", GroupEventType::GroupAtMessage);
        message.member_openid = None;

        assert_eq!(prefix_group_reply_text(&message, "回复正文"), "回复正文");
    }

    #[test]
    fn group_at_reply_outbound_mentions_sender() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let outbound = OutboundMessage::Text {
            text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound),
            OutboundMessage::Text {
                text: "<@member-1>\n回复正文".to_owned(),
            }
        );
    }

    #[test]
    fn group_message_mode_policy_matches_triggers() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let active_keywords = vec!["小女仆".to_owned()];
        let ordinary = group_message("hello", GroupEventType::GroupMessage);
        let command = group_message("/rss", GroupEventType::GroupMessage);
        let mention = group_message("[CQ:at,qq=123] hello", GroupEventType::GroupMessage);
        let active_keyword = group_message("小女仆在吗", GroupEventType::GroupMessage);
        let at_event = group_message("hello", GroupEventType::GroupAtMessage);

        assert!(!should_process_group_message(
            GroupMessageMode::Off,
            &active_keywords,
            &ordinary,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Off,
            &active_keywords,
            &at_event,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Command,
            &active_keywords,
            &command,
            &cache
        ));
        assert!(!should_process_group_message(
            GroupMessageMode::Command,
            &active_keywords,
            &mention,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &active_keywords,
            &mention,
            &cache
        ));
        assert!(!should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &ordinary,
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &active_keyword,
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
            &[],
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
        assert!(cooldowns.check_and_mark(
            &message,
            now + super::group_filter::GROUP_USER_COOLDOWN + Duration::from_secs(1)
        ));
    }

    #[tokio::test]
    async fn resolve_signals_fills_known_reply_content() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(
            ReplyCacheKey::new("private:user-1".to_owned(), "quoted-1"),
            "上一条消息".to_owned(),
        );
        let mut message = C2cMessage {
            message_id: "msg-1".to_owned(),
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: None,
            }),
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        };

        resolve_signals(&mut message, &cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: Some("上一条消息".to_owned()),
            })
        );
        assert_eq!(
            cache
                .lock()
                .unwrap()
                .get(&ReplyCacheKey::new("private:user-1".to_owned(), "msg-1"))
                .map(String::as_str),
            Some("你好")
        );
    }

    #[test]
    fn resolve_signals_keeps_reply_content_none_on_cache_miss() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        let mut message = C2cMessage {
            message_id: "msg-1".to_owned(),
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: Some(MessageReply {
                message_id: "quoted-missing".to_owned(),
                content: None,
            }),
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        };

        resolve_signals(&mut message, &cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-missing".to_owned(),
                content: None,
            })
        );
        assert_eq!(
            cache
                .lock()
                .unwrap()
                .get(&ReplyCacheKey::new("private:user-1".to_owned(), "msg-1"))
                .map(String::as_str),
            Some("你好")
        );
    }

    #[test]
    fn reply_cache_isolated_by_scope_key() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(
            ReplyCacheKey::new("private:user-a".to_owned(), "same-id"),
            "私聊消息".to_owned(),
        );
        cache.lock().unwrap().insert(
            ReplyCacheKey::new("group:group-a".to_owned(), "same-id"),
            "群聊消息".to_owned(),
        );

        let mut private_message = C2cMessage {
            message_id: "m1".to_owned(),
            event_id: Some("e1".to_owned()),
            source_message_ids: vec!["m1".to_owned()],
            source_event_ids: vec!["e1".to_owned()],
            user_openid: "user-a".to_owned(),
            content: "当前消息".to_owned(),
            reply: Some(MessageReply {
                message_id: "same-id".to_owned(),
                content: None,
            }),
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        };
        resolve_signals(&mut private_message, &cache);

        let mut group_like_private = C2cMessage {
            message_id: "m2".to_owned(),
            event_id: Some("e2".to_owned()),
            source_message_ids: vec!["m2".to_owned()],
            source_event_ids: vec!["e2".to_owned()],
            user_openid: "user-b".to_owned(),
            content: "另一条".to_owned(),
            reply: Some(MessageReply {
                message_id: "same-id".to_owned(),
                content: None,
            }),
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        };
        resolve_signals(&mut group_like_private, &cache);

        assert_eq!(
            private_message.reply.and_then(|reply| reply.content),
            Some("私聊消息".to_owned())
        );
        assert_eq!(
            group_like_private.reply.and_then(|reply| reply.content),
            None
        );
    }
}
