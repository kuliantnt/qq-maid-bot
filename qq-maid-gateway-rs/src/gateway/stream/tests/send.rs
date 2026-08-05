use super::*;

#[tokio::test]
async fn stream_chunk_failure_does_not_advance_next_index() {
    let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);
    let mut stream_state = C2cStreamState::new();
    stream_state.stream_id = Some("stream-1".to_owned());
    stream_state.index = 1;
    let mut content = "失败分片".to_owned();

    let result = send_stream_chunk(
        &sender,
        "user-1",
        Some("msg-1"),
        &mut content,
        &mut stream_state,
        1,
        false,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(stream_state.index, 1);
    assert_eq!(content, "失败分片");
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
async fn stream_chunk_failure_keeps_failed_and_following_content_after_successful_prefix() {
    let sender = FakeStreamSender::new([Ok(None), Err(ApiError::Unsupported("stream"))]);
    let mut stream_state = C2cStreamState::new();
    stream_state.stream_id = Some("stream-1".to_owned());
    let mut content = "中".repeat(STREAM_CHUNK_CHAR_LIMIT + 1);

    let result = send_stream_chunk(
        &sender,
        "user-1",
        Some("msg-1"),
        &mut content,
        &mut stream_state,
        1,
        false,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(stream_state.index, 1);
    assert_eq!(content.chars().count(), 1);
    assert_eq!(
        sender
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeCall::Stream { content, .. } => Some(content.chars().count()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![STREAM_CHUNK_CHAR_LIMIT, 1]
    );
}

#[tokio::test]
async fn stream_chunk_preserves_grapheme_clusters_at_chunk_boundary() {
    let body = format!("{}👩‍💻e\u{301}", "中".repeat(STREAM_CHUNK_CHAR_LIMIT - 1));
    let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);
    let mut stream_state = C2cStreamState::new();
    let mut content = body.clone();

    send_stream_chunk(
        &sender,
        "user-1",
        Some("msg-1"),
        &mut content,
        &mut stream_state,
        1,
        false,
    )
    .await
    .unwrap();

    assert!(content.is_empty());
    let chunks = sender
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeCall::Stream { content, .. } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(chunks.iter().map(String::as_str).collect::<String>(), body);
    assert!(chunks.iter().all(|chunk| {
        !chunk.ends_with('‍') && chunk.chars().count() <= STREAM_CHUNK_CHAR_LIMIT
    }));
    assert_eq!(
        chunks.iter().filter(|chunk| chunk.contains("👩‍💻")).count(),
        1
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.contains("e\u{301}"))
            .count(),
        1
    );
}

#[tokio::test]
async fn stream_final_success_commits_next_index() {
    let sender = FakeStreamSender::new([Ok(None)]);
    let mut stream_state = C2cStreamState::new();
    stream_state.stream_id = Some("stream-1".to_owned());
    stream_state.index = 2;
    let mut content = "最终正文".to_owned();

    send_stream_end(
        &sender,
        "user-1",
        Some("msg-1"),
        &mut content,
        &mut stream_state,
    )
    .await
    .unwrap();

    assert_eq!(stream_state.index, 3);
    assert!(content.is_empty());
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
