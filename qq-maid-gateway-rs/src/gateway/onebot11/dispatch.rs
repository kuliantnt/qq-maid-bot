//! OneBot 11 入站到 Core 与文本 sender 的最小闭环。
//!
//! 本模块只编排统一入站模型、CoreService、结构化输出渲染和 OneBot sender；命令、
//! Todo、Memory、Pending 与 Tool Loop 仍完全由 Core 的既有场景策略决定。

use std::{future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use qq_maid_core::service::{
    CoreFailureKind, CoreRespondFailure, CoreResponse, CoreResponseEvent, CoreResponseStream,
    VisibleEntitySnapshot,
};
use thiserror::Error;
use tracing::{debug, warn};

use crate::{
    gateway::{
        command::{GatewayCommandContext, GatewayCommandConversation, GatewayCommandService},
        platform::{self, ConversationTarget, InboundMessage, is_slash_command_candidate},
        ref_index::SharedRefIndex,
    },
    render::render_respond_response_for_profile,
    respond::{RespondClient, RespondError, RespondTransport, respond_error_to_qq_text},
};

use super::{OneBotSendError, OneBotSendResult, OneBotSender};

const STREAM_FAILED_TEXT: &str = "处理失败，请稍后再试。";
const STREAM_TIMEOUT_TEXT: &str = "LLM 服务处理超时，请稍后再试";
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
            RespondTransport::Complete(response) => Ok(OneBotCoreTransport::Complete(response)),
            RespondTransport::Stream(stream) => Ok(OneBotCoreTransport::Stream(Box::new(stream))),
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
}

impl OneBotInboundDispatcher {
    pub(super) fn new(
        respond: RespondClient,
        sender: OneBotSender,
        bot_display_name: String,
        ref_index: SharedRefIndex,
        commands: GatewayCommandService,
    ) -> Self {
        Self {
            core: Arc::new(respond),
            sender: Arc::new(sender),
            bot_display_name,
            ref_index,
            commands: Some(commands),
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
            && !is_slash_command_candidate(&inbound.text)
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
                self.send_text(&inbound, output.render(&capability).fallback_text(), None)
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
        let content = platform::render_text_for_core(&inbound);
        let transport = match self.core.respond(&inbound, content).await {
            Ok(transport) => transport,
            Err(error) => {
                let summary = error.log_summary();
                let visible = respond_error_to_qq_text(&error);
                self.send_text(&inbound, &visible, None).await?;
                return Err(OneBotDispatchError::Core { summary });
            }
        };
        let response = match complete_response(transport).await {
            Ok(response) => response,
            Err(CompletionError::Failed(failure)) => {
                let kind = failure.kind;
                self.send_text(&inbound, stream_failure_text(&failure), None)
                    .await?;
                return Err(OneBotDispatchError::StreamFailed { kind });
            }
            Err(CompletionError::Ended) => {
                self.send_text(&inbound, STREAM_FAILED_TEXT, None).await?;
                return Err(OneBotDispatchError::StreamEnded);
            }
        };
        if response.suppresses_reply() {
            return Ok(OneBotDispatchOutcome::SuppressedByCore);
        }
        let capability = crate::gateway::outbound::ReplyCapability::onebot11_text();
        let Some(outbound) = render_respond_response_for_profile(&response, &capability.render)
        else {
            let fallback = self.empty_reply_text();
            self.send_text(&inbound, &fallback, None).await?;
            return Err(OneBotDispatchError::EmptyResponse);
        };
        let text = outbound.fallback_text();
        if text.trim().is_empty() {
            let fallback = self.empty_reply_text();
            self.send_text(&inbound, &fallback, None).await?;
            return Err(OneBotDispatchError::EmptyResponse);
        }
        self.send_text(&inbound, text, response.visible_entity_snapshot.clone())
            .await?;
        Ok(OneBotDispatchOutcome::Sent)
    }

    async fn send_text(
        &self,
        inbound: &InboundMessage,
        text: &str,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let result = match &inbound.conversation {
            ConversationTarget::Private { target_id } => {
                self.sender.send_private_text(target_id, text).await
            }
            ConversationTarget::Group { target_id } => {
                self.sender.send_group_text(target_id, text).await
            }
            ConversationTarget::Channel { .. } | ConversationTarget::ServiceAccount { .. } => {
                // OneBot 一期 adapter 不会构造这两类目标；若未来边界变化，必须显式失败。
                Err(OneBotSendError::InvalidTargetId)
            }
        };
        if let Ok(sent) = &result {
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
        result
    }

    pub(super) fn log_result(result: Result<OneBotDispatchOutcome, OneBotDispatchError>) {
        match result {
            Ok(OneBotDispatchOutcome::Sent) => debug!("OneBot 11 reply dispatch completed"),
            Ok(OneBotDispatchOutcome::IgnoredNonBotReply) => {
                debug!("ignored OneBot 11 group reply not addressed to current bot")
            }
            Ok(OneBotDispatchOutcome::SuppressedByCore) => {
                debug!("OneBot 11 reply suppressed by Core")
            }
            Err(error) => warn!(error = %error, "OneBot 11 reply dispatch failed"),
        }
    }
}

enum CompletionError {
    Failed(CoreRespondFailure),
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
                        return Err(CompletionError::Failed(failure));
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
        CoreFailureKind::SearchFailed | CoreFailureKind::LlmFailed | CoreFailureKind::Internal => {
            STREAM_FAILED_TEXT
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use qq_maid_common::{
        identity_context::IdentitySource,
        input_part::{MessageInputPart, QuotedMessageContext},
    };
    use qq_maid_core::service::{
        AssistantOutput, CoreError, CoreResponseStatus, CoreResponseStatusKind, VisibleEntityItem,
    };

    use super::*;
    use crate::gateway::{
        onebot11::OneBotCallError,
        platform::{Actor, Platform},
    };

    struct FakeStream {
        events: VecDeque<CoreResponseEvent>,
    }

    impl OneBotResponseEventStream for FakeStream {
        fn recv_event<'a>(&'a mut self) -> EventFuture<'a> {
            Box::pin(async move { self.events.pop_front() })
        }
    }

    struct FakeCore {
        outputs: Mutex<VecDeque<Result<OneBotCoreTransport, RespondError>>>,
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl OneBotCoreResponder for FakeCore {
        async fn respond(
            &self,
            inbound: &InboundMessage,
            content: String,
        ) -> Result<OneBotCoreTransport, RespondError> {
            self.calls
                .lock()
                .unwrap()
                .push((inbound.message_id.clone(), content));
            self.outputs.lock().unwrap().pop_front().unwrap()
        }
    }

    #[derive(Default)]
    struct FakeSender {
        sent: Mutex<Vec<(String, String, String)>>,
        fail: bool,
    }

    #[async_trait]
    impl OneBotReplySender for FakeSender {
        async fn send_private_text(
            &self,
            user_id: &str,
            text: &str,
        ) -> Result<OneBotSendResult, OneBotSendError> {
            self.send("private", user_id, text)
        }

        async fn send_group_text(
            &self,
            group_id: &str,
            text: &str,
        ) -> Result<OneBotSendResult, OneBotSendError> {
            self.send("group", group_id, text)
        }
    }

    impl FakeSender {
        fn send(
            &self,
            kind: &str,
            target: &str,
            text: &str,
        ) -> Result<OneBotSendResult, OneBotSendError> {
            if self.fail {
                return Err(OneBotSendError::Transport(OneBotCallError::NotConnected));
            }
            self.sent
                .lock()
                .unwrap()
                .push((kind.to_owned(), target.to_owned(), text.to_owned()));
            Ok(OneBotSendResult {
                message_id: "sent-1".to_owned(),
            })
        }
    }

    fn response(text: Option<&str>) -> Box<CoreResponse> {
        Box::new(CoreResponse {
            output: text.map(AssistantOutput::text),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        })
    }

    fn suppressed_response() -> Box<CoreResponse> {
        Box::new(CoreResponse {
            output: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: Some(serde_json::json!({
                "suppressed": true,
                "reason": "unknown_group_slash_command",
            })),
            visible_entity_snapshot: None,
        })
    }

    fn unhandled_empty_response() -> Box<CoreResponse> {
        Box::new(CoreResponse {
            output: None,
            handled: Some(false),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        })
    }

    fn snapshot(entity_id: &str) -> VisibleEntitySnapshot {
        VisibleEntitySnapshot {
            platform: "onebot11".to_owned(),
            account_id: Some("10001".to_owned()),
            scope_key: "platform:onebot:account:10001:private:20002".to_owned(),
            owner_key: Some("platform:onebot:account:10001:private:20002".to_owned()),
            created_at: "2026-07-13T12:00:00+08:00".to_owned(),
            items: vec![VisibleEntityItem {
                domain: "todo".to_owned(),
                entity_kind: "todo".to_owned(),
                entity_id: entity_id.to_owned(),
                visible_number: 1,
                label: None,
                status: Some("list".to_owned()),
            }],
        }
    }

    fn inbound(message_id: &str, group: bool) -> InboundMessage {
        InboundMessage {
            platform: Platform::OneBot11,
            account_id: Some("10001".to_owned()),
            conversation: if group {
                ConversationTarget::Group {
                    target_id: "30003".to_owned(),
                }
            } else {
                ConversationTarget::Private {
                    target_id: "20002".to_owned(),
                }
            },
            actor: Actor {
                sender_id: Some("20002".to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: None,
                is_bot: false,
                source: IdentitySource::Event,
            },
            message_id: message_id.to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "/help".to_owned(),
            input_parts: vec![MessageInputPart::text("/help")],
            attachments: Vec::new(),
            quoted: None,
            visible_entity_snapshot: None,
            mentions: Vec::new(),
            mentioned_bot: group,
        }
    }

    fn dispatcher(
        outputs: Vec<Result<OneBotCoreTransport, RespondError>>,
        sender: Arc<FakeSender>,
    ) -> (OneBotInboundDispatcher, Arc<FakeCore>) {
        let core = Arc::new(FakeCore {
            outputs: Mutex::new(outputs.into()),
            calls: Mutex::new(Vec::new()),
        });
        (
            OneBotInboundDispatcher {
                core: core.clone(),
                sender,
                bot_display_name: "小助手".to_owned(),
                ref_index: crate::gateway::ref_index::ref_index(),
                commands: None,
            },
            core,
        )
    }

    #[tokio::test]
    async fn complete_private_and_group_responses_send_once() {
        for group in [false, true] {
            let sender = Arc::new(FakeSender::default());
            let (dispatcher, core) = dispatcher(
                vec![Ok(OneBotCoreTransport::Complete(response(Some(
                    "命令结果",
                ))))],
                sender.clone(),
            );

            assert_eq!(
                dispatcher.dispatch(inbound("m1", group)).await.unwrap(),
                OneBotDispatchOutcome::Sent
            );
            assert_eq!(core.calls.lock().unwrap().len(), 1);
            assert_eq!(sender.sent.lock().unwrap().len(), 1);
            assert_eq!(sender.sent.lock().unwrap()[0].2, "命令结果");
        }
    }

    #[tokio::test]
    async fn direct_group_slash_candidate_without_at_reaches_core_once() {
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, core) = dispatcher(
            vec![Ok(OneBotCoreTransport::Complete(response(Some(
                "命令结果",
            ))))],
            sender.clone(),
        );
        let mut command = inbound("direct-command", true);
        command.mentioned_bot = false;

        assert_eq!(
            dispatcher.dispatch(command).await.unwrap(),
            OneBotDispatchOutcome::Sent
        );
        assert_eq!(core.calls.lock().unwrap().len(), 1);
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn group_slash_suppressed_by_core_sends_nothing() {
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, core) = dispatcher(
            vec![Ok(OneBotCoreTransport::Complete(suppressed_response()))],
            sender.clone(),
        );
        let mut command = inbound("unknown-command", true);
        command.mentioned_bot = false;
        command.text = "/unknown".to_owned();
        command.input_parts = vec![MessageInputPart::text("/unknown")];

        assert_eq!(
            dispatcher.dispatch(command).await.unwrap(),
            OneBotDispatchOutcome::SuppressedByCore
        );
        assert_eq!(core.calls.lock().unwrap().len(), 1);
        assert!(sender.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unhandled_empty_response_without_marker_is_not_suppressed() {
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, core) = dispatcher(
            vec![Ok(
                OneBotCoreTransport::Complete(unhandled_empty_response()),
            )],
            sender.clone(),
        );

        let error = dispatcher.dispatch(inbound("empty-response", true)).await;

        assert!(matches!(error, Err(OneBotDispatchError::EmptyResponse)));
        assert_eq!(core.calls.lock().unwrap().len(), 1);
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stream_ignores_status_and_delta_then_sends_only_completed_body() {
        let stream = FakeStream {
            events: VecDeque::from([
                CoreResponseEvent::Status(CoreResponseStatus {
                    kind: CoreResponseStatusKind::CommandStarted,
                    text: "正在处理".to_owned(),
                }),
                CoreResponseEvent::TextDelta("不能提前发送".to_owned()),
                CoreResponseEvent::Completed(response(Some("最终完整回复"))),
            ]),
        };
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, _) = dispatcher(
            vec![Ok(OneBotCoreTransport::Stream(Box::new(stream)))],
            sender.clone(),
        );

        assert_eq!(
            dispatcher
                .dispatch(inbound("m-stream", false))
                .await
                .unwrap(),
            OneBotDispatchOutcome::Sent
        );
        assert_eq!(
            sender.sent.lock().unwrap().as_slice(),
            &[(
                "private".to_owned(),
                "20002".to_owned(),
                "最终完整回复".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn successful_send_indexes_platform_message_id_and_visible_snapshot() {
        let expected_snapshot = snapshot("todo-1");
        let mut output = response(Some("待办列表"));
        output.visible_entity_snapshot = Some(expected_snapshot.clone());
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, _) = dispatcher(vec![Ok(OneBotCoreTransport::Complete(output))], sender);
        let ref_index = dispatcher.ref_index.clone();

        dispatcher
            .dispatch(inbound("user-message", false))
            .await
            .unwrap();

        let mut quoted = inbound("reply-message", false);
        quoted.quoted = Some(QuotedMessageContext {
            current_message_id: Some("reply-message".to_owned()),
            reference_id: Some("sent-1".to_owned()),
            ..Default::default()
        });
        ref_index.lock().unwrap().enrich_inbound(&mut quoted);
        let context = quoted.quoted.unwrap();
        assert!(context.lookup_found);
        assert_eq!(context.text_summary.as_deref(), Some("待办列表"));
        assert_eq!(context.from_bot, Some(true));
        assert_eq!(quoted.visible_entity_snapshot, Some(expected_snapshot));
    }

    #[tokio::test]
    async fn group_reply_without_at_only_triggers_when_ref_index_confirms_bot_message() {
        let sender = Arc::new(FakeSender::default());
        let (dispatcher, core) = dispatcher(
            vec![
                Ok(OneBotCoreTransport::Complete(response(Some("第一条回复")))),
                Ok(OneBotCoreTransport::Complete(response(Some("引用回复")))),
            ],
            sender,
        );
        dispatcher
            .dispatch(inbound("user-message", true))
            .await
            .unwrap();

        let mut bot_reply = inbound("reply-to-bot", true);
        bot_reply.mentioned_bot = false;
        bot_reply.text = "继续".to_owned();
        bot_reply.input_parts = vec![MessageInputPart::text("继续")];
        bot_reply.quoted = Some(QuotedMessageContext {
            reference_id: Some("sent-1".to_owned()),
            ..Default::default()
        });
        assert_eq!(
            dispatcher.dispatch(bot_reply).await.unwrap(),
            OneBotDispatchOutcome::Sent
        );

        let mut user_reply = inbound("reply-to-user", true);
        user_reply.mentioned_bot = false;
        user_reply.text = "继续".to_owned();
        user_reply.input_parts = vec![MessageInputPart::text("继续")];
        user_reply.quoted = Some(QuotedMessageContext {
            reference_id: Some("user-message".to_owned()),
            ..Default::default()
        });
        assert_eq!(
            dispatcher.dispatch(user_reply).await.unwrap(),
            OneBotDispatchOutcome::IgnoredNonBotReply
        );
        assert_eq!(core.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn core_error_stream_failure_and_empty_response_send_explicit_fallbacks() {
        let cases = [
            (
                Err(RespondError::Core(CoreError::new(
                    "timeout",
                    "respond",
                    "timed out",
                ))),
                "LLM 服务处理超时，请稍后再试",
            ),
            (
                Ok(OneBotCoreTransport::Stream(Box::new(FakeStream {
                    events: VecDeque::from([CoreResponseEvent::Failed(CoreRespondFailure {
                        kind: CoreFailureKind::Cancelled,
                        message: "cancelled".to_owned(),
                        retryable: false,
                        agent: None,
                    })]),
                }))),
                STREAM_CANCELLED_TEXT,
            ),
            (
                Ok(OneBotCoreTransport::Complete(response(None))),
                "唔，小助手刚刚没整理出可用回复。可以再说一次。",
            ),
        ];

        for (index, (output, expected_text)) in cases.into_iter().enumerate() {
            let sender = Arc::new(FakeSender::default());
            let (dispatcher, _) = dispatcher(vec![output], sender.clone());
            assert!(
                dispatcher
                    .dispatch(inbound(&format!("m-{index}"), false))
                    .await
                    .is_err()
            );
            assert_eq!(sender.sent.lock().unwrap()[0].2, expected_text);
        }
    }

    #[tokio::test]
    async fn sender_failure_is_returned_instead_of_core_success() {
        let sender = Arc::new(FakeSender {
            fail: true,
            ..FakeSender::default()
        });
        let (dispatcher, _) = dispatcher(
            vec![Ok(OneBotCoreTransport::Complete(response(Some(
                "不会伪装成功",
            ))))],
            sender,
        );

        assert!(matches!(
            dispatcher.dispatch(inbound("send-fail", false)).await,
            Err(OneBotDispatchError::Send(OneBotSendError::Transport(
                OneBotCallError::NotConnected
            )))
        ));
    }
}
