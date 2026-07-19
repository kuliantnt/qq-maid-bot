//! C2C 私聊消息处理管道。
//!
//! 私聊链路负责本地 `/ping`、Signal Layer 回填、Core 调用和普通回复发送；
//! C2C 流式发送状态机独立放在 `stream.rs`。

use std::{future::Future, pin::Pin};

use anyhow::Context;
use tracing::{debug, info, warn};

use super::super::{
    command::{GatewayCommandContext, GatewayCommandConversation, GatewayCommandService},
    dedupe::MessageDedupe,
    event::C2cMessage,
    logging::{c2c_message_log_summary, mask_openid},
    media_fetch::{MediaFetchContext, fetch_qq_official_image_attachments},
    outbound::{
        DeliveryMode, ReplyCapability, ReplyTarget, RuntimeRecordingSender,
        send_c2c_text_with_status,
    },
    ping::GatewayRuntimeStatus,
    platform,
    ref_index::SharedRefIndex,
    stream::stream_respond_c2c,
    typing::{C2cTypingStatusGuard, TypingStopReason},
};
use crate::{
    api::{OutboundSender, QqApiClient, SendMessageIds, send_outbound_with_fallback},
    config::AppConfig,
    message_chunk::{ChunkLimits, OutboundSendError, send_c2c_outbound_chunked},
    render::{OutboundMessage, render_respond_response_for_profile},
    respond::{
        RespondClient, RespondEvent, RespondResponse, RespondTransport, build_respond_content,
        respond_error_to_qq_text,
    },
};
use qq_maid_core::service::{
    CoreFailureKind, CoreInboundKind, CoreOutputPolicy, CoreRespondFailure, CoreResponseStatus,
};

const CORE_STREAM_CLOSED_FALLBACK_TEXT: &str = "处理失败，请稍后再试。";

fn empty_reply_fallback_text(bot_display_name: &str) -> String {
    format!("唔，{bot_display_name}刚刚没整理出可用回复。可以再说一次。")
}

type RespondEventFuture<'a> = Pin<Box<dyn Future<Output = Option<RespondEvent>> + Send + 'a>>;

trait RespondEventStream: Send {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a>;
    fn output_policy(&self) -> CoreOutputPolicy;
}

impl RespondEventStream for qq_maid_core::service::CoreResponseStream {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
        Box::pin(async move { self.recv().await })
    }

    fn output_policy(&self) -> CoreOutputPolicy {
        self.output_policy()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisabledStreamOutcome {
    Completed,
    Failed(CoreFailureKind),
    ClosedBeforeCompleted,
}

/// 发送 C2C 普通（非流式）回复消息，供真实网关入口调用。
async fn send_c2c_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    let capability = ReplyCapability::qq_official_c2c(config);
    let (sent_ids, fallback_text) =
        send_c2c_respond_response_with_sender(&sender, message, response, config, &capability)
            .await?;
    record_c2c_bot_outbound_refs(
        ref_index,
        message,
        config,
        sent_ids,
        &fallback_text,
        response.visible_entity_snapshot.clone(),
    );
    Ok(())
}

pub(crate) fn record_c2c_bot_outbound_refs(
    ref_index: &SharedRefIndex,
    message: &C2cMessage,
    config: &AppConfig,
    sent_ids: impl IntoIterator<Item = SendMessageIds>,
    text: &str,
    visible_entity_snapshot: Option<qq_maid_core::service::VisibleEntitySnapshot>,
) {
    let inbound = platform::qq_official::inbound_from_c2c(message);
    let mut index = ref_index.lock().unwrap();
    for sent_id in sent_ids.into_iter() {
        index.insert_bot_outbound(
            platform::Platform::QqOfficial,
            config.app_id.as_deref(),
            &inbound.conversation,
            sent_id.ref_index_lookup_id().map(str::to_owned),
            text,
            visible_entity_snapshot.clone(),
        );
    }
}

/// 普通 C2C 回复发送的共享实现。
///
/// 流式 fallback 必须走这里，才能保留 Markdown、文本 fallback、图片开关、reply target
/// 以及发送状态记录等既有语义。
pub(crate) async fn send_c2c_respond_response_with_sender<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
    capability: &ReplyCapability,
) -> anyhow::Result<(Vec<SendMessageIds>, String)> {
    let masked_user = mask_openid(&message.user_openid);
    let outbound = match render_respond_response_for_profile(response, &capability.render) {
        Some(outbound) => outbound,
        None => {
            warn!(
            message_id = %message.message_id,
            user = %masked_user,
            fallback_reason = "empty_rendered_response",
            "respond backend produced no reply text; sending local fallback"
            );
            OutboundMessage::Text {
                text: empty_reply_fallback_text(config.bot_display_name()),
            }
        }
    };

    let target = ReplyTarget::qq_c2c(
        message.user_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_c2c_target()
    .expect("QQ C2C reply target should adapt to QQ API target");
    debug!(
        message_id = target.msg_id.as_deref().unwrap_or(""),
        user = %masked_user,
        reply_len = outbound.fallback_text().chars().count(),
        "preparing QQ reply"
    );
    let limits = ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    // 普通回复统一走分段编排：长回复拆成多条逐段发送，段间失败返回 PartiallySent。
    let fallback_text = outbound.fallback_text().to_owned();
    match send_c2c_outbound_chunked(sender, &target, &outbound, &limits, |_, _| {}).await {
        Ok(sent_ids) => Ok((sent_ids, fallback_text)),
        Err(OutboundSendError::NotSent { source }) => {
            warn!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %masked_user,
                error = %source.log_summary(),
                "QQ reply send failed before any chunk was sent"
            );
            Err(source.into())
        }
        Err(OutboundSendError::PartiallySent {
            source,
            sent_chunks,
            total_chunks,
            failed_chunk_index,
            remaining_chars,
        }) => {
            warn!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %masked_user,
                error = %source.log_summary(),
                sent_chunks,
                total_chunks,
                failed_chunk_index,
                remaining_chars,
                "QQ reply partially sent; some chunks already delivered"
            );
            Err(source.into())
        }
    }
}

// 私聊消息处理需要贯穿 QQ 回复、LLM 调用、去重和诊断状态，保持参数显式便于看清跨层依赖。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_c2c_message(
    mut message: C2cMessage,
    config: &AppConfig,
    commands: &GatewayCommandService,
    respond: &RespondClient,
    api: &QqApiClient,
    _dedupe: &MessageDedupe,
    ref_index: &SharedRefIndex,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    log_c2c_message_received(&message, config.verbose_log);
    runtime.record_c2c_message_received(&message);

    let masked_user = mask_openid(&message.user_openid);
    let respond_content = build_respond_content(&message);
    if respond_content.trim().is_empty()
        && message.reply.is_none()
        && message.input_parts.is_empty()
        && message.attachments.is_empty()
    {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "ignoring empty C2C message"
        );
        return Ok(());
    }
    // C2C message/event ID 已在 Aggregator 入口原子 reservation；这里不能再按逻辑批次任意 source ID 命中丢弃整批。
    let command_context = GatewayCommandContext {
        platform_name: "QQ 官方机器人",
        platform_code: "qq_official_gateway_rs",
        event_name: "C2C 消息",
        conversation: GatewayCommandConversation::Private,
        user_id: Some(message.user_openid.clone()),
        group_id: None,
        message_id: Some(message.message_id.clone()),
        timestamp: message.timestamp.clone(),
        attachment_count: message.attachments.len(),
    };
    if let Some(output) = commands
        .try_handle(&message.content, &command_context)
        .await
    {
        info!(
            message_id = %message.message_id,
            user = %masked_user,
            "local /ping command matched"
        );
        let target = ReplyTarget::qq_c2c(message.user_openid, Some(message.message_id))
            .to_qq_c2c_target()
            .expect("QQ C2C reply target should adapt to QQ API target");
        let capability = ReplyCapability::qq_official_c2c(config);
        let outbound = output.render(&capability);
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
    fetch_qq_official_image_attachments(
        &qq_maid_common::http_client::client(),
        &MediaFetchContext {
            platform: "qq_official",
            app_id: config
                .app_id
                .clone()
                .context("QQ C2C handler requires a bound channel")?,
            peer_id: message.user_openid.clone(),
            root_dir: config.media_dir.clone(),
            timeout: config.media_download_timeout,
            max_bytes: config.media_max_bytes,
        },
        &message.message_id,
        &mut message.input_parts,
        &message.attachments,
    )
    .await;

    let mut inbound = respond.prepare_inbound(platform::qq_official::inbound_from_c2c(&message));
    {
        let mut index = ref_index.lock().unwrap();
        index.enrich_inbound(&mut inbound);
        index.insert_inbound(&inbound);
    }

    info!(
        message_id = %message.message_id,
        user = %masked_user,
        "calling respond backend"
    );
    let mut typing = schedule_agent_typing_if_needed(
        config,
        respond,
        api.clone(),
        &message,
        &inbound,
        respond_content.clone(),
    )
    .await;
    let transport = match respond.respond_inbound(&inbound, respond_content).await {
        Ok(response) => {
            runtime.record_respond_success();
            response
        }
        Err(err) => {
            stop_typing(&mut typing, TypingStopReason::RequestFailed);
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
            stop_typing(&mut typing, TypingStopReason::FinalReply);
            send_c2c_respond_response(api, runtime, &message, &response, config, ref_index).await?;
        }
        RespondTransport::Stream(stream) => {
            let capability = ReplyCapability::qq_official_c2c(config);
            if should_use_c2c_streaming(&capability) {
                stream_respond_c2c(stream, api, runtime, &message, config, typing, ref_index)
                    .await?;
            } else {
                let sender = RuntimeRecordingSender {
                    inner: api,
                    runtime,
                };
                let outcome = handle_c2c_stream_disabled(
                    stream,
                    &sender,
                    &message,
                    config,
                    &mut typing,
                    Some(ref_index),
                )
                .await?;
                match outcome {
                    DisabledStreamOutcome::Completed => {}
                    DisabledStreamOutcome::Failed(kind) => runtime
                        .record_respond_failure(format!("stream_failed_before_completed:{kind:?}")),
                    DisabledStreamOutcome::ClosedBeforeCompleted => {
                        runtime.record_respond_failure("stream_closed_before_completed")
                    }
                }
            }
        }
    }
    Ok(())
}

async fn schedule_agent_typing_if_needed(
    config: &AppConfig,
    respond: &RespondClient,
    api: QqApiClient,
    message: &C2cMessage,
    inbound: &platform::InboundMessage,
    respond_content: String,
) -> Option<C2cTypingStatusGuard> {
    if !config.agent_typing.enabled {
        return None;
    }
    match respond.classify_inbound(inbound, respond_content).await {
        Ok(classification) if classification.kind == CoreInboundKind::NormalChat => {
            C2cTypingStatusGuard::schedule(&config.agent_typing, api, message, "c2c")
        }
        Ok(_) => None,
        Err(error) => {
            warn!(
                message_id = %message.message_id,
                user = %mask_openid(&message.user_openid),
                error = %error.log_summary(),
                "agent typing classification failed; skipping typing status"
            );
            None
        }
    }
}

fn stop_typing(typing: &mut Option<C2cTypingStatusGuard>, reason: TypingStopReason) {
    if let Some(typing) = typing.as_mut() {
        typing.stop(reason);
    }
}

async fn handle_c2c_stream_disabled<E, S>(
    mut stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
    typing: &mut Option<C2cTypingStatusGuard>,
    ref_index: Option<&SharedRefIndex>,
) -> anyhow::Result<DisabledStreamOutcome>
where
    E: RespondEventStream,
    S: OutboundSender + ?Sized,
{
    let output_policy = stream.output_policy();
    let mut text_delta_count = 0_usize;
    let mut status_event_count = 0_usize;
    let mut progress_status_send_attempted = false;
    while let Some(event) = stream.recv_event().await {
        match event {
            RespondEvent::Status(status) => {
                status_event_count += 1;
                debug!(
                    message_id = %message.message_id,
                    user = %mask_openid(&message.user_openid),
                    status_kind = status.kind.as_str(),
                    response_delivery_mode = "progress_status",
                    status_chars = status.text.chars().count(),
                    status_event_count,
                    "C2C stream disabled; status event recorded without separate final send"
                );
                if should_send_disabled_progress_status(
                    config.c2c_visible_progress_status_enabled,
                    output_policy,
                    progress_status_send_attempted,
                ) {
                    progress_status_send_attempted = true;
                    send_disabled_progress_status(sender, message, &status).await;
                }
            }
            RespondEvent::TextDelta(delta) => {
                if !delta.is_empty() {
                    text_delta_count += 1;
                }
            }
            RespondEvent::Completed(response) => {
                stop_typing(typing, TypingStopReason::FinalReply);
                let capability = ReplyCapability::qq_official_c2c(config);
                let (sent_ids, fallback_text) = send_c2c_respond_response_with_sender(
                    sender,
                    message,
                    &response,
                    config,
                    &capability,
                )
                .await
                .inspect(|_| {
                    debug!(
                        message_id = %message.message_id,
                        user = %mask_openid(&message.user_openid),
                        response_delivery_mode = output_policy.as_str(),
                        final_send_exit = "ordinary_reply",
                        text_delta_count,
                        status_event_count,
                        "C2C stream disabled; ordinary final reply sent"
                    );
                })
                .inspect_err(|send_err| {
                    warn!(
                        message_id = %message.message_id,
                        user = %mask_openid(&message.user_openid),
                        response_delivery_mode = output_policy.as_str(),
                        final_send_exit = "ordinary_reply",
                        text_delta_count,
                        status_event_count,
                        error = %send_err,
                        "C2C stream disabled; ordinary final reply failed"
                    );
                })?;
                if let Some(ref_index) = ref_index {
                    record_c2c_bot_outbound_refs(
                        ref_index,
                        message,
                        config,
                        sent_ids,
                        &fallback_text,
                        response.visible_entity_snapshot.clone(),
                    );
                }
                return Ok(DisabledStreamOutcome::Completed);
            }
            RespondEvent::Failed(failure) => {
                stop_typing(typing, failure_stop_reason(&failure));
                warn!(
                    message_id = %message.message_id,
                    user = %mask_openid(&message.user_openid),
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    text_delta_count,
                    status_event_count,
                    "core respond stream failed while C2C stream was disabled"
                );
                send_local_c2c_failure_text(sender, message, &failure.message).await?;
                return Ok(DisabledStreamOutcome::Failed(failure.kind));
            }
        }
    }
    stop_typing(typing, TypingStopReason::Cancelled);
    warn!(
        message_id = %message.message_id,
        user = %mask_openid(&message.user_openid),
        "core respond stream closed before Completed while C2C stream was disabled"
    );
    send_local_c2c_failure_text(sender, message, CORE_STREAM_CLOSED_FALLBACK_TEXT).await?;
    Ok(DisabledStreamOutcome::ClosedBeforeCompleted)
}

fn should_send_disabled_progress_status(
    enabled: bool,
    policy: CoreOutputPolicy,
    attempted: bool,
) -> bool {
    enabled
        && !attempted
        && matches!(
            policy,
            CoreOutputPolicy::ProgressThenComplete | CoreOutputPolicy::ProgressThenStream
        )
}

async fn send_disabled_progress_status<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    status: &CoreResponseStatus,
) {
    let target = ReplyTarget::qq_c2c(
        message.user_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_c2c_target()
    .expect("QQ C2C reply target should adapt to QQ API target");
    // progress status 是 Core 生成的受控短提示，失败只记录，不影响最终回复。
    match sender.send_text(&target, &status.text).await {
        Ok(_) => {
            debug!(
                message_id = %message.message_id,
                user = %mask_openid(&message.user_openid),
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                "C2C stream disabled; progress status sent"
            );
        }
        Err(error) => {
            warn!(
                message_id = %message.message_id,
                user = %mask_openid(&message.user_openid),
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                error = %error,
                "C2C stream disabled; progress status send failed"
            );
        }
    }
}

pub(crate) async fn send_local_c2c_failure_text<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    text: &str,
) -> anyhow::Result<SendMessageIds> {
    let target = ReplyTarget::qq_c2c(
        message.user_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_c2c_target()
    .expect("QQ C2C reply target should adapt to QQ API target");
    Ok(sender.send_text(&target, text).await?)
}

fn failure_stop_reason(failure: &CoreRespondFailure) -> TypingStopReason {
    match failure.kind {
        CoreFailureKind::SearchTimeout | CoreFailureKind::LlmTimeout => TypingStopReason::Timeout,
        _ => TypingStopReason::RequestFailed,
    }
}

fn should_use_c2c_streaming(capability: &ReplyCapability) -> bool {
    debug_assert!(
        capability.supports_delivery_mode(DeliveryMode::AsynchronousReply)
            || capability.supports_delivery_mode(DeliveryMode::SynchronousReply),
        "reply capability must expose at least one non-stream delivery mode"
    );
    capability.supports_delivery_mode(DeliveryMode::Streaming)
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

#[cfg(test)]
mod tests;
