//! OneBot 11 入站到 Core 与文本 sender 的最小闭环。
//!
//! 本模块只编排统一入站模型、CoreService、结构化输出渲染和 OneBot sender；命令、
//! Todo、Memory、Pending 与 Tool Loop 仍完全由 Core 的既有场景策略决定。

use std::{future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use qq_maid_common::command_prefix::CommandPrefix;
use qq_maid_core::service::{
    CoreFailureKind, CoreRespondFailure, CoreRespondOutput, CoreResponse, CoreResponseEvent,
    CoreResponseStream, VisibleEntitySnapshot,
};
use thiserror::Error;
use tracing::{debug, warn};

use crate::{
    gateway::{
        command::{GatewayCommandContext, GatewayCommandConversation, GatewayCommandService},
        platform::{self, ConversationTarget, InboundMessage},
        ref_index::SharedRefIndex,
    },
    media::ImagePayload,
    render::{OutboundMessage, render_respond_response_parts_for_profile},
    respond::{RespondClient, RespondError, respond_error_to_qq_text},
};

use super::{OneBotSendError, OneBotSendResult, OneBotSender};

const STREAM_FAILED_TEXT: &str = "处理失败，请稍后再试。";
const STREAM_TIMEOUT_TEXT: &str = "LLM 请求超时，请稍后重试。";
const STREAM_CANCELLED_TEXT: &str = "本次处理已取消，请重新发送消息。";

type EventFuture<'a> = Pin<Box<dyn Future<Output = Option<CoreResponseEvent>> + Send + 'a>>;

/// Core 流事件的最小抽象，使 OneBot 非流式收口逻辑可使用 fake Core 覆盖。
trait OneBotResponseEventStream: Send {
    fn recv_event<'a>(&'a mut self) -> EventFuture<'a>;
}

impl OneBotResponseEventStream for CoreResponseStream {
    fn recv_event<'a>(&'a mut self) -> EventFuture<'a> {
        Box::pin(async move { self.recv().await })
    }
}

enum OneBotCoreTransport {
    Complete(Box<CoreResponse>),
    Stream(Box<dyn OneBotResponseEventStream>),
}

#[async_trait]
trait OneBotCoreResponder: Send + Sync {
    async fn respond(
        &self,
        inbound: &InboundMessage,
        content: String,
    ) -> Result<OneBotCoreTransport, RespondError>;
}

#[async_trait]
impl OneBotCoreResponder for RespondClient {
    async fn respond(
        &self,
        inbound: &InboundMessage,
        content: String,
    ) -> Result<OneBotCoreTransport, RespondError> {
        match self.respond_inbound(inbound, content).await? {
            CoreRespondOutput::Complete(response) => Ok(OneBotCoreTransport::Complete(response)),
            CoreRespondOutput::Stream(stream) => Ok(OneBotCoreTransport::Stream(Box::new(stream))),
        }
    }
}

#[async_trait]
trait OneBotReplySender: Send + Sync {
    async fn send_private_text(
        &self,
        user_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError>;

    async fn send_group_text(
        &self,
        group_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError>;

    async fn send_group_text_with_mentions(
        &self,
        group_id: &str,
        mention_user_ids: &[String],
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let _ = mention_user_ids;
        self.send_group_text(group_id, text).await
    }

    async fn send_private_image(
        &self,
        user_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError>;

    async fn send_group_image(
        &self,
        group_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError>;

    async fn send_group_image_with_mentions(
        &self,
        group_id: &str,
        mention_user_ids: &[String],
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let _ = mention_user_ids;
        self.send_group_image(group_id, image).await
    }
}

#[async_trait]
impl OneBotReplySender for OneBotSender {
    async fn send_private_text(
        &self,
        user_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_private_text(self, user_id, text).await
    }

    async fn send_group_text(
        &self,
        group_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_text(self, group_id, text).await
    }

    async fn send_group_text_with_mentions(
        &self,
        group_id: &str,
        mention_user_ids: &[String],
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_text_with_mentions(self, group_id, mention_user_ids, text).await
    }

    async fn send_private_image(
        &self,
        user_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_private_image(self, user_id, image).await
    }

    async fn send_group_image(
        &self,
        group_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_image(self, group_id, image).await
    }

    async fn send_group_image_with_mentions(
        &self,
        group_id: &str,
        mention_user_ids: &[String],
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_image_with_mentions(self, group_id, mention_user_ids, image).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OneBotDispatchOutcome {
    Sent,
    IgnoredNonBotReply,
    SuppressedByCore,
}

#[derive(Debug, Error)]
pub(super) enum OneBotDispatchError {
    #[error("OneBot Core request failed: {summary}")]
    Core { summary: String },
    #[error("OneBot Core stream failed: {kind:?}")]
    StreamFailed { kind: CoreFailureKind },
    #[error("OneBot Core stream ended without a terminal event")]
    StreamEnded,
    #[error("OneBot Core response did not contain visible text")]
    EmptyResponse,
    #[error(transparent)]
    Send(#[from] OneBotSendError),
}

#[derive(Clone)]
pub(super) struct OneBotInboundDispatcher {
    core: Arc<dyn OneBotCoreResponder>,
    sender: Arc<dyn OneBotReplySender>,
    bot_display_name: String,
    ref_index: SharedRefIndex,
    commands: Option<GatewayCommandService>,
    command_prefix: CommandPrefix,
}

impl OneBotInboundDispatcher {
    pub(super) fn new(
        respond: RespondClient,
        sender: OneBotSender,
        bot_display_name: String,
        ref_index: SharedRefIndex,
        commands: GatewayCommandService,
        command_prefix: CommandPrefix,
    ) -> Self {
        Self {
            core: Arc::new(respond),
            sender: Arc::new(sender),
            bot_display_name,
            ref_index,
            commands: Some(commands),
            command_prefix,
        }
    }

    fn empty_reply_text(&self) -> String {
        format!(
            "唔，{}刚刚没整理出可用回复。可以再说一次。",
            self.bot_display_name
        )
    }

    pub(super) async fn dispatch(
        &self,
        mut inbound: InboundMessage,
    ) -> Result<OneBotDispatchOutcome, OneBotDispatchError> {
        {
            let mut ref_index = self
                .ref_index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ref_index.enrich_inbound(&mut inbound);
        }
        if matches!(inbound.conversation, ConversationTarget::Group { .. })
            && !inbound.mentioned_bot
            && !self
                .command_prefix
                .is_candidate_with_dot_compat(&inbound.text)
            && inbound.quoted.as_ref().and_then(|quoted| quoted.from_bot) != Some(true)
        {
            // 群聊 reply 候选只有在索引确认引用机器人出站消息后才触发；重启后的 miss
            // 或引用其他成员不会扩大群聊响应面。
            return Ok(OneBotDispatchOutcome::IgnoredNonBotReply);
        }
        if let Some(commands) = self.commands.as_ref() {
            let (conversation, group_id) = match &inbound.conversation {
                ConversationTarget::Private { .. } => (GatewayCommandConversation::Private, None),
                ConversationTarget::Group { target_id } => {
                    (GatewayCommandConversation::Group, Some(target_id.clone()))
                }
                ConversationTarget::Channel { .. } | ConversationTarget::ServiceAccount { .. } => {
                    (GatewayCommandConversation::Private, None)
                }
            };
            let context = GatewayCommandContext {
                platform_name: "OneBot 11",
                platform_code: "onebot11",
                event_name: match conversation {
                    GatewayCommandConversation::Group => "group_message",
                    _ => "private_message",
                },
                conversation,
                user_id: inbound.actor.sender_id.clone(),
                group_id,
                message_id: Some(inbound.message_id.clone()),
                timestamp: inbound.timestamp.clone(),
                attachment_count: inbound.attachments.len(),
            };
            if let Some(output) = commands.try_handle(&inbound.text, &context).await {
                let capability = crate::gateway::outbound::ReplyCapability::onebot11_text();
                let mention_user_id = incoming_at_mention_user_id(&inbound);
                self.send_text(
                    &inbound,
                    output.render(&capability).fallback_text(),
                    mention_user_id,
                    None,
                )
                .await?;
                return Ok(OneBotDispatchOutcome::Sent);
            }
        }
        self.ref_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert_inbound(&inbound);
        self.dispatch_reserved(inbound).await
    }

    async fn dispatch_reserved(
        &self,
        inbound: InboundMessage,
    ) -> Result<OneBotDispatchOutcome, OneBotDispatchError> {
        let mention_user_id = incoming_at_mention_user_id(&inbound);
        let content = platform::render_text_for_core(&inbound);
        let transport = match self.core.respond(&inbound, content).await {
            Ok(transport) => transport,
            Err(error) => {
                let summary = error.log_summary();
                let visible = respond_error_to_qq_text(&error);
                self.send_text(&inbound, &visible, mention_user_id, None)
                    .await?;
                return Err(OneBotDispatchError::Core { summary });
            }
        };
        let response = match complete_response(transport).await {
            Ok(response) => response,
            Err(CompletionError::Failed(failure)) => {
                let kind = failure.kind;
                self.send_text(
                    &inbound,
                    stream_failure_text(&failure),
                    mention_user_id,
                    None,
                )
                .await?;
                return Err(OneBotDispatchError::StreamFailed { kind });
            }
            Err(CompletionError::Ended) => {
                self.send_text(&inbound, STREAM_FAILED_TEXT, mention_user_id, None)
                    .await?;
                return Err(OneBotDispatchError::StreamEnded);
            }
        };
        if response.suppresses_reply() {
            return Ok(OneBotDispatchOutcome::SuppressedByCore);
        }
        let capability = crate::gateway::outbound::ReplyCapability::onebot11_text();
        let outbounds = render_respond_response_parts_for_profile(&response, &capability.render);
        if outbounds.is_empty() {
            let fallback = self.empty_reply_text();
            self.send_text(&inbound, &fallback, mention_user_id, None)
                .await?;
            return Err(OneBotDispatchError::EmptyResponse);
        }
        let structured_mention_ids = response
            .output
            .as_ref()
            .map(|output| {
                output
                    .mentions
                    .iter()
                    .filter_map(|mention| {
                        mention
                            .target
                            .user_id
                            .as_deref()
                            .map(str::trim)
                            .filter(|user_id| !user_id.is_empty())
                            .map(str::to_owned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (index, outbound) in outbounds.iter().enumerate() {
            let mut mention_user_ids = if index == 0 {
                structured_mention_ids.clone()
            } else {
                Vec::new()
            };
            if let Some(user_id) = mention_user_id
                && !mention_user_ids
                    .iter()
                    .any(|mentioned| mentioned == user_id)
            {
                mention_user_ids.push(user_id.to_owned());
            }
            self.send_outbound(
                &inbound,
                outbound,
                &mention_user_ids,
                response.visible_entity_snapshot.clone(),
            )
            .await?;
        }
        Ok(OneBotDispatchOutcome::Sent)
    }

    async fn send_outbound(
        &self,
        inbound: &InboundMessage,
        outbound: &OutboundMessage,
        mention_user_ids: &[String],
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        if let OutboundMessage::Image {
            image,
            fallback_text,
        } = outbound
        {
            let result = match &inbound.conversation {
                ConversationTarget::Private { target_id } => {
                    self.sender.send_private_image(target_id, image).await
                }
                ConversationTarget::Group { target_id } => {
                    if !mention_user_ids.is_empty() {
                        self.sender
                            .send_group_image_with_mentions(target_id, mention_user_ids, image)
                            .await
                    } else {
                        self.sender.send_group_image(target_id, image).await
                    }
                }
                _ => Err(OneBotSendError::InvalidTargetId),
            };
            match result {
                Ok(sent) => {
                    self.record_outbound(inbound, &sent, fallback_text, visible_entity_snapshot);
                    return Ok(sent);
                }
                Err(error) => {
                    warn!(error = %error, "OneBot 11 图片发送失败，将降级为文本发送");
                }
            }
        }
        self.send_text_with_mentions(
            inbound,
            outbound.fallback_text(),
            mention_user_ids,
            visible_entity_snapshot,
        )
        .await
    }

    async fn send_text_with_mentions(
        &self,
        inbound: &InboundMessage,
        text: &str,
        mention_user_ids: &[String],
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let result = match &inbound.conversation {
            ConversationTarget::Private { target_id } => {
                self.sender.send_private_text(target_id, text).await
            }
            ConversationTarget::Group { target_id } => {
                if mention_user_ids.is_empty() {
                    self.sender.send_group_text(target_id, text).await
                } else {
                    self.sender
                        .send_group_text_with_mentions(target_id, mention_user_ids, text)
                        .await
                }
            }
            ConversationTarget::Channel { .. } | ConversationTarget::ServiceAccount { .. } => {
                Err(OneBotSendError::InvalidTargetId)
            }
        };
        if let Ok(sent) = &result {
            self.record_outbound(inbound, sent, text, visible_entity_snapshot);
        }
        result
    }

    async fn send_text(
        &self,
        inbound: &InboundMessage,
        text: &str,
        mention_user_id: Option<&str>,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let result = match &inbound.conversation {
            ConversationTarget::Private { target_id } => {
                self.sender.send_private_text(target_id, text).await
            }
            ConversationTarget::Group { target_id } => {
                if let Some(user_id) = mention_user_id {
                    self.sender
                        .send_group_text_with_mentions(target_id, &[user_id.to_owned()], text)
                        .await
                } else {
                    self.sender.send_group_text(target_id, text).await
                }
            }
            ConversationTarget::Channel { .. } | ConversationTarget::ServiceAccount { .. } => {
                // OneBot 一期 adapter 不会构造这两类目标；若未来边界变化，必须显式失败。
                Err(OneBotSendError::InvalidTargetId)
            }
        };
        if let Ok(sent) = &result {
            self.record_outbound(inbound, sent, text, visible_entity_snapshot);
        }
        result
    }

    fn record_outbound(
        &self,
        inbound: &InboundMessage,
        sent: &OneBotSendResult,
        text: &str,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) {
        self.ref_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert_bot_outbound(
                inbound.platform,
                inbound.account_id.as_deref(),
                &inbound.conversation,
                Some(sent.message_id.clone()),
                text,
                visible_entity_snapshot,
            );
    }

    pub(super) fn log_result(result: Result<OneBotDispatchOutcome, OneBotDispatchError>) {
        match result {
            Ok(OneBotDispatchOutcome::Sent) => debug!("OneBot 11 回复分发完成"),
            Ok(OneBotDispatchOutcome::IgnoredNonBotReply) => {
                debug!("OneBot 11 群聊回复未指向当前机器人，已忽略")
            }
            Ok(OneBotDispatchOutcome::SuppressedByCore) => {
                debug!("OneBot 11 回复已被 Core 抑制")
            }
            Err(error) => warn!(error = %error, "OneBot 11 回复分发失败"),
        }
    }
}

/// 群消息显式 @ 当前机器人时，回复用平台原生 mention 回到原发言人。
///
/// 这是 Gateway 的通用出站规则，与骰点、命令或模型响应类型无关；没有 @ 机器人的
/// 直接命令不会因此额外提及发送者。
fn incoming_at_mention_user_id(inbound: &InboundMessage) -> Option<&str> {
    if !matches!(&inbound.conversation, ConversationTarget::Group { .. }) || !inbound.mentioned_bot
    {
        return None;
    }
    inbound
        .actor
        .sender_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

enum CompletionError {
    Failed(Box<CoreRespondFailure>),
    Ended,
}

async fn complete_response(
    transport: OneBotCoreTransport,
) -> Result<Box<CoreResponse>, CompletionError> {
    match transport {
        OneBotCoreTransport::Complete(response) => Ok(response),
        OneBotCoreTransport::Stream(mut stream) => {
            while let Some(event) = stream.recv_event().await {
                match event {
                    // OneBot 一期只发送可信 Completed；status/delta 一律不触发平台发送。
                    CoreResponseEvent::Status(_) | CoreResponseEvent::TextDelta(_) => {}
                    CoreResponseEvent::Completed(response) => return Ok(response),
                    CoreResponseEvent::Failed(failure) => {
                        return Err(CompletionError::Failed(Box::new(failure)));
                    }
                }
            }
            Err(CompletionError::Ended)
        }
    }
}

fn stream_failure_text(failure: &CoreRespondFailure) -> &'static str {
    match failure.kind {
        CoreFailureKind::SearchTimeout | CoreFailureKind::LlmTimeout => STREAM_TIMEOUT_TEXT,
        CoreFailureKind::Cancelled => STREAM_CANCELLED_TEXT,
        CoreFailureKind::SearchFailed
        | CoreFailureKind::LlmFailed
        | CoreFailureKind::ContextBudgetExceeded
        | CoreFailureKind::Internal => STREAM_FAILED_TEXT,
    }
}

#[cfg(test)]
mod image_tests;

#[cfg(test)]
mod tests;
