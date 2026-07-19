use super::*;

mod completion;
use crate::gateway::typing::{
    C2cTypingSender, C2cTypingStatusGuard, TypingSendFuture, TypingStopReason,
};
use crate::{
    api::{
        ApiError, C2cReplyTarget, C2cStreamState, OutboundSender, SendFuture, SendMessageIds,
        StreamSendResult,
    },
    config::{AgentTypingConfig, AppConfig},
    event::MessageReply,
    gateway::test_support::{
        c2c_message_fixture as c2c_message, qq_official_test_config,
        respond_response_fixture as respond_response,
    },
    markdown::MarkdownPayload,
    media::ImagePayload,
    respond::RespondEvent,
};
use qq_maid_core::service::{
    AssistantOutput, CoreFailureKind, CoreOutputPolicy, CoreRespondFailure, CoreResponseStatus,
    CoreResponseStatusKind, OutputMedia, OutputPart,
};
use std::{collections::VecDeque, sync::Arc, time::Duration};

fn test_config() -> AppConfig {
    let mut config = qq_official_test_config();
    config.c2c_final_reply_stream_enabled = true;
    config
}

#[derive(Debug)]
struct FakeEventStream {
    events: VecDeque<(Duration, RespondEvent)>,
    output_policy: CoreOutputPolicy,
}

impl FakeEventStream {
    fn new(events: impl IntoIterator<Item = RespondEvent>) -> Self {
        Self {
            events: events
                .into_iter()
                .map(|event| (Duration::ZERO, event))
                .collect(),
            output_policy: CoreOutputPolicy::DirectStream,
        }
    }

    fn with_delays(events: impl IntoIterator<Item = (Duration, RespondEvent)>) -> Self {
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
        Box::pin(async move {
            let (delay, event) = self.events.pop_front()?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Some(event)
        })
    }

    fn output_policy(&self) -> CoreOutputPolicy {
        self.output_policy
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
    stream_results: std::sync::Mutex<VecDeque<StreamSendResult>>,
    calls: std::sync::Mutex<Vec<FakeCall>>,
}

#[derive(Debug)]
struct NoopTypingSender;

impl C2cTypingSender for NoopTypingSender {
    fn send_typing<'a>(
        &'a self,
        _user_openid: &'a str,
        _msg_id: Option<&'a str>,
    ) -> TypingSendFuture<'a> {
        Box::pin(async move { Ok(SendMessageIds::none()) })
    }
}

impl FakeStreamSender {
    fn new(stream_results: impl IntoIterator<Item = StreamSendResult>) -> Self {
        Self {
            stream_results: std::sync::Mutex::new(stream_results.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
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
            Ok(SendMessageIds {
                message_id: Some("ordinary-text-id".to_owned()),
                ref_index_id: Some("REFIDX_ordinary_text".to_owned()),
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
                message_id: Some("ordinary-markdown-id".to_owned()),
                ref_index_id: Some("REFIDX_ordinary_markdown".to_owned()),
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

impl C2cStreamSender for FakeStreamSender {
    fn send_stream_markdown<'a>(
        &'a self,
        _user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a mut C2cStreamState,
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

fn quoted_lookup_found(
    ref_index: &crate::gateway::ref_index::SharedRefIndex,
    config: &AppConfig,
    ref_id: &str,
) -> Option<String> {
    let mut message = c2c_message();
    message.message_id = "msg-quote".to_owned();
    message.reply = Some(MessageReply {
        message_id: "qq-reply-message-id".to_owned(),
        ref_msg_idx: Some(ref_id.to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    let mut inbound = crate::gateway::platform::qq_official::inbound_from_c2c(&message);
    inbound.account_id = config.app_id.clone();
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    inbound
        .quoted
        .as_ref()
        .filter(|quoted| quoted.lookup_found)
        .and_then(|quoted| quoted.text_summary.clone())
}

#[tokio::test]
async fn stream_first_send_error_falls_back_to_completed_response() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
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
async fn stream_pending_fallback_records_ref_index() {
    let config = test_config();
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
    ]);
    let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);
    let ref_index = crate::gateway::ref_index::ref_index();

    stream_respond_c2c_with_sender_and_ref_index(
        events,
        &sender,
        &c2c_message(),
        &config,
        &ref_index,
    )
    .await
    .unwrap();

    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_ordinary_markdown").as_deref(),
        Some("晚上好")
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "ordinary-markdown-id"),
        None
    );
}

#[tokio::test]
async fn active_stream_does_not_fake_ref_index_from_stream_id() {
    let config = test_config();
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上好".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);
    let ref_index = crate::gateway::ref_index::ref_index();

    stream_respond_c2c_with_sender_and_ref_index(
        events,
        &sender,
        &c2c_message(),
        &config,
        &ref_index,
    )
    .await
    .unwrap();

    assert_eq!(quoted_lookup_found(&ref_index, &config, "stream-1"), None);
    assert_eq!(quoted_lookup_found(&ref_index, &config, "msg-1"), None);
}

#[tokio::test]
async fn stream_status_event_does_not_start_qq_stream_or_extra_send() {
    let events = FakeEventStream::new([
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "正在处理".to_owned(),
        }),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ]);
    let sender = FakeStreamSender::new([]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec![FakeCall::Markdown {
            content: "最终回复".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn progress_policy_status_sends_one_visible_hint_then_final_reply() {
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
    let sender = FakeStreamSender::new([]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

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
            },
        ]
    );
}

#[tokio::test]
async fn progress_then_stream_sends_one_visible_hint_then_streams_final_answer() {
    let events = FakeEventStream::new([
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentFinalizing,
            text: "小女仆正在确认结果…".to_owned(),
        }),
        RespondEvent::TextDelta("最终".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ])
    .with_policy(CoreOutputPolicy::ProgressThenStream);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec![
            FakeCall::Text {
                content: "小女仆正在处理…".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            },
            FakeCall::Stream {
                content: "最终".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: None,
                index: 0,
                stream_state_value: 1,
                reset: Some(false),
            },
            FakeCall::Stream {
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 10,
                reset: Some(false),
            },
        ]
    );
}

#[tokio::test]
async fn stream_first_send_without_id_falls_back_to_completed_response() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
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
async fn progress_policy_status_respects_visible_progress_config() {
    let events = FakeEventStream::new([
        RespondEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Completed(Box::new(respond_response("最终回复"))),
    ])
    .with_policy(CoreOutputPolicy::ProgressThenComplete);
    let sender = FakeStreamSender::new([]);
    let mut config = test_config();
    config.c2c_visible_progress_status_enabled = false;

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &config)
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec![FakeCall::Markdown {
            content: "最终回复".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn stream_single_content_packet_then_final_keeps_stream_id() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("测试成功".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("测试成功"))),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

    let phase = stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert!(matches!(phase, C2cStreamingPhase::Completed));
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 10,
                reset: Some(false),
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
            RespondEvent::Completed(Box::new(respond_response("晚上好"))),
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 2,
                stream_state_value: 10,
                reset: Some(false),
            },
        ]
    );
}

#[tokio::test]
async fn stream_empty_delta_does_not_consume_index() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta(String::new()),
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("好"))),
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 10,
                reset: Some(false),
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
            RespondEvent::Completed(Box::new(respond_response("晚上"))),
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 2,
                stream_state_value: 10,
                reset: Some(false),
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
            RespondEvent::Completed(Box::new(respond_response("晚上好"))),
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 2,
                stream_state_value: 10,
                reset: Some(false),
            },
        ]
    );
}

#[tokio::test]
async fn stream_final_failure_does_not_send_ordinary_fallback_after_active() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
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
                content: STREAM_FINAL_MARKER.to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 10,
                reset: Some(false),
            },
        ]
    );
}

#[tokio::test]
async fn stream_chunk_failure_does_not_advance_next_index() {
    let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);
    let mut stream_state = C2cStreamState::new();
    stream_state.stream_id = Some("stream-1".to_owned());
    stream_state.index = 1;

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
async fn stream_final_success_commits_next_index() {
    let sender = FakeStreamSender::new([Ok(None)]);
    let mut stream_state = C2cStreamState::new();
    stream_state.stream_id = Some("stream-1".to_owned());
    stream_state.index = 2;

    send_stream_end(
        &sender,
        "user-1",
        Some("msg-1"),
        "最终正文",
        &mut stream_state,
    )
    .await
    .unwrap();

    assert_eq!(stream_state.index, 3);
    assert_eq!(
        sender.calls(),
        vec![FakeCall::Stream {
            content: "最终正文".to_owned(),
            msg_id: Some("msg-1".to_owned()),
            stream_id: Some("stream-1".to_owned()),
            index: 2,
            stream_state_value: 10,
            reset: Some(false),
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
            RespondEvent::Completed(Box::new(respond_response("晚上"))),
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
                content: "上".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 10,
                reset: Some(false),
            },
        ]
    );
}

#[tokio::test]
async fn pending_core_failure_sends_safe_ordinary_failure_reply() {
    let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
        kind: CoreFailureKind::Internal,
        message: "处理失败，请稍后再试。".to_owned(),
        retryable: false,
        agent: None,
    })]);
    let sender = FakeStreamSender::new([]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

    assert!(matches!(result.unwrap(), C2cStreamingPhase::Completed));
    assert_eq!(
        sender.calls(),
        vec![FakeCall::Text {
            content: "处理失败，请稍后再试。".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn stream_timeout_failure_stops_typing_with_timeout_reason() {
    let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
        kind: CoreFailureKind::LlmTimeout,
        message: "LLM 服务处理超时，请稍后再试。".to_owned(),
        retryable: true,
        agent: None,
    })]);
    let sender = FakeStreamSender::new([]);
    let typing = C2cTypingStatusGuard::schedule_with_sender(
        &AgentTypingConfig {
            enabled: true,
            delay: Duration::from_secs(60),
        },
        Arc::new(NoopTypingSender),
        &c2c_message(),
        "test",
    )
    .unwrap();
    let stop_reason = typing.stop_reason_probe_for_test();

    let result = stream_respond_c2c_with_sender_and_typing(
        events,
        &sender,
        &c2c_message(),
        &test_config(),
        Some(typing),
    )
    .await;

    assert!(matches!(result.unwrap(), C2cStreamingPhase::Completed));
    assert_eq!(
        *stop_reason.lock().unwrap(),
        Some(TypingStopReason::Timeout)
    );
    assert_eq!(
        sender.calls(),
        vec![FakeCall::Text {
            content: "LLM 服务处理超时，请稍后再试。".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn active_core_failure_finalizes_stream_without_ordinary_failure_reply() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("已发送".to_owned()),
        RespondEvent::Failed(CoreRespondFailure {
            kind: CoreFailureKind::LlmTimeout,
            message: "LLM 服务处理超时，请稍后再试。".to_owned(),
            retryable: true,
            agent: None,
        }),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

    assert!(result.is_err());
    let calls = sender.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| matches!(call, FakeCall::Stream { .. }))
    );
}
