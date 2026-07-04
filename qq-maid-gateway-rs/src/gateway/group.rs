//! 群消息处理管道。
//!
//! 这里串起群消息过滤、Core 调用、QQ 群回复发送和机器人 outbound id 回填。
//! 群触发策略与冷却的纯判定逻辑放在 `group_filter.rs`，避免处理管道继续膨胀。

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use tracing::{debug, info, warn};

use super::{
    bot_identity::SharedBotIdentity,
    cache::BotOutboundCache,
    dedupe::MessageDedupe,
    event::{GroupEventType, GroupMessage},
    group_filter::{GroupCooldowns, should_ignore_group_message, should_process_group_message},
    logging::{group_message_log_summary, mask_openid},
    outbound::{RuntimeRecordingGroupSender, send_group_text_with_status},
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::{GroupReplyTarget, QqApiClient},
    config::AppConfig,
    message_chunk::{ChunkLimits, OutboundSendError, send_group_outbound_chunked},
    render::{OutboundMessage, render_respond_response},
    respond::{
        RespondClient, RespondEvent, RespondResponse, RespondTransport, respond_error_to_qq_text,
    },
};

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
    bot_identity: &SharedBotIdentity,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content =
        crate::respond::build_group_respond_content(&message, &config.group_active_keywords);
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
    let outbound = prefix_group_reply_outbound(message, outbound);
    let sender = RuntimeRecordingGroupSender {
        inner: api,
        runtime,
    };
    let target = GroupReplyTarget {
        group_openid: message.group_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    let limits = ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    // 普通群回复统一走分段编排：每个成功发送并返回 message id 的分段写入
    // `BotOutboundCache`；失败分段不写，错误向上传递为 PartiallySent / NotSent。
    match send_group_outbound_chunked(
        &sender,
        &target,
        &outbound,
        &limits,
        |_, sent_message_id| {
            group_outbound_cache
                .lock()
                .unwrap()
                .insert(sent_message_id.clone());
        },
    )
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

async fn consume_respond_stream(
    mut stream: qq_maid_core::service::CoreResponseStream,
) -> Option<RespondResponse> {
    let output_policy = stream.output_policy();
    let mut status_event_count = 0_usize;
    let mut text_delta_count = 0_usize;
    while let Some(event) = stream.recv().await {
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
                return Some(response);
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
                return None;
            }
        }
    }
    None
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
mod tests {
    use super::*;

    fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("member-1".to_owned()),
            content: content.to_owned(),
            mention_ids: Vec::new(),
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
}
