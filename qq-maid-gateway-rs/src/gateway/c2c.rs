//! C2C 私聊消息处理管道。
//!
//! 私聊链路负责本地 `/ping`、Signal Layer 回填、Core 调用和普通回复发送；
//! C2C 流式发送状态机独立放在 `stream.rs`。

use std::{future::Future, pin::Pin};

use tracing::{debug, info, warn};

use super::{
    cache::{ReplyCache, resolve_signals},
    dedupe::MessageDedupe,
    event::C2cMessage,
    logging::{c2c_message_log_summary, mask_openid},
    media_fetch::{MediaFetchContext, fetch_qq_official_image_attachments},
    outbound::{
        DeliveryMode, ReplyCapability, ReplyTarget, RuntimeRecordingSender,
        send_c2c_text_with_status,
    },
    ping::{
        GatewayRuntimeStatus, build_c2c_ping_reply_with_check_failure, is_ping_check_command,
        is_ping_command,
    },
    stream::stream_respond_c2c,
    typing::{C2cTypingStatusGuard, TypingStopReason},
};
use crate::{
    api::{OutboundSender, QqApiClient, send_outbound_with_fallback},
    auth::AccessTokenManager,
    config::AppConfig,
    markdown::MarkdownPayload,
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
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    let capability = ReplyCapability::qq_official_c2c(config);
    send_c2c_respond_response_with_sender(&sender, message, response, config, &capability).await
}

/// 普通 C2C 回复发送的共享实现。
///
/// 流式 fallback 必须走这里，才能保留 Markdown、文本 fallback、图片开关、reply target
/// 以及发送状态记录等既有语义。
pub(super) async fn send_c2c_respond_response_with_sender<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
    capability: &ReplyCapability,
) -> anyhow::Result<()> {
    let masked_user = mask_openid(&message.user_openid);
    let Some(outbound) = render_respond_response_for_profile(response, &capability.render) else {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "respond backend produced no reply text"
        );
        return Ok(());
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
    match send_c2c_outbound_chunked(sender, &target, &outbound, &limits, |_, _| {}).await {
        Ok(_) => Ok(()),
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
        let target = ReplyTarget::qq_c2c(message.user_openid, Some(message.message_id))
            .to_qq_c2c_target()
            .expect("QQ C2C reply target should adapt to QQ API target");
        let capability = ReplyCapability::qq_official_c2c(config);
        let outbound = render_local_ping_reply(reply, &capability);
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
        &reqwest::Client::new(),
        &MediaFetchContext {
            platform: "qq_official",
            app_id: config.app_id.clone(),
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
        respond_content.clone(),
    )
    .await;
    let transport = match respond.respond_c2c(&message, respond_content).await {
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
            send_c2c_respond_response(api, runtime, &message, &response, config).await?;
        }
        RespondTransport::Stream(stream) => {
            let capability = ReplyCapability::qq_official_c2c(config);
            if should_use_c2c_streaming(&capability) {
                stream_respond_c2c(stream, api, runtime, &message, config, typing).await?;
            } else {
                let sender = RuntimeRecordingSender {
                    inner: api,
                    runtime,
                };
                let outcome =
                    handle_c2c_stream_disabled(stream, &sender, &message, config, &mut typing)
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
    respond_content: String,
) -> Option<C2cTypingStatusGuard> {
    if !config.agent_typing.enabled {
        return None;
    }
    match respond.classify_c2c(message, respond_content).await {
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
                send_c2c_respond_response_with_sender(
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
                        response_delivery_mode = "ordinary_complete",
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
                        response_delivery_mode = "ordinary_complete",
                        final_send_exit = "ordinary_reply",
                        text_delta_count,
                        status_event_count,
                        error = %send_err,
                        "C2C stream disabled; ordinary final reply failed"
                    );
                })?;
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

async fn send_local_c2c_failure_text<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    text: &str,
) -> anyhow::Result<()> {
    let target = ReplyTarget::qq_c2c(
        message.user_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_c2c_target()
    .expect("QQ C2C reply target should adapt to QQ API target");
    sender.send_text(&target, text).await?;
    Ok(())
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

fn render_local_ping_reply(reply: String, capability: &ReplyCapability) -> OutboundMessage {
    if capability.render.supports_markdown {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::{ApiError, C2cReplyTarget, SendFuture},
        config::{
            AgentTypingConfig, DEFAULT_CONVERSATION_QUEUE_CAPACITY,
            DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT, DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            DEFAULT_MEDIA_MAX_BYTES, DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
            DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS, DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
            DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS, DEFAULT_MESSAGE_AGGREGATION_QUIET_MS,
            DEFAULT_TEXT_CHUNK_SOFT_LIMIT, GroupMessageMode, MessageAggregationConfig,
        },
        media::ImagePayload,
    };
    use qq_maid_core::service::{CoreRespondFailure, CoreResponseStatus, CoreResponseStatusKind};
    use std::{collections::VecDeque, sync::Mutex, time::Duration};

    #[derive(Debug)]
    struct FakeEventStream {
        events: VecDeque<RespondEvent>,
        output_policy: CoreOutputPolicy,
    }

    impl FakeEventStream {
        fn new(events: impl IntoIterator<Item = RespondEvent>) -> Self {
            Self {
                events: events.into_iter().collect(),
                output_policy: CoreOutputPolicy::DirectStream,
            }
        }

        fn with_policy(mut self, output_policy: CoreOutputPolicy) -> Self {
            self.output_policy = output_policy;
            self
        }
    }

    impl RespondEventStream for FakeEventStream {
        fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
            Box::pin(async move { self.events.pop_front() })
        }

        fn output_policy(&self) -> CoreOutputPolicy {
            self.output_policy
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeCall {
        Text {
            content: String,
            msg_id: Option<String>,
        },
        Markdown {
            content: String,
            msg_id: Option<String>,
        },
        Image,
    }

    #[derive(Debug, Default)]
    struct FakeOutboundSender {
        calls: Mutex<Vec<FakeCall>>,
    }

    impl FakeOutboundSender {
        fn calls(&self) -> Vec<FakeCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OutboundSender for FakeOutboundSender {
        fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Text {
                    content: text.to_owned(),
                    msg_id: target.msg_id.clone(),
                });
                Ok(Some("text-id".to_owned()))
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
                Ok(Some("markdown-id".to_owned()))
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
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("晚上好")],
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
            bot_mention_ids: Vec::new(),
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
            c2c_final_reply_stream_enabled: false,
            c2c_visible_progress_status_enabled: true,
            agent_typing: AgentTypingConfig {
                enabled: false,
                delay: Duration::from_secs(1),
            },
            markdown_chunk_soft_limit: DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
            text_chunk_soft_limit: DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
            media_dir: std::path::PathBuf::from("media/inbound"),
            media_download_timeout: Duration::from_secs(10),
            media_max_bytes: DEFAULT_MEDIA_MAX_BYTES,
            wechat_service: crate::config::WechatServiceConfig::default(),
        }
    }

    #[test]
    fn local_ping_reply_respects_markdown_config() {
        let markdown_config = test_config();
        let markdown_capability = ReplyCapability::qq_official_c2c(&markdown_config);
        let markdown =
            render_local_ping_reply("# 状态\n\n| A | B |".to_owned(), &markdown_capability);
        assert_eq!(
            markdown,
            OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("# 状态\n\n| A | B |"),
                fallback_text: "# 状态\n\n| A | B |".to_owned(),
            }
        );

        let mut text_config = test_config();
        text_config.enable_markdown = false;
        let text_capability = ReplyCapability::qq_official_c2c(&text_config);
        let text = render_local_ping_reply("# 状态".to_owned(), &text_capability);
        assert_eq!(
            text,
            OutboundMessage::Text {
                text: "# 状态".to_owned(),
            }
        );
    }

    #[test]
    fn c2c_stream_branch_requires_stream_capability() {
        let mut config = test_config();
        config.c2c_final_reply_stream_enabled = true;
        let streaming = ReplyCapability::qq_official_c2c(&config);
        assert!(should_use_c2c_streaming(&streaming));

        config.c2c_final_reply_stream_enabled = false;
        let ordinary = ReplyCapability::qq_official_c2c(&config);
        assert!(!should_use_c2c_streaming(&ordinary));
    }

    #[tokio::test]
    async fn disabled_stream_completed_sends_single_ordinary_reply() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("不应外发".to_owned()),
            RespondEvent::Completed(respond_response("最终回复")),
        ]);
        let sender = FakeOutboundSender::default();
        let mut typing = None;

        let outcome = handle_c2c_stream_disabled(
            events,
            &sender,
            &c2c_message(),
            &test_config(),
            &mut typing,
        )
        .await
        .unwrap();

        assert_eq!(outcome, DisabledStreamOutcome::Completed);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Markdown {
                content: "最终回复".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn disabled_stream_status_does_not_create_extra_reply() {
        let events = FakeEventStream::new([
            RespondEvent::Status(CoreResponseStatus {
                kind: CoreResponseStatusKind::ToolLoopStarted,
                text: "正在处理".to_owned(),
            }),
            RespondEvent::Completed(respond_response("最终回复")),
        ]);
        let sender = FakeOutboundSender::default();
        let mut typing = None;

        let outcome = handle_c2c_stream_disabled(
            events,
            &sender,
            &c2c_message(),
            &test_config(),
            &mut typing,
        )
        .await
        .unwrap();

        assert_eq!(outcome, DisabledStreamOutcome::Completed);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Markdown {
                content: "最终回复".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn disabled_stream_progress_policy_sends_one_visible_hint_then_final_reply() {
        let events = FakeEventStream::new([
            RespondEvent::Status(CoreResponseStatus {
                kind: CoreResponseStatusKind::ToolLoopStarted,
                text: "小女仆正在处理…".to_owned(),
            }),
            RespondEvent::Status(CoreResponseStatus {
                kind: CoreResponseStatusKind::ToolLoopFinalizing,
                text: "小女仆正在确认结果…".to_owned(),
            }),
            RespondEvent::Completed(respond_response("最终回复")),
        ])
        .with_policy(CoreOutputPolicy::ProgressThenComplete);
        let sender = FakeOutboundSender::default();
        let mut typing = None;

        let outcome = handle_c2c_stream_disabled(
            events,
            &sender,
            &c2c_message(),
            &test_config(),
            &mut typing,
        )
        .await
        .unwrap();

        assert_eq!(outcome, DisabledStreamOutcome::Completed);
        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Text {
                    content: "小女仆正在处理…".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                },
                FakeCall::Markdown {
                    content: "最终回复".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                }
            ]
        );
    }

    #[tokio::test]
    async fn disabled_stream_progress_status_respects_visible_progress_config() {
        let events = FakeEventStream::new([
            RespondEvent::Status(CoreResponseStatus {
                kind: CoreResponseStatusKind::ToolLoopStarted,
                text: "小女仆正在处理…".to_owned(),
            }),
            RespondEvent::Completed(respond_response("最终回复")),
        ])
        .with_policy(CoreOutputPolicy::ProgressThenComplete);
        let sender = FakeOutboundSender::default();
        let mut typing = None;
        let mut config = test_config();
        config.c2c_visible_progress_status_enabled = false;

        let outcome =
            handle_c2c_stream_disabled(events, &sender, &c2c_message(), &config, &mut typing)
                .await
                .unwrap();

        assert_eq!(outcome, DisabledStreamOutcome::Completed);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Markdown {
                content: "最终回复".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn disabled_stream_failed_sends_safe_failure_without_reinvoking_core() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("不完整".to_owned()),
            RespondEvent::Failed(CoreRespondFailure {
                kind: CoreFailureKind::LlmFailed,
                message: "上游服务暂时不可用，请稍后再试。".to_owned(),
                retryable: true,
            }),
        ]);
        let sender = FakeOutboundSender::default();
        let mut typing = None;

        let outcome = handle_c2c_stream_disabled(
            events,
            &sender,
            &c2c_message(),
            &test_config(),
            &mut typing,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            DisabledStreamOutcome::Failed(CoreFailureKind::LlmFailed)
        );
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Text {
                content: "上游服务暂时不可用，请稍后再试。".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn disabled_stream_closed_before_completed_sends_fixed_failure_not_delta() {
        let events = FakeEventStream::new([RespondEvent::TextDelta("半截回复".to_owned())]);
        let sender = FakeOutboundSender::default();
        let mut typing = None;

        let outcome = handle_c2c_stream_disabled(
            events,
            &sender,
            &c2c_message(),
            &test_config(),
            &mut typing,
        )
        .await
        .unwrap();

        assert_eq!(outcome, DisabledStreamOutcome::ClosedBeforeCompleted);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Text {
                content: CORE_STREAM_CLOSED_FALLBACK_TEXT.to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }
}
