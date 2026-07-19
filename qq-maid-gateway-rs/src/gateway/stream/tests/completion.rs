//! Completed 事件、最终帧与结构化媒体续发测试。

use super::*;

#[tokio::test]
async fn stream_completed_flushes_pending_delta_before_final() {
    let events = FakeEventStream::new([
        RespondEvent::TextDelta("晚".to_owned()),
        RespondEvent::TextDelta("上".to_owned()),
        RespondEvent::Completed(Box::new(respond_response("晚上"))),
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
    let events = FakeEventStream::new([RespondEvent::Completed(Box::new(respond_response(
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
    let events = FakeEventStream::new([RespondEvent::Completed(Box::new(respond_response(
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
        RespondEvent::Completed(Box::new(respond_response("晚上好"))),
        RespondEvent::Completed(Box::new(respond_response("不应重复发送"))),
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
        RespondEvent::Completed(Box::new(respond_response("好"))),
        RespondEvent::Completed(Box::new(respond_response("好"))),
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
        RespondEvent::TextDelta("说明".to_owned()),
        RespondEvent::Completed(Box::new(response)),
    ]);
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);
    let mut config = test_config();
    config.enable_image = true;

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
