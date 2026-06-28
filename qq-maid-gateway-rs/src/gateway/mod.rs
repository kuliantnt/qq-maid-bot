//! QQ gateway 运行域。负责 WebSocket 主循环、事件分发、去重、诊断与回发编排。

pub mod dedupe;
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

use anyhow::Context;
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
        C2cReplyTarget, GroupReplyTarget, QqApiClient, send_group_outbound_with_fallback,
        send_outbound_with_fallback,
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

/// Signal Layer 只是 gateway 内部的临时语义增强层，不是业务核心。
/// 这里只维护一个短时 `message_id -> content` 缓存，用于 reply.content 本地回填。
/// gateway 不负责 prompt 构建；真正交给 CoreService 的字符串统一在 respond.rs 的 Egress 层生成。
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
pub async fn run(
    config: AppConfig,
    respond: RespondClient,
    push_sink: GatewayPushSink,
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
    let dedupe = MessageDedupe::new(DEDUPE_TTL);
    // 运行时状态，记录网关连接、收发消息等统计信息，供 /ping 等命令使用
    let runtime = GatewayRuntimeStatus::new();
    let group_outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    // 主动推送已经进程内化；Core 通过 PushSink 进入这里，仍由 Gateway 负责 QQ 发送。
    push_sink.bind(api.clone(), runtime.clone(), group_outbound_cache.clone());
    // reply 只需要一个极简 HashMap 缓存，不引入额外抽象层或持久化。
    let mut reply_cache = HashMap::new();
    let mut group_cooldowns = GroupCooldowns::default();
    // 断线续连所需的状态（session_id + seq）
    let mut resume = ResumeState::default();

    loop {
        info!(api_base = %config.api_base, "fetching QQ gateway url");
        // 每次重连前重新获取网关地址，避免 IP/调度发生变化后仍连旧地址
        let gateway_url = match protocol::fetch_gateway_url(&http_client, &config, &auth).await {
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
        tokio::time::sleep(protocol::reconnect_delay()).await;
    }
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
        Ok(response) => {
            runtime.record_respond_success();
            response
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

async fn send_group_respond_response(
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

async fn consume_respond_stream(
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

    let response = match transport {
        RespondTransport::Complete(response) => response,
        RespondTransport::Stream(stream) => {
            let Some(response) = consume_respond_stream(stream).await else {
                return Ok(());
            };
            response
        }
    };

    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
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
