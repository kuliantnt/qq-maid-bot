use super::*;
use crate::gateway::typing::{
    C2cTypingSender, C2cTypingStatusGuard, TypingSendFuture, TypingStopReason,
};
use crate::{
    api::{
        ApiError, C2cReplyTarget, C2cStreamState, OutboundSender, SendFuture, SendMessageIds,
        StreamSendResult,
    },
    config::{
        AgentTypingConfig, AppConfig, DEFAULT_CONVERSATION_QUEUE_CAPACITY,
        DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT, DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
        DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS, DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
        DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES, DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
        DEFAULT_MESSAGE_AGGREGATION_QUIET_MS, DEFAULT_TEXT_CHUNK_SOFT_LIMIT, GroupMessageMode,
        MessageAggregationConfig,
    },
    event::{C2cMessage, MessageReply},
    markdown::MarkdownPayload,
    media::ImagePayload,
    respond::{RespondEvent, RespondResponse},
};
use qq_maid_core::service::{
    CoreFailureKind, CoreOutputPolicy, CoreRespondFailure, CoreResponseStatus,
    CoreResponseStatusKind,
};
use std::{collections::VecDeque, sync::Arc, time::Duration};

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

fn c2c_message() -> C2cMessage {
    C2cMessage {
        message_id: "msg-1".to_owned(),
        current_msg_idx: None,
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
        tools_visible_snapshot: None,
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
    inbound.account_id = Some(config.app_id.clone());
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    inbound
        .quoted
        .as_ref()
        .filter(|quoted| quoted.lookup_found)
        .and_then(|quoted| quoted.text_summary.clone())
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
        c2c_final_reply_stream_enabled: true,
        c2c_visible_progress_status_enabled: true,
        agent_typing: AgentTypingConfig {
            enabled: false,
            delay: Duration::from_secs(1),
        },
        markdown_chunk_soft_limit: DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
        text_chunk_soft_limit: DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
        media_dir: std::path::PathBuf::from("media/inbound"),
        media_download_timeout: Duration::from_secs(10),
        media_max_bytes: crate::config::DEFAULT_MEDIA_MAX_BYTES,
        wechat_service: crate::config::WechatServiceConfig::default(),
    }
}

#[tokio::test]
async fn stream_first_send_error_falls_back_to_completed_response() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(respond_response("晚上好")),
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
        RespondEvent::Completed(respond_response("晚上好")),
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
        RespondEvent::Completed(respond_response("晚上好")),
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
            kind: CoreResponseStatusKind::ToolLoopStarted,
            text: "正在处理".to_owned(),
        }),
        RespondEvent::Completed(respond_response("最终回复")),
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
async fn stream_first_send_without_id_falls_back_to_completed_response() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚上".to_owned()),
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(respond_response("晚上好")),
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
            kind: CoreResponseStatusKind::ToolLoopStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        RespondEvent::Completed(respond_response("最终回复")),
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
        RespondEvent::Completed(respond_response("测试成功")),
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
            RespondEvent::Completed(respond_response("晚上好")),
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
        RespondEvent::Completed(respond_response("好")),
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
            RespondEvent::Completed(respond_response("晚上")),
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
            RespondEvent::Completed(respond_response("晚上好")),
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
        RespondEvent::Completed(respond_response("晚上好")),
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
async fn stream_completed_flushes_pending_delta_before_final() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚".to_owned()),
        RespondEvent::TextDelta("上".to_owned()),
        RespondEvent::Completed(respond_response("晚上")),
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
async fn stream_completed_without_delta_uses_ordinary_reply_path() {
    let events = FakeEventStream::new([RespondEvent::Completed(respond_response("晚上好"))]);
    let sender = FakeStreamSender::new([]);

    let phase = stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert!(matches!(phase, C2cStreamingPhase::Completed));
    assert_eq!(
        sender.calls(),
        vec![FakeCall::Markdown {
            content: "晚上好".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn stream_pending_completed_stops_typing_with_final_reply_before_ordinary_reply() {
    let events = FakeEventStream::new([RespondEvent::Completed(respond_response("晚上好"))]);
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

    stream_respond_c2c_with_sender_and_typing(
        events,
        &sender,
        &c2c_message(),
        &test_config(),
        Some(typing),
    )
    .await
    .unwrap();

    assert_eq!(
        *stop_reason.lock().unwrap(),
        Some(TypingStopReason::FinalReply)
    );
    assert_eq!(
        sender.calls(),
        vec![FakeCall::Markdown {
            content: "晚上好".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn stream_pending_completed_sends_ordinary_reply_once() {
    let events = FakeEventStream::new([
        RespondEvent::Completed(respond_response("晚上好")),
        RespondEvent::Completed(respond_response("不应重复发送")),
    ]);
    let sender = FakeStreamSender::new([]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec![FakeCall::Markdown {
            content: "晚上好".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }]
    );
}

#[tokio::test]
async fn stream_completed_sends_single_final_chunk() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("好".to_owned()),
        RespondEvent::Completed(respond_response("好")),
        RespondEvent::Completed(respond_response("好")),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let final_count = sender
        .calls()
        .into_iter()
        .filter(|call| {
            matches!(
                call,
                FakeCall::Stream {
                    stream_state_value: 10,
                    ..
                }
            )
        })
        .count();
    assert_eq!(final_count, 1);
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
            RespondEvent::Completed(respond_response("晚上")),
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
async fn core_failed_event_is_returned_as_observable_error() {
    let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
        kind: CoreFailureKind::Internal,
        message: "boom".to_owned(),
        retryable: false,
    })]);
    let sender = FakeStreamSender::new([]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

    assert!(result.is_err());
    assert!(sender.calls().is_empty());
}

#[tokio::test]
async fn stream_timeout_failure_stops_typing_with_timeout_reason() {
    let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
        kind: CoreFailureKind::LlmTimeout,
        message: "LLM 服务处理超时，请稍后再试。".to_owned(),
        retryable: true,
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

    assert!(result.is_err());
    assert_eq!(
        *stop_reason.lock().unwrap(),
        Some(TypingStopReason::Timeout)
    );
}
