use super::*;
use crate::markdown::MarkdownPayload;

#[test]
fn empty_reply_fallback_uses_configured_bot_display_name() {
    assert_eq!(
        empty_reply_fallback_text("小助手"),
        "唔，小助手刚刚没整理出可用回复。可以再说一次。"
    );
}
use crate::{
    api::{ApiError, C2cReplyTarget, SendFuture},
    config::AppConfig,
    gateway::test_support::{
        c2c_message_fixture as c2c_message, qq_official_test_config as test_config,
        respond_response_fixture as respond_response,
    },
    media::ImagePayload,
};
use qq_maid_core::service::{CoreRespondFailure, CoreResponseStatus, CoreResponseStatusKind};
use std::{collections::VecDeque, sync::Mutex};

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
            Ok(SendMessageIds {
                message_id: Some("text-id".to_owned()),
                ref_index_id: Some("REFIDX_text_id".to_owned()),
            })
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
            Ok(SendMessageIds {
                message_id: Some("markdown-id".to_owned()),
                ref_index_id: Some("REFIDX_markdown_id".to_owned()),
            })
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

fn quoted_lookup_found(
    ref_index: &SharedRefIndex,
    config: &AppConfig,
    ref_id: &str,
) -> Option<String> {
    let mut message = c2c_message();
    message.message_id = "msg-quote".to_owned();
    message.reply = Some(crate::gateway::event::MessageReply {
        message_id: "qq-reply-message-id".to_owned(),
        ref_msg_idx: Some(ref_id.to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    let mut inbound = platform::qq_official::inbound_from_c2c(&message);
    inbound.account_id = config.app_id.clone();
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    inbound
        .quoted
        .as_ref()
        .filter(|quoted| quoted.lookup_found)
        .and_then(|quoted| quoted.text_summary.clone())
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

#[test]
fn complete_c2c_reply_records_ref_index_with_config_app_id() {
    let config = test_config();
    let ref_index = crate::gateway::ref_index::ref_index();

    record_c2c_bot_outbound_refs(
        &ref_index,
        &c2c_message(),
        &config,
        [SendMessageIds {
            message_id: Some("markdown-id".to_owned()),
            ref_index_id: Some("REFIDX_markdown_id".to_owned()),
        }],
        "完整回复",
        None,
    );

    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_markdown_id").as_deref(),
        Some("完整回复")
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "markdown-id"),
        None
    );
}

#[test]
fn complete_c2c_reply_does_not_record_message_id_as_ref_index() {
    let config = test_config();
    let ref_index = crate::gateway::ref_index::ref_index();

    record_c2c_bot_outbound_refs(
        &ref_index,
        &c2c_message(),
        &config,
        [SendMessageIds {
            message_id: Some("markdown-id-only".to_owned()),
            ref_index_id: None,
        }],
        "完整回复",
        None,
    );

    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "markdown-id-only"),
        None
    );
}

#[tokio::test]
async fn disabled_stream_completed_sends_single_ordinary_reply() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("不应外发".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ]);
    let sender = FakeOutboundSender::default();
    let mut typing = None;

    let outcome = handle_c2c_stream_disabled(
        events,
        &sender,
        &c2c_message(),
        &test_config(),
        &mut typing,
        None,
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
async fn disabled_stream_completed_records_ref_index() {
    let config = test_config();
    let events = FakeEventStream::new([RespondEvent::Completed(Box::new(respond_response(
        "最终回复",
    )))]);
    let sender = FakeOutboundSender::default();
    let mut typing = None;
    let ref_index = crate::gateway::ref_index::ref_index();

    let outcome = handle_c2c_stream_disabled(
        events,
        &sender,
        &c2c_message(),
        &config,
        &mut typing,
        Some(&ref_index),
    )
    .await
    .unwrap();

    assert_eq!(outcome, DisabledStreamOutcome::Completed);
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_markdown_id").as_deref(),
        Some("最终回复")
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "markdown-id"),
        None
    );
}

#[tokio::test]
async fn disabled_stream_completed_records_rendered_parts_fallback_ref_index() {
    let config = test_config();
    let response = RespondResponse {
        output: Some(qq_maid_core::service::AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![
                qq_maid_core::service::OutputPart::Markdown {
                    markdown: "# 标题".to_owned(),
                },
                qq_maid_core::service::OutputPart::Image {
                    media: qq_maid_core::service::OutputMedia {
                        fallback_text: Some("图片：天气雷达".to_owned()),
                        ..qq_maid_core::service::OutputMedia::default()
                    },
                },
            ],
        }),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    };
    let events = FakeEventStream::new([RespondEvent::Completed(Box::new(response))]);
    let sender = FakeOutboundSender::default();
    let mut typing = None;
    let ref_index = crate::gateway::ref_index::ref_index();

    let outcome = handle_c2c_stream_disabled(
        events,
        &sender,
        &c2c_message(),
        &config,
        &mut typing,
        Some(&ref_index),
    )
    .await
    .unwrap();

    assert_eq!(outcome, DisabledStreamOutcome::Completed);
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_markdown_id").as_deref(),
        Some("标题\n\n图片：天气雷达")
    );
}

#[tokio::test]
async fn disabled_stream_status_does_not_create_extra_reply() {
    let events = FakeEventStream::new([
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "正在处理".to_owned(),
        }),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ]);
    let sender = FakeOutboundSender::default();
    let mut typing = None;

    let outcome = handle_c2c_stream_disabled(
        events,
        &sender,
        &c2c_message(),
        &test_config(),
        &mut typing,
        None,
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
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentFinalizing,
            text: "小女仆正在确认结果…".to_owned(),
        }),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
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
        None,
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
async fn disabled_stream_progress_then_stream_sends_one_visible_hint_then_final_reply() {
    let events = FakeEventStream::new([
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentFinalizing,
            text: "小女仆正在确认结果…".to_owned(),
        }),
        RespondEvent::TextDelta("不应外发".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ])
    .with_policy(CoreOutputPolicy::ProgressThenStream);
    let sender = FakeOutboundSender::default();
    let mut typing = None;

    let outcome = handle_c2c_stream_disabled(
        events,
        &sender,
        &c2c_message(),
        &test_config(),
        &mut typing,
        None,
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
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ])
    .with_policy(CoreOutputPolicy::ProgressThenComplete);
    let sender = FakeOutboundSender::default();
    let mut typing = None;
    let mut config = test_config();
    config.c2c_visible_progress_status_enabled = false;

    let outcome =
        handle_c2c_stream_disabled(events, &sender, &c2c_message(), &config, &mut typing, None)
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
            agent: None,
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
        None,
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
        None,
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
