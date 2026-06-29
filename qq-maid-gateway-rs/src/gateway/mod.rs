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
    logging::{c2c_message_log_summary, group_message_log_summary, mask_openid},
    outbound::{
        RuntimeRecordingGroupSender, RuntimeRecordingSender, send_c2c_text_with_status,
        send_group_text_with_status,
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
        C2cReplyTarget, C2cStreamState, GroupReplyTarget, QqApiClient,
        send_group_outbound_with_fallback, send_outbound_with_fallback,
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

/// 发送 C2C 普通（非流式）回复消息。
///
/// 从原 `handle_c2c_message` 的发送逻辑抽取，供 Complete 和
/// Stream fallback 两路径复用。
async fn send_c2c_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
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
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    send_outbound_with_fallback(&sender, &target, &outbound)
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
/// - 首次流式发送失败时，自动回退到普通 C2C 消息发送。
/// - 已成功输出部分内容后发生错误时，不再重复发送完整普通消息。
async fn stream_respond_c2c(
    mut stream: qq_maid_core::service::CoreResponseStream,
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let user_openid = &message.user_openid;
    let masked_user = mask_openid(user_openid);
    let reply_msg_id = &message.message_id;

    // 流式发送状态
    let mut stream_state = C2cStreamState {
        stream_id: None,
        index: 0,
    };
    let mut accumulated = String::new();
    let mut streaming_active = false;
    let mut first_send_attempted = false;
    let mut last_send_at = Instant::now();
    let mut sent_len = 0usize;

    // 消费 Core 流事件
    while let Some(event) = stream.recv().await {
        match event {
            RespondEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                accumulated.push_str(&delta);

                if !first_send_attempted {
                    first_send_attempted = true;
                    // 首次发送：立即尝试流式发送
                    match send_stream_chunk(
                        api,
                        user_openid,
                        &accumulated,
                        &mut stream_state,
                        1,     // state=1 生成中
                        false, // reset=false
                    )
                    .await
                    {
                        Ok(()) => {
                            streaming_active = true;
                            sent_len = accumulated.len();
                            last_send_at = Instant::now();
                            info!(
                                user = %masked_user,
                                reply_msg_id,
                                sent_len,
                                "QQ stream send started successfully"
                            );
                        }
                        Err(err) => {
                            warn!(
                                user = %masked_user,
                                reply_msg_id,
                                error = %err.log_summary(),
                                "QQ stream first send failed; switching to fallback"
                            );
                            // 回退：跳出 TextDelta 分支，进入 fallback 收集路径
                            break;
                        }
                    }
                } else if streaming_active {
                    // 流式发送中：按固定间隔节流发送中间更新
                    let elapsed = last_send_at.elapsed();
                    let new_content_len = accumulated.len() - sent_len;
                    if elapsed >= Duration::from_millis(STREAM_THROTTLE_MS) && new_content_len > 0 {
                        match send_stream_chunk(
                            api,
                            user_openid,
                            &accumulated,
                            &mut stream_state,
                            1,     // state=1 生成中
                            false, // reset=false
                        )
                        .await
                        {
                            Ok(()) => {
                                sent_len = accumulated.len();
                                last_send_at = Instant::now();
                            }
                            Err(err) => {
                                // 中间发送失败：不中断，继续尝试发送后续内容
                                // 已成功输出部分内容，不再重复发送完整普通消息
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id,
                                    error = %err.log_summary(),
                                    "QQ stream mid send failed; continuing"
                                );
                            }
                        }
                    }
                }
                // else: first_send_attempted && !streaming_active
                // 即首次流式发送失败的 fallback 阶段，继续收集但不发送
            }
            RespondEvent::Completed(response) => {
                if streaming_active {
                    // 流式发送中：发送最终帧（state=10, reset=true）
                    send_stream_final(api, user_openid, &response, &mut stream_state, &accumulated)
                        .await;
                } else if first_send_attempted {
                    // 首次流式发送失败，回退到普通消息
                    send_c2c_respond_response(api, runtime, message, &response, config).await?;
                } else {
                    // 无 TextDelta 直接 Completed（非流式 LLM 响应的流包装）
                    send_c2c_respond_response(api, runtime, message, &response, config).await?;
                }
                return Ok(());
            }
            RespondEvent::Failed(failure) => {
                warn!(
                    user = %masked_user,
                    reply_msg_id,
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    streaming_active,
                    "core respond stream failed"
                );
                if streaming_active && !accumulated.is_empty() {
                    // 已成功输出部分内容：发送已累积内容结束流式状态
                    send_stream_final_text(api, user_openid, &accumulated, &mut stream_state).await;
                }
                // 否则不重复发送（尚未开始发送时无内容可发）
                return Ok(());
            }
        }
    }

    // 流 channel 意外关闭（无 Completed 事件）
    if streaming_active && !accumulated.is_empty() {
        send_stream_final_text(api, user_openid, &accumulated, &mut stream_state).await;
    } else if first_send_attempted && !streaming_active && !accumulated.is_empty() {
        // fallback 中收集到的内容：作为普通 markdown 消息发送
        let payload = MarkdownPayload::new(accumulated);
        let _ = api
            .send_c2c_markdown(user_openid, Some(reply_msg_id), &payload)
            .await
            .inspect_err(|err| {
                warn!(
                    user = %masked_user,
                    reply_msg_id,
                    error = %err.log_summary(),
                    "QQ fallback markdown send failed"
                );
            });
    }
    Ok(())
}

/// 发送流式消息分片到 QQ。
///
/// 封装 `QqApiClient::send_c2c_markdown_stream` 并自动更新 `C2cStreamState`。
async fn send_stream_chunk(
    api: &QqApiClient,
    user_openid: &str,
    content: &str,
    stream_state: &mut C2cStreamState,
    state: u8,
    reset: bool,
) -> Result<(), crate::api::ApiError> {
    let markdown = MarkdownPayload::new(content);
    match api
        .send_c2c_markdown_stream(user_openid, &markdown, stream_state, state, reset)
        .await
    {
        Ok(Some(id)) => {
            stream_state.stream_id = Some(id);
            stream_state.index += 1;
            Ok(())
        }
        Ok(None) => {
            // 成功但无 id：仍递增 index，后续流式可能无法续接
            stream_state.index += 1;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// 发送流式最终帧（使用 Completed 响应中的格式化正文）。
///
/// 优先使用 markdown 格式，fallback 到 text，再 fallback 到累积文本。
async fn send_stream_final(
    api: &QqApiClient,
    user_openid: &str,
    response: &RespondResponse,
    stream_state: &mut C2cStreamState,
    fallback_text: &str,
) {
    let final_content = response
        .markdown
        .as_deref()
        .or(response.text.as_deref())
        .unwrap_or(fallback_text);
    send_stream_final_text(api, user_openid, final_content, stream_state).await;
}

/// 发送流式最终帧（使用给定的文本，state=10, reset=true）。
///
/// reset=true 用全量文本替换流式消息，确保 QQ 端显示完整正文。
async fn send_stream_final_text(
    api: &QqApiClient,
    user_openid: &str,
    content: &str,
    stream_state: &mut C2cStreamState,
) {
    let markdown = MarkdownPayload::new(content);
    match api
        .send_c2c_markdown_stream(user_openid, &markdown, stream_state, 10, true)
        .await
    {
        Ok(Some(id)) => {
            info!(
                user = %mask_openid(user_openid),
                final_msg_id = %id,
                final_len = content.chars().count(),
                "QQ stream send completed"
            );
            stream_state.stream_id = Some(id);
        }
        Ok(None) => {
            info!(
                user = %mask_openid(user_openid),
                final_len = content.chars().count(),
                "QQ stream send completed (no id returned)"
            );
        }
        Err(err) => {
            warn!(
                user = %mask_openid(user_openid),
                error = %err.log_summary(),
                "QQ stream final send failed"
            );
        }
    }
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
    use crate::config::GroupMessageMode;

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
