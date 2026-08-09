use super::*;

mod completion;
mod send;

use crate::gateway::typing::{
    C2cTypingSender, C2cTypingStatusGuard, TypingSendFuture, TypingStopReason,
};
use crate::{
    api::{
        ApiError, C2cReplyTarget, C2cStreamResponse, OutboundSender, SendFuture, SendMessageIds,
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
};
use qq_maid_core::service::{
    CoreOutputPolicy, CoreResponseEvent, CoreResponseStatus, CoreResponseStatusKind,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn test_config() -> AppConfig {
    let mut config = qq_official_test_config();
    config.c2c_final_reply_stream_enabled = true;
    config
}

#[derive(Debug)]
struct FakeEventStream {
    events: VecDeque<(Duration, CoreResponseEvent)>,
    output_policy: CoreOutputPolicy,
}

impl FakeEventStream {
    fn new(events: impl IntoIterator<Item = CoreResponseEvent>) -> Self {
        Self {
            events: events
                .into_iter()
                .map(|event| (Duration::ZERO, event))
                .collect(),
            output_policy: CoreOutputPolicy::DirectStream,
        }
    }

    fn with_delays(events: impl IntoIterator<Item = (Duration, CoreResponseEvent)>) -> Self {
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
            let delay = self.events.front().map(|(delay, _)| *delay)?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            self.events.pop_front().map(|(_, event)| event)
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
        stream_msg_id: Option<String>,
        msg_seq: u32,
        index: u32,
        input_state: u8,
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
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
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
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().unwrap().clone()
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::Relaxed)
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
        content_raw: &'a str,
        stream_state: &'a mut C2cStreamState,
        input_state: u8,
    ) -> StreamSendFuture<'a> {
        // 模拟真实客户端：请求开始就预留 index/msg_seq，失败也不能让后续 complete
        // 复用可能已经被平台消费的 index。
        let transport = &mut stream_state.transport;
        let msg_seq = *transport.msg_seq.get_or_insert(1);
        let index = transport.index;
        transport.index = transport.index.saturating_add(1);
        let stream_msg_id = transport.stream_msg_id.clone();
        self.calls.lock().unwrap().push(FakeCall::Stream {
            content: content_raw.to_owned(),
            msg_id: msg_id.map(str::to_owned),
            stream_msg_id,
            msg_seq,
            index,
            input_state,
        });
        let in_flight = Arc::clone(&self.in_flight);
        let max_in_flight = Arc::clone(&self.max_in_flight);
        Box::pin(async move {
            let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(current, Ordering::SeqCst);
            let result = self
                .stream_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ApiError::Unsupported("stream")));
            if let Ok(response) = &result
                && stream_state.transport.stream_msg_id.is_none()
            {
                stream_state.transport.stream_msg_id = Some(response.message_id.clone());
            }
            in_flight.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }
}

fn stream_response(message_id: &str, ref_index_id: Option<&str>) -> StreamSendResult {
    Ok(C2cStreamResponse {
        message_id: message_id.to_owned(),
        ref_index_id: ref_index_id.map(str::to_owned),
    })
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
async fn official_stream_uses_cumulative_content_and_single_complete() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("你好".to_owned()),
        CoreResponseEvent::TextDelta("，这是".to_owned()),
        CoreResponseEvent::TextDelta("内容".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("你好，这是内容"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        stream_response("reply-update", None),
        stream_response("reply-complete", Some("REFIDX_complete")),
    ]);

    let phase = stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    assert!(matches!(phase, C2cStreamingPhase::Completed));
    assert_eq!(sender.max_in_flight(), 1);
    assert_eq!(
        sender
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeCall::Stream {
                    content,
                    stream_msg_id,
                    msg_seq,
                    index,
                    input_state,
                    ..
                } => Some((content, stream_msg_id, msg_seq, index, input_state)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![
            ("你好".to_owned(), None, 1, 0, 1),
            (
                "你好，这是内容".to_owned(),
                Some("stream-1".to_owned()),
                1,
                1,
                1
            ),
            (
                "你好，这是内容".to_owned(),
                Some("stream-1".to_owned()),
                1,
                2,
                10,
            ),
        ]
    );
}

#[tokio::test]
async fn throttled_updates_are_serial_and_use_latest_full_text() {
    let events = FakeEventStream::with_delays([
        (Duration::ZERO, CoreResponseEvent::TextDelta("A".to_owned())),
        (
            Duration::from_millis(STREAM_THROTTLE_MS + 30),
            CoreResponseEvent::TextDelta("B".to_owned()),
        ),
        (
            Duration::ZERO,
            CoreResponseEvent::Completed(Box::new(respond_response("AB"))),
        ),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        stream_response("reply-update", None),
        stream_response("reply-complete", None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let contents = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream { content, .. } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contents, vec!["A", "AB", "AB"]);
    assert_eq!(sender.max_in_flight(), 1);
}

#[tokio::test]
async fn first_update_failure_allows_one_ordinary_fallback() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("晚上".to_owned()),
        CoreResponseEvent::TextDelta("好".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("晚上好"))),
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
                stream_msg_id: None,
                msg_seq: 1,
                index: 0,
                input_state: 1,
            },
            FakeCall::Markdown {
                content: "晚上好".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            },
        ]
    );
}

#[tokio::test]
async fn active_update_failure_never_sends_ordinary_full_fallback() {
    let events = FakeEventStream::with_delays([
        (Duration::ZERO, CoreResponseEvent::TextDelta("A".to_owned())),
        (
            Duration::from_millis(STREAM_THROTTLE_MS + 30),
            CoreResponseEvent::TextDelta("B".to_owned()),
        ),
        (
            Duration::ZERO,
            CoreResponseEvent::Completed(Box::new(respond_response("AB"))),
        ),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        Err(ApiError::Unsupported("update")),
        Err(ApiError::Unsupported("complete")),
    ]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;
    assert!(result.is_err());
    assert!(
        sender
            .calls()
            .iter()
            .all(|call| matches!(call, FakeCall::Stream { .. }))
    );
}

#[tokio::test]
async fn completed_failure_does_not_send_ordinary_fallback_or_repeat_complete() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("内容".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("内容"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        Err(ApiError::Unsupported("complete")),
    ]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;
    assert!(result.is_err());
    assert_eq!(
        sender
            .calls()
            .iter()
            .filter(|call| matches!(
                call,
                FakeCall::Stream {
                    input_state: 10,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        sender
            .calls()
            .iter()
            .all(|call| matches!(call, FakeCall::Stream { .. }))
    );
}

#[tokio::test]
async fn completed_stream_writes_complete_ref_idx_with_accepted_text() {
    let config = test_config();
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("引用正文".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("引用正文"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", Some("REFIDX_update_should_not_win")),
        stream_response("reply-complete", Some("REFIDX_complete")),
    ]);
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
        quoted_lookup_found(&ref_index, &config, "REFIDX_complete").as_deref(),
        Some("引用正文")
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_update_should_not_win"),
        None
    );
    assert_eq!(quoted_lookup_found(&ref_index, &config, "stream-1"), None);
}

#[tokio::test]
async fn candidate_rollover_completes_old_once_and_sends_new_reply() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("正在查询相关资料……".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("查询失败，请稍后重试"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-old", Some("REFIDX_old_update")),
        stream_response("reply-old", Some("REFIDX_old_complete")),
    ]);
    let config = test_config();
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

    let calls = sender.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(
                call,
                FakeCall::Stream {
                    input_state: 10,
                    ..
                }
            ))
            .count(),
        1,
        "旧官方流只能完成一次"
    );
    assert_eq!(
        calls
            .iter()
            .filter_map(|call| match call {
                FakeCall::Stream {
                    content,
                    input_state: 10,
                    ..
                } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["正在查询相关资料……"]
    );
    assert_eq!(
        calls
            .iter()
            .filter_map(|call| match call {
                FakeCall::Markdown { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["查询失败，请稍后重试"]
    );
    assert!(!calls.iter().any(|call| matches!(
        call,
        FakeCall::Stream {
            content,
            input_state: 10,
            ..
        } if content == "查询失败，请稍后重试"
    )));
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_old_complete").as_deref(),
        Some("正在查询相关资料……")
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_old_update"),
        None
    );
    assert_eq!(
        quoted_lookup_found(&ref_index, &config, "REFIDX_ordinary_markdown").as_deref(),
        Some("查询失败，请稍后重试")
    );
    assert_eq!(quoted_lookup_found(&ref_index, &config, "stream-old"), None);
}

#[tokio::test]
async fn candidate_prefix_rewrite_rolls_over_for_equal_and_longer_text() {
    for candidate in ["ABXDE", "ABXDEFG"] {
        let events = FakeEventStream::new([
            CoreResponseEvent::TextDelta("ABCDE".to_owned()),
            CoreResponseEvent::Completed(Box::new(respond_response(candidate))),
        ]);
        let sender = FakeStreamSender::new([
            stream_response("stream-old", None),
            stream_response("reply-old", None),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        let calls = sender.calls();
        assert_eq!(
            calls
                .iter()
                .filter_map(|call| match call {
                    FakeCall::Stream {
                        content,
                        input_state: 10,
                        ..
                    } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec!["ABCDE"]
        );
        assert_eq!(
            calls
                .iter()
                .filter_map(|call| match call {
                    FakeCall::Markdown { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![candidate]
        );
    }
}

#[tokio::test]
async fn long_markdown_emoji_and_code_are_sent_as_raw_cumulative_text_without_chunk_limit() {
    let body = format!(
        "# 标题\n\n- 列表\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {{}}\n```\n\n[链接](https://example.com) 👩‍💻 {}",
        "中".repeat(3000)
    );
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta(body.clone()),
        CoreResponseEvent::Completed(Box::new(respond_response(&body))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        stream_response("reply-complete", None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let stream_contents = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream { content, .. } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stream_contents, vec![body.clone(), body]);
    assert!(stream_contents[0].chars().count() > 2000);
}

#[tokio::test]
async fn status_and_progress_paths_do_not_start_official_stream_unnecessarily() {
    let events = FakeEventStream::new([
        CoreResponseEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "正在处理".to_owned(),
        }),
        CoreResponseEvent::Completed(Box::new(respond_response("最终回复"))),
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
async fn progress_policy_sends_one_hint_then_ordinary_final_reply() {
    let events = FakeEventStream::new([
        CoreResponseEvent::Status(CoreResponseStatus {
            kind: CoreResponseStatusKind::AgentStarted,
            text: "小女仆正在处理…".to_owned(),
        }),
        CoreResponseEvent::Completed(Box::new(respond_response("最终回复"))),
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
async fn pending_core_failure_uses_safe_failure_reply() {
    let events = FakeEventStream::new([CoreResponseEvent::Failed(
        qq_maid_core::service::CoreRespondFailure {
            kind: qq_maid_core::service::CoreFailureKind::Internal,
            message: "处理失败".to_owned(),
            retryable: false,
            agent: None,
        },
    )]);
    let sender = FakeStreamSender::new([]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();
    assert!(
        sender
            .calls()
            .iter()
            .any(|call| matches!(call, FakeCall::Text { .. }))
    );
}

#[tokio::test]
async fn pending_completed_stops_typing_before_fallback() {
    let events = FakeEventStream::new([CoreResponseEvent::Completed(Box::new(respond_response(
        "晚上好",
    )))]);
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
}
