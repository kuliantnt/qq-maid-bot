use super::*;

#[tokio::test]
async fn flattened_secondary_quote_stays_text_without_media_download_or_data_url() {
    use crate::gateway::event::{EVENT_GROUP_MESSAGE_CREATE, GatewayEnvelope, parse_group_message};

    let download_count = Arc::new(AtomicUsize::new(0));
    let handler_count = Arc::clone(&download_count);
    let app = Router::new().route(
        "/history.png",
        get(move || {
            let handler_count = Arc::clone(&handler_count);
            async move {
                handler_count.fetch_add(1, Ordering::Relaxed);
                ([(header::CONTENT_TYPE.as_str(), "image/png")], "history")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // 脱敏自 QQ 二次引用真实字段形态：历史附件信息已经被平台拍平进 content，
    // 同时仍可能携带内层结构化 msg_elements。Gateway 只把前者视为普通文本。
    let flattened = format!(
        "[关联消息]\n发送者：member_redacted\n[消息内容]\n请看历史图片\n\
[附件1]\nfilename: history_redacted.png\nfile_id: file_redacted\n\
URL:http://{addr}/history.png?rkey=rkey_redacted"
    );
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: Some("event-redacted".to_owned()),
        d: serde_json::json!({
            "id": "message-redacted",
            "group_openid": "group-redacted",
            "author": {"member_openid": "member-redacted"},
            "content": "继续分析",
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=TMP_current_redacted",
                "ref_msg_idx=REFIDX_quote_redacted"
            ]},
            "msg_elements": [{
                "content": flattened,
                "msg_elements": [{
                    "content": "[图片]",
                    "attachments": [{
                        "content_type": "image/png",
                        "filename": "history_redacted.png",
                        "fileid": "file_redacted",
                        "url": format!("http://{addr}/history.png?rkey=rkey_redacted")
                    }]
                }]
            }]
        }),
    };

    let mut message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_mut().expect("quoted payload");
    assert_eq!(reply.content.as_deref(), Some(flattened.as_str()));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(
        reply.input_parts[0].text_content(),
        Some(flattened.as_str())
    );
    assert!(reply.input_parts.iter().all(|part| part.media().is_none()));
    assert!(reply.media_summaries.is_empty());

    let root_dir = std::env::temp_dir().join(format!(
        "qq-maid-flattened-quote-test-{}",
        MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app-redacted".to_owned(),
        peer_id: "peer-redacted".to_owned(),
        root_dir: root_dir.clone(),
        timeout: Duration::from_secs(1),
        max_bytes: 1024,
    };
    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "message-redacted",
        Some(reply),
    )
    .await;

    assert_eq!(download_count.load(Ordering::Relaxed), 0);
    assert_eq!(media_file_count(&root_dir), 0);
    assert_eq!(
        reply.input_parts[0].text_content(),
        Some(flattened.as_str())
    );
    let serialized = serde_json::to_string(&reply.input_parts).unwrap();
    assert!(serialized.contains("filename: history_redacted.png"));
    assert!(serialized.contains("file_id: file_redacted"));
    assert!(serialized.contains("rkey=rkey_redacted"));
    assert!(!serialized.contains("data:image"));
}

#[tokio::test]
async fn flattened_quote_img_tag_stays_text_without_media_download_or_data_url() {
    use crate::gateway::event::{EVENT_GROUP_MESSAGE_CREATE, GatewayEnvelope, parse_group_message};

    let download_count = Arc::new(AtomicUsize::new(0));
    let handler_count = Arc::clone(&download_count);
    let app = Router::new().route(
        "/history.png",
        get(move || {
            let handler_count = Arc::clone(&handler_count);
            async move {
                handler_count.fetch_add(1, Ordering::Relaxed);
                ([(header::CONTENT_TYPE.as_str(), "image/png")], "history")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let flattened = format!(
        "拍平历史图片：<img src=\"http://{addr}/history.png\">\n\
[附件1] filename: history.png file_id: file-redacted rkey: rkey-redacted"
    );
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: Some("event-redacted".to_owned()),
        d: serde_json::json!({
            "id": "message-redacted",
            "group_openid": "group-redacted",
            "author": {"member_openid": "member-redacted"},
            "content": "继续分析",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_quote_redacted"]},
            "msg_elements": [{"content": flattened}]
        }),
    };

    let mut message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_mut().expect("quoted payload");
    assert_eq!(reply.content.as_deref(), Some(flattened.as_str()));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(
        reply.input_parts[0].text_content(),
        Some(flattened.as_str())
    );
    assert_eq!(
        reply
            .input_parts
            .iter()
            .filter(|part| matches!(part, MessageInputPart::Image { .. }))
            .count(),
        0
    );

    let root_dir = std::env::temp_dir().join(format!(
        "qq-maid-flattened-img-tag-test-{}",
        MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app-redacted".to_owned(),
        peer_id: "peer-redacted".to_owned(),
        root_dir: root_dir.clone(),
        timeout: Duration::from_secs(1),
        max_bytes: 1024,
    };
    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "message-redacted",
        Some(reply),
    )
    .await;

    assert_eq!(download_count.load(Ordering::Relaxed), 0);
    assert_eq!(media_file_count(&root_dir), 0);
    let serialized = serde_json::to_string(&reply.input_parts).unwrap();
    assert!(serialized.contains("<img src="));
    assert!(!serialized.contains("data:image"));
}

#[tokio::test]
async fn quoted_images_with_same_filename_download_and_send_only_first_image() {
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
        filename: Some(if number == 1 {
            " Same.PNG ".to_owned()
        } else {
            "same.png".to_owned()
        }),
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

#[test]
fn quoted_images_without_filename_are_not_deduplicated() {
    let mut parts = vec![
        MessageInputPart::image(MessageMedia {
            url: Some("https://example.test/first.png".to_owned()),
            ..Default::default()
        }),
        MessageInputPart::image(MessageMedia {
            url: Some("https://example.test/second.png".to_owned()),
            ..Default::default()
        }),
    ];

    deduplicate_quoted_images_by_filename(&mut parts);

    assert_eq!(parts.len(), 2);
}

#[tokio::test]
async fn same_filename_in_different_quoted_payloads_is_not_cross_deduplicated() {
    let app = Router::new()
        .route(
            "/first.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "first") }),
        )
        .route(
            "/second.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "second") }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let make_reply = |message_id: &str, path: &str| MessageReply {
        message_id: message_id.to_owned(),
        ref_msg_idx: Some(message_id.to_owned()),
        content: None,
        input_parts: vec![MessageInputPart::image(MessageMedia {
            mime_type: Some("image/png".to_owned()),
            filename: Some("same.png".to_owned()),
            url: Some(format!("http://{addr}/{path}.png")),
            status: MediaStatus::Available,
            ..Default::default()
        })],
        media_summaries: Vec::new(),
    };
    let mut first = make_reply("quoted-first", "first");
    let mut second = make_reply("quoted-second", "second");
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app".to_owned(),
        peer_id: "peer".to_owned(),
        root_dir: std::env::temp_dir().join(format!(
            "qq-maid-quoted-media-scope-test-{}",
            MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        )),
        timeout: Duration::from_secs(3),
        max_bytes: 1024,
    };

    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-first",
        Some(&mut first),
    )
    .await;
    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-second",
        Some(&mut second),
    )
    .await;

    let first_media = first.input_parts[0].media().unwrap();
    let second_media = second.input_parts[0].media().unwrap();
    assert_eq!(
        std::fs::read(first_media.local_path.as_ref().unwrap()).unwrap(),
        b"first"
    );
    assert_eq!(
        std::fs::read(second_media.local_path.as_ref().unwrap()).unwrap(),
        b"second"
    );
}

#[tokio::test]
async fn current_and_quoted_same_filename_are_not_cross_deduplicated() {
    let app = Router::new()
        .route(
            "/current.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "current") }),
        )
        .route(
            "/quoted.png",
            get(|| async { ([(header::CONTENT_TYPE.as_str(), "image/png")], "quoted") }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let current_attachment = Attachment {
        content_type: Some("image/png".to_owned()),
        filename: Some("same.png".to_owned()),
        url: Some(format!("http://{addr}/current.png")),
        size_bytes: None,
        media_id: None,
        file_id: Some("current-file".to_owned()),
        attachment_id: None,
        asr_refer_text: None,
        voice_wav_url: None,
    };
    let mut current_parts = vec![MessageInputPart::image(MessageMedia {
        mime_type: current_attachment.content_type.clone(),
        filename: current_attachment.filename.clone(),
        url: current_attachment.url.clone(),
        file_id: current_attachment.file_id.clone(),
        status: MediaStatus::Available,
        ..Default::default()
    })];
    let mut quoted = MessageReply {
        message_id: "quoted".to_owned(),
        ref_msg_idx: Some("quoted".to_owned()),
        content: None,
        input_parts: vec![MessageInputPart::image(MessageMedia {
            mime_type: Some("image/png".to_owned()),
            filename: Some("same.png".to_owned()),
            url: Some(format!("http://{addr}/quoted.png")),
            file_id: Some("quoted-file".to_owned()),
            status: MediaStatus::Available,
            ..Default::default()
        })],
        media_summaries: Vec::new(),
    };
    let context = MediaFetchContext {
        platform: "qq_official",
        app_id: "app".to_owned(),
        peer_id: "peer".to_owned(),
        root_dir: std::env::temp_dir().join(format!(
            "qq-maid-current-quoted-scope-test-{}",
            MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        )),
        timeout: Duration::from_secs(3),
        max_bytes: 1024,
    };

    fetch_qq_official_image_attachments(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-current",
        &mut current_parts,
        &[current_attachment],
    )
    .await;
    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &context,
        "msg-current",
        Some(&mut quoted),
    )
    .await;

    let current_media = current_parts[0].media().unwrap();
    let quoted_media = quoted.input_parts[0].media().unwrap();
    assert_eq!(
        std::fs::read(current_media.local_path.as_ref().unwrap()).unwrap(),
        b"current"
    );
    assert_eq!(
        std::fs::read(quoted_media.local_path.as_ref().unwrap()).unwrap(),
        b"quoted"
    );
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
