//! 官方 StreamSession 的最终累计更新、完成顺序和候选正文边界测试。

use super::*;

#[tokio::test]
async fn completed_response_extends_accepted_prefix_before_complete() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("前缀".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("前缀追加"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        stream_response("reply-update", None),
        stream_response("reply-complete", None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let calls = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream {
                content,
                input_state,
                ..
            } => Some((content, input_state)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            ("前缀".to_owned(), 1),
            ("前缀追加".to_owned(), 1),
            ("前缀追加".to_owned(), 10),
        ]
    );
}

#[tokio::test]
async fn complete_is_attempted_once_after_a_failed_final_update() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("稳定前缀".to_owned()),
        CoreResponseEvent::Completed(Box::new(respond_response("稳定前缀追加"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        Err(ApiError::Unsupported("final update")),
        stream_response("reply-complete", None),
    ]);

    let result =
        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;
    assert!(result.is_err());

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
        1
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
        vec!["稳定前缀"]
    );
}

#[tokio::test]
async fn completed_waits_for_a_pending_delta_before_complete() {
    let events = FakeEventStream::new([
        CoreResponseEvent::TextDelta("已接受".to_owned()),
        CoreResponseEvent::TextDelta("的尾部".to_owned()),
        // Core 的最终投影可能尚未包含刚到达的最后一个 delta；Gateway 累计值优先。
        CoreResponseEvent::Completed(Box::new(respond_response("已接受"))),
    ]);
    let sender = FakeStreamSender::new([
        stream_response("stream-1", None),
        stream_response("reply-update", None),
        stream_response("reply-complete", None),
    ]);

    stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
        .await
        .unwrap();

    let calls = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream {
                content,
                input_state,
                ..
            } => Some((content, input_state)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            ("已接受".to_owned(), 1),
            ("已接受的尾部".to_owned(), 1),
            ("已接受的尾部".to_owned(), 10),
        ]
    );
}
