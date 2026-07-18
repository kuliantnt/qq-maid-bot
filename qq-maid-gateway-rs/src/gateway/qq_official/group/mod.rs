//! 群消息处理管道。
//!
//! 这里串起群消息过滤、Core 调用、QQ 群回复发送和机器人 outbound id 回填。
//! 群触发策略与冷却的纯判定逻辑放在 `group_filter.rs`，避免处理管道继续膨胀。

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use qq_maid_core::service::{CoreInboundKind, CoreRespondFailure};
use tracing::{debug, info, warn};

fn empty_group_reply_fallback_text(bot_display_name: &str) -> String {
    format!("唔，{bot_display_name}刚刚没整理出可用回复。可以再说一次。")
}

/// 群聊冷却命中但明确指向机器人时的轻量提示文案。
///
/// 不走 LLM，仅让用户知道“机器人听见了但要稍等”，避免静默吞掉造成“没听见”的体感
/// （#386）。称呼复用主动关键词首项，且不携带任何业务臆测。
fn group_cooldown_hint_text(bot_display_name: &str) -> String {
    format!("哦哦，刚刚在处理上一条消息，稍后再说一声{bot_display_name}就能继续了呢。")
}

#[cfg(test)]
use super::super::group_filter::should_process_group_message;
use super::super::{
    bot_identity::SharedBotIdentity,
    cache::BotOutboundCache,
    command::{GatewayCommandContext, GatewayCommandConversation, GatewayCommandService},
    dedupe::MessageDedupe,
    event::{GroupEventType, GroupMessage},
    group_filter::{
        GroupCooldowns, group_message_addresses_bot, mentions_current_bot,
        should_ignore_group_message, should_process_group_message_with_prefix,
    },
    logging::{group_message_log_summary, mask_openid},
    media_fetch::{MediaFetchContext, fetch_qq_official_image_attachments},
    outbound::{
        ReplyCapability, ReplyTarget, RuntimeRecordingGroupSender, send_group_text_with_status,
    },
    ping::GatewayRuntimeStatus,
    platform,
    ref_index::SharedRefIndex,
    stream::RespondEventStream,
};
use crate::{
    api::{GroupOutboundSender, QqApiClient, SendMessageIds},
    config::AppConfig,
    message_chunk::{ChunkLimits, OutboundSendError, send_group_outbound_chunked},
    render::{OutboundMessage, render_respond_response_for_profile},
    respond::{
        RespondClient, RespondEvent, RespondResponse, RespondTransport, respond_error_to_qq_text,
    },
};

fn group_reply_mention_prefix(
    message: &GroupMessage,
    capability: &ReplyCapability,
) -> Option<String> {
    // 只有官方确认提到当前机器人时，才在回复正文里 @ 回发起人；
    // 普通群命令、关键词触发和回复机器人消息继续只挂原消息 msg_id，避免额外打扰。
    if !capability.supports_at_mention {
        return None;
    }
    if !mentions_current_bot(message) {
        return None;
    }
    message
        .member_openid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|member_openid| format!("<@{member_openid}>"))
}

fn prefix_group_reply_outbound(
    message: &GroupMessage,
    outbound: OutboundMessage,
    capability: &ReplyCapability,
) -> OutboundMessage {
    // QQ 官方群文本消息不会把 `<@openid>` 渲染成昵称，会直接暴露 openid。
    // 只有 markdown 出站消息保留显式 at；纯文本命令依靠 reply target 关联原消息。
    if !matches!(outbound, OutboundMessage::Markdown { .. }) {
        return outbound;
    }
    let Some(prefix) = group_reply_mention_prefix(message, capability) else {
        return outbound;
    };
    outbound.prefix_text(&prefix)
}

fn group_respond_error_texts(
    _message: &GroupMessage,
    err: &crate::respond::RespondError,
    _capability: &ReplyCapability,
) -> (String, String) {
    let log_text = respond_error_to_qq_text(err);
    // 本地错误 fallback 是纯文本发送，不能拼 `<@openid>`，否则 QQ 会原样展示 openid。
    let qq_text = log_text.clone();
    (qq_text, log_text)
}

/// 群聊冷却命中但明确指向机器人时发送轻量提示。
///
/// 这里只发一条普通文本，挂在被冷却消息的 `message_id` 上回复；不走 LLM、不调 Core，
/// 发送失败只记录运行状态日志，不阻断或重试，避免冷却路径本身被放大成新的负担。
async fn send_cooldown_hint(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &GroupMessage,
    bot_display_name: &str,
) {
    let text = group_cooldown_hint_text(bot_display_name);
    let result = send_group_text_with_status(
        api,
        runtime,
        &message.group_openid,
        Some(&message.message_id),
        &text,
    )
    .await;
    if let Err(error) = result {
        warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            error = %error.log_summary(),
            "group cooldown hint send failed"
        );
    }
}

// 群消息链路同样需要显式串起 QQ 回复、LLM 调用、去重、冷却和运行状态；
// 这里沿用私聊分支的做法保留展开参数，避免把跨层依赖藏进临时聚合对象。
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) async fn handle_group_message_for_test(
    message: GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &Arc<Mutex<GroupCooldowns>>,
    bot_identity: &SharedBotIdentity,
    runtime: &GatewayRuntimeStatus,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    let commands =
        GatewayCommandService::from_config(config.clone(), runtime.clone(), respond.clone());
    handle_group_message(
        message,
        config,
        &commands,
        respond,
        api,
        dedupe,
        group_outbound_cache,
        group_cooldowns,
        bot_identity,
        runtime,
        ref_index,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_group_message(
    mut message: GroupMessage,
    config: &AppConfig,
    commands: &GatewayCommandService,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &Arc<Mutex<GroupCooldowns>>,
    bot_identity: &SharedBotIdentity,
    runtime: &GatewayRuntimeStatus,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content = crate::respond::build_group_respond_content_with_prefix(
        &message,
        &config.group_active_keywords,
        config.command_prefix,
    );
    observe_group_message_ref_index(&message, respond, ref_index);
    if should_ignore_group_message(
        &message,
        &respond_content,
        &masked_group,
        group_outbound_cache,
    ) {
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
    let command_context = GatewayCommandContext {
        platform_name: "QQ 官方机器人",
        platform_code: "qq_official_gateway_rs",
        event_name: message.event_type.as_respond_event_type(),
        conversation: GatewayCommandConversation::Group,
        user_id: message.member_openid.clone(),
        group_id: Some(message.group_openid.clone()),
        message_id: Some(message.message_id.clone()),
        timestamp: message.timestamp.clone(),
        attachment_count: message.attachments.len(),
    };
    if let Some(output) = commands
        .try_handle(&respond_content, &command_context)
        .await
    {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            "local Gateway command matched for QQ group"
        );
        send_group_local_command(
            api,
            runtime,
            config,
            &message,
            output.render(&ReplyCapability::qq_official_group(config)),
        )
        .await?;
        return Ok(());
    }
    if !should_process_group_message_with_prefix(
        config.group_message_mode,
        &config.group_active_keywords,
        config.command_prefix,
        &message,
        &respond_content,
        bot_identity,
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
    // 只用轻量、身份完整的 Core request 读取确定性命令和当前 actor 可见 Pending；
    // 分类不会下载媒体、补全成员信息或写入 ref index。Immediate 直接进入 Core，普通聊天
    // 才沿用群级/用户级冷却，避免 Gateway 复制 Memory/Todo 等业务词判断。
    let bypass_normal_chat_cooldown = if message.event_type == GroupEventType::GroupMessage {
        match respond
            .classify_group(
                &message,
                &config.group_active_keywords,
                config.command_prefix,
                respond_content.clone(),
            )
            .await
        {
            Ok(classification) => classification.kind == CoreInboundKind::Immediate,
            Err(error) => {
                warn!(
                    message_id = %message.message_id,
                    group = %masked_group,
                    member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                    error = %error.log_summary(),
                    "group inbound classification failed; preserving normal chat cooldown"
                );
                false
            }
        }
    } else {
        false
    };
    if message.event_type == GroupEventType::GroupMessage
        && !bypass_normal_chat_cooldown
        && !group_cooldowns
            .lock()
            .unwrap()
            .check_and_mark(&message, Instant::now())
    {
        if group_message_addresses_bot(&message, group_outbound_cache) {
            info!(
                message_id = %message.message_id,
                group = %masked_group,
                member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                "group message throttled by cooldown; sending lightweight hint"
            );
            send_cooldown_hint(api, runtime, &message, config.bot_display_name()).await;
        } else {
            info!(
                message_id = %message.message_id,
                group = %masked_group,
                member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                "group message ignored by cooldown"
            );
        }
        return Ok(());
    }

    fetch_qq_official_image_attachments(
        &reqwest::Client::new(),
        &MediaFetchContext {
            platform: "qq_official",
            app_id: config
                .app_id
                .clone()
                .context("QQ group handler requires a bound channel")?,
            peer_id: message.group_openid.clone(),
            root_dir: config.media_dir.clone(),
            timeout: config.media_download_timeout,
            max_bytes: config.media_max_bytes,
        },
        &message.message_id,
        &mut message.input_parts,
        &message.attachments,
    )
    .await;

    let mut inbound =
        respond.prepare_inbound(crate::respond::normalized_group_inbound_with_prefix(
            &message,
            &config.group_active_keywords,
            config.command_prefix,
        ));
    {
        let mut index = ref_index.lock().unwrap();
        index.enrich_inbound(&mut inbound);
    }
    // 成员详情补全（#319）：best-effort 调用 #229 补全 actor / mention / 引用 sender
    // 的展示字段，失败降级 source=Event，不阻断主回复流程。补全后再 insert_inbound，
    // 让索引里存的是补全后的 sender。配置开关默认开启，可经环境变量关闭。
    if config.member_detail_enrich_enabled {
        platform::member_enrich::enrich_inbound_member_details(api, &mut inbound).await;
    }
    {
        let mut index = ref_index.lock().unwrap();
        index.insert_inbound(&inbound);
    }

    info!(
        message_id = %message.message_id,
        group = %masked_group,
        "calling respond backend for group"
    );
    let transport = match respond.respond_inbound(&inbound, respond_content).await {
        Ok(response) => {
            runtime.record_respond_success();
            response
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let capability = ReplyCapability::qq_official_group(config);
            let (qq_text, log_text) = group_respond_error_texts(&message, &err, &capability);
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
            group_outbound_cache
                .lock()
                .unwrap()
                .insert(sent_message_id.message_id.clone());
            group_outbound_cache
                .lock()
                .unwrap()
                .insert_ref_index_id(sent_message_id.ref_index_id);
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
                ref_index,
            )
            .await?;
        }
        RespondTransport::Stream(stream) => match consume_respond_stream(stream).await {
            GroupStreamOutcome::Completed(response) => {
                send_group_respond_response(
                    api,
                    runtime,
                    config,
                    group_outbound_cache,
                    &message,
                    &response,
                    ref_index,
                )
                .await?;
            }
            GroupStreamOutcome::Failed(failure) => {
                let sender = RuntimeRecordingGroupSender {
                    inner: api,
                    runtime,
                };
                send_group_stream_failure(&sender, group_outbound_cache, &message, &failure)
                    .await?;
            }
            GroupStreamOutcome::ClosedBeforeCompleted => {}
        },
    }
    Ok(())
}

async fn send_group_local_command(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    message: &GroupMessage,
    outbound: OutboundMessage,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingGroupSender {
        inner: api,
        runtime,
    };
    let target = ReplyTarget::qq_group(
        message.group_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_group_target()
    .expect("QQ group command target should adapt to QQ API target");
    let limits = ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    send_group_outbound_chunked(&sender, &target, &outbound, &limits, |_, _| {})
        .await
        .map(|_| ())
        .map_err(|error| match error {
            OutboundSendError::NotSent { source }
            | OutboundSendError::PartiallySent { source, .. } => source.into(),
        })
}

fn observe_group_message_ref_index(
    message: &GroupMessage,
    respond: &RespondClient,
    ref_index: &SharedRefIndex,
) {
    if message.author_is_self || message.author_is_bot || message.current_msg_idx.is_none() {
        return;
    }
    let inbound = respond.prepare_inbound(platform::qq_official::inbound_from_group(message));
    match ref_index.lock() {
        Ok(mut index) => index.insert_inbound(&inbound),
        Err(_) => warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "group inbound ref_index observe skipped because index lock is poisoned"
        ),
    }
}

async fn send_group_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    response: &RespondResponse,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    if response.suppresses_reply() {
        debug!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "group reply suppressed by Core"
        );
        return Ok(());
    }
    let capability = ReplyCapability::qq_official_group(config);
    let outbound = match render_respond_response_for_profile(response, &capability.render) {
        Some(outbound) => outbound,
        None => {
            warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            fallback_reason = "empty_rendered_response",
            "respond backend produced no group reply text; sending local fallback"
            );
            OutboundMessage::Text {
                text: empty_group_reply_fallback_text(config.bot_display_name()),
            }
        }
    };
    let outbound = prefix_group_reply_outbound(message, outbound, &capability);
    let sender = RuntimeRecordingGroupSender {
        inner: api,
        runtime,
    };
    let target = ReplyTarget::qq_group(
        message.group_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_group_target()
    .expect("QQ group reply target should adapt to QQ API target");
    let limits = ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    // 普通群回复统一走分段编排：每个成功发送并返回 message id 的分段写入
    // `BotOutboundCache`；失败分段不写，错误向上传递为 PartiallySent / NotSent。
    match send_group_outbound_chunked(&sender, &target, &outbound, &limits, |_, sent_ids| {
        record_group_bot_outbound_send(
            group_outbound_cache,
            ref_index,
            message,
            response,
            config,
            sent_ids,
            outbound.fallback_text(),
        );
    })
    .await
    {
        Ok(_) => Ok(()),
        Err(OutboundSendError::NotSent { source }) => Err(source.into()),
        Err(OutboundSendError::PartiallySent { source, .. }) => {
            // 已成功前段已写入 cache，这里只把底层错误向上传递，不伪造完整送达。
            Err(source.into())
        }
    }
}

fn record_group_bot_outbound_send(
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    ref_index: &SharedRefIndex,
    message: &GroupMessage,
    response: &RespondResponse,
    config: &AppConfig,
    sent_ids: &SendMessageIds,
    text: &str,
) {
    {
        let mut cache = group_outbound_cache.lock().unwrap();
        cache.insert(sent_ids.message_id.clone());
        cache.insert_ref_index_id(sent_ids.ref_index_id.clone());
    }
    let inbound = platform::qq_official::inbound_from_group(message);
    ref_index.lock().unwrap().insert_bot_outbound(
        platform::Platform::QqOfficial,
        config.app_id.as_deref(),
        &inbound.conversation,
        sent_ids.ref_index_lookup_id().map(str::to_owned),
        text,
        response.visible_entity_snapshot.clone(),
    );
}

#[derive(Debug)]
enum GroupStreamOutcome {
    Completed(RespondResponse),
    Failed(CoreRespondFailure),
    ClosedBeforeCompleted,
}

async fn consume_respond_stream<E>(mut stream: E) -> GroupStreamOutcome
where
    E: RespondEventStream,
{
    let output_policy = stream.output_policy();
    let mut status_event_count = 0_usize;
    let mut text_delta_count = 0_usize;
    while let Some(event) = stream.recv_event().await {
        match event {
            RespondEvent::Status(status) => {
                status_event_count += 1;
                debug!(
                    status_kind = status.kind.as_str(),
                    response_delivery_mode = "progress_status",
                    status_chars = status.text.chars().count(),
                    status_event_count,
                    "group stream status event recorded without group progress send"
                );
            }
            RespondEvent::TextDelta(delta) => {
                if !delta.is_empty() {
                    text_delta_count += 1;
                }
            }
            RespondEvent::Completed(response) => {
                debug!(
                    response_delivery_mode = output_policy.as_str(),
                    text_delta_count,
                    status_event_count,
                    "group stream collapsed into single Completed response"
                );
                return GroupStreamOutcome::Completed(*response);
            }
            RespondEvent::Failed(failure) => {
                warn!(
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    response_delivery_mode = output_policy.as_str(),
                    text_delta_count,
                    status_event_count,
                    "core respond stream failed"
                );
                return GroupStreamOutcome::Failed(failure);
            }
        }
    }
    GroupStreamOutcome::ClosedBeforeCompleted
}

async fn send_group_stream_failure<S>(
    sender: &S,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    failure: &CoreRespondFailure,
) -> anyhow::Result<()>
where
    S: GroupOutboundSender + ?Sized,
{
    let target = ReplyTarget::qq_group(
        message.group_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_group_target()
    .expect("QQ group reply target should adapt to QQ API target");
    // failure.message 由 Core 按失败类型映射为安全用户文案；Gateway 只负责真实发送，
    // 不把上游原始错误、工具结果或模型中间内容暴露到群聊。
    let sent_ids = sender.send_text(&target, &failure.message).await?;
    let mut cache = group_outbound_cache.lock().unwrap();
    cache.insert(sent_ids.message_id);
    cache.insert_ref_index_id(sent_ids.ref_index_id);
    Ok(())
}

fn log_group_message_received(message: &GroupMessage, verbose_log: bool) {
    let summary = group_message_log_summary(message, verbose_log);
    if let Some(extracted_content) = summary.extracted_content.as_deref() {
        info!(
            message_id = %summary.message_id,
            group = %summary.masked_group,
            member = %summary.masked_member.as_deref().unwrap_or(""),
            event_type = summary.event_type,
            content_len = summary.content_len,
            mention_count = summary.mention_count,
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
            event_type = summary.event_type,
            content_len = summary.content_len,
            mention_count = summary.mention_count,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            "received group message"
        );
    }
}

#[cfg(test)]
mod tests;
