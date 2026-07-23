use super::*;

#[tokio::test]
async fn quoted_images_with_same_filename_keep_only_first_image() {
    let app = Router::new()
        .route(
            "/1.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "one") }),
        )
        .route(
            "/2.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "two") }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let make_media = |number: u8| MessageMedia {
        mime_type: Some("image/png".to_owned()),
        filename: Some("same.png".to_owned()),
        size_bytes: Some(3),
        url: Some(format!("http://{addr}/{number}.png")),
        file_id: Some(format!("file-{number}")),
        status: MediaStatus::Available,
        ..Default::default()
    };
    let mut reply = MessageReply {
        message_id: "quoted".to_owned(),
        ref_msg_idx: Some("quoted".to_owned()),
        content: Some("引用正文".to_owned()),
        input_parts: vec![
            MessageInputPart::text("引用正文"),
            MessageInputPart::image(make_media(1)),
            MessageInputPart::image(make_media(2)),
        ],
        media_summaries: Vec::new(),
    };
    let root_dir = std::env::temp_dir().join(format!(
        "qq-maid-quoted-media-test-{}",
        MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app".to_owned(),
        peer_id: "peer".to_owned(),
        root_dir,
        timeout: Duration::from_secs(3),
        max_bytes: 1024,
    };

    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-current",
        Some(&mut reply),
    )
    .await;

    let media = reply
        .input_parts
        .iter()
        .filter_map(MessageInputPart::media)
        .collect::<Vec<_>>();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].url, None);
    assert_eq!(
        std::fs::read(media[0].local_path.as_ref().unwrap()).unwrap(),
        b"one"
    );
    assert_eq!(reply.media_summaries.len(), 1);
}

#[tokio::test]
async fn quoted_images_with_different_filenames_keep_original_order() {
    let app = Router::new()
        .route(
            "/1.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "one") }),
        )
        .route(
            "/2.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "two") }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut reply = MessageReply {
        message_id: "quoted".to_owned(),
        ref_msg_idx: Some("quoted".to_owned()),
        content: None,
        input_parts: [1_u8, 2]
            .into_iter()
            .map(|number| {
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    filename: Some(format!("image-{number}.png")),
                    url: Some(format!("http://{addr}/{number}.png")),
                    status: MediaStatus::Available,
                    ..Default::default()
                })
            })
            .collect(),
        media_summaries: Vec::new(),
    };
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app".to_owned(),
        peer_id: "peer".to_owned(),
        root_dir: std::env::temp_dir().join(format!(
            "qq-maid-quoted-media-order-test-{}",
            MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        )),
        timeout: Duration::from_secs(3),
        max_bytes: 1024,
    };

    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-current",
        Some(&mut reply),
    )
    .await;

    let media = reply
        .input_parts
        .iter()
        .filter_map(MessageInputPart::media)
        .collect::<Vec<_>>();
    assert_eq!(media.len(), 2);
    assert_eq!(
        std::fs::read(media[0].local_path.as_ref().unwrap()).unwrap(),
        b"one"
    );
    assert_eq!(
        std::fs::read(media[1].local_path.as_ref().unwrap()).unwrap(),
        b"two"
    );
}

#[tokio::test]
async fn quoted_image_timeout_marks_media_failed_without_losing_text() {
    let app = Router::new().route(
        "/slow.png",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            ([(header::CONTENT_TYPE.as_str(), "image/png")], "late")
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut reply = MessageReply {
        message_id: "quoted".to_owned(),
        ref_msg_idx: Some("quoted".to_owned()),
        content: Some("引用正文".to_owned()),
        input_parts: vec![
            MessageInputPart::text("引用正文"),
            MessageInputPart::image(MessageMedia {
                mime_type: Some("image/png".to_owned()),
                filename: Some("slow.png".to_owned()),
                url: Some(format!("http://{addr}/slow.png")),
                status: MediaStatus::Available,
                ..Default::default()
            }),
        ],
        media_summaries: Vec::new(),
    };
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app".to_owned(),
        peer_id: "peer".to_owned(),
        root_dir: std::env::temp_dir(),
        timeout: Duration::from_millis(20),
        max_bytes: 1024,
    };

    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-current",
        Some(&mut reply),
    )
    .await;

    assert_eq!(reply.content.as_deref(), Some("引用正文"));
    assert_eq!(reply.input_parts[0].text_content(), Some("引用正文"));
    let media = reply.input_parts[1].media().unwrap();
    assert_eq!(media.status, MediaStatus::DownloadFailed);
    assert_eq!(media.url, None);
    assert!(media.local_path.is_none());
}
