//! Completed 事件、最终帧与结构化媒体续发测试。

use super::*;

#[tokio::test]
async fn stream_completed_flushes_pending_delta_before_final() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("晚".to_owned()),
        CoreResponseEvent::TextDelta("上".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("晚上"))),
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
async fn long_first_delta_is_split_into_legal_unicode_chunks() {
    let body = "中".repeat(3000);
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta(body.clone()),
        CoreResponseEvent::Completed(Box::new(respond_response(&body))),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let chunks = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream {
                content,
                stream_state_value: 1,
                ..
            } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(chunks.iter().map(String::as_str).collect::<String>(), body);
    assert_eq!(chunks.len(), 2);
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.chars().count() <= STREAM_CHUNK_CHAR_LIMIT)
    );
}

#[tokio::test]
async fn completed_flush_splits_large_pending_delta_before_final() {
    let tail = "中".repeat(3000);
    let final_text = format!("首{tail}");
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("首".to_owned()),
        CoreResponseEvent::TextDelta(tail.clone()),
        CoreResponseEvent::Completed(Box::new(respond_response(&final_text))),
    ]);
    let sender = FakeStreamSender::new([
        Ok(Some("stream-1".to_owned())),
        Ok(None),
        Ok(None),
        Ok(None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let calls = sender.calls();
    let stream_contents = calls
        .iter()
        .filter_map(|call| match call {
            FakeCall::Stream { content, .. } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stream_contents.len(), 4);
    assert!(
        stream_contents
            .iter()
            .all(|content| content.chars().count() <= STREAM_CHUNK_CHAR_LIMIT)
    );
    assert_eq!(stream_contents[0], "首");
    assert_eq!(stream_contents[1].chars().count(), STREAM_CHUNK_CHAR_LIMIT);
    assert_eq!(stream_contents[2].chars().count(), 1000);
    assert_eq!(stream_contents[3], STREAM_FINAL_MARKER);
    assert_eq!(
        format!(
            "{}{}{}",
            stream_contents[0], stream_contents[1], stream_contents[2]
        ),
        final_text
    );
}

#[tokio::test]
async fn broken_active_final_does_not_retry_oversized_state_ten_chunk() {
    let tail = "中".repeat(3000);
    let final_text = format!("首{tail}");
    let events = FakeEventStream::with_delays([
        (
            Duration::ZERO,
            CoreResponseEvent::TextDelta("首".to_owned()),
        ),
        (
            Duration::from_millis(STREAM_THROTTLE_MS + 50),
            CoreResponseEvent::TextDelta(tail),
        ),
        (
            Duration::ZERO,
            CoreResponseEvent::Completed(Box::new(respond_response(&final_text))),
        ),
    ]);
    let sender = FakeStreamSender::new([
        Ok(Some("stream-1".to_owned())),
        Err(ApiError::Unsupported("stream")),
        Ok(None),
        Ok(None),
        Ok(None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let calls = sender.calls();
    let final_calls = calls
        .iter()
        .filter_map(|call| match call {
            FakeCall::Stream {
                content,
                stream_state_value: 10,
                ..
            } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(final_calls, vec![&STREAM_FINAL_MARKER.to_owned()]);
    assert!(calls.iter().all(|call| match call {
        FakeCall::Stream { content, .. } => content.chars().count() <= STREAM_CHUNK_CHAR_LIMIT,
        _ => true,
    }));
}

#[tokio::test]
async fn stream_completed_without_delta_uses_ordinary_reply_path() {
    let events = FakeEventStream::new([CoreResponseEvent::Completed(Box::new(respond_response(
        "晚上好",
    )))]);
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
async fn stream_pending_completed_stops_typing_before_ordinary_reply() {
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
    assert!(matches!(
        sender.calls().as_slice(),
        [FakeCall::Markdown { .. }]
    ));
}

#[tokio::test]
async fn stream_pending_completed_sends_ordinary_reply_once() {
    let events = FakeEventStream::new([
        CoreResponseEvent::Completed(Box::new(respond_response("晚上好"))),
        CoreResponseEvent::Completed(Box::new(respond_response("不应重复发送"))),
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
        CoreResponseEvent::TextDelta("好".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("好"))),
        CoreResponseEvent::Completed(Box::new(respond_response("好"))),
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
async fn active_text_stream_sends_completed_image_then_only_its_fallback() {
    let mut response = respond_response("说明");
    response.output = Some(AssistantOutput {
        text_fallback: String::new(),
        markdown: None,
        parts: vec![
            OutputPart::Text {
                text: "说明".to_owned(),
            },
            OutputPart::Image {
                media: OutputMedia {
                    data_base64: Some("aGVsbG8=".to_owned()),
                    fallback_text: Some("图片发送失败".to_owned()),
                    ..OutputMedia::default()
                },
            },
        ],
    });
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("说明".to_owned()),
        CoreResponseEvent::Completed(Box::new(response)),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);
    let config = test_config();

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &config)
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec![
            FakeCall::Stream {
                content: "说明".to_owned(),
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
            FakeCall::Image,
            FakeCall::Text {
                content: "图片发送失败".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            },
        ]
    );
}
