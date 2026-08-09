use super::*;

#[test]
fn injects_qq_audio_asr_as_user_text_and_keeps_wav_url() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "voice-1",
            "author": {"user_openid": "user-1"},
            "attachments": [{
                "content_type": "audio/silk",
                "filename": "voice.silk",
                "url": "https://example.test/raw.silk?token=secret",
                "voice_wav_url": "https://example.test/voice.wav?token=secret",
                "asr_refer_text": "明天下午提醒我开会"
            }]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(message.input_parts.len(), 2);
    assert!(matches!(
        &message.input_parts[0],
        MessageInputPart::File { media }
            if media.url.as_deref() == Some("https://example.test/voice.wav?token=secret")
    ));
    assert!(matches!(
        &message.input_parts[1],
        MessageInputPart::Text { text, source: Some(TextSource::Transcript) }
            if text == "[语音转文字] 明天下午提醒我开会"
    ));
}

#[test]
fn accepts_qq_voice_content_type_for_asr() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "voice-content-type",
            "author": {"user_openid": "user-1"},
            "attachments": [{
                "content_type": "voice",
                "url": "https://example.test/voice",
                "asr_refer_text": "请提醒我下午开会"
            }]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert!(message.input_parts.iter().any(|part| matches!(
        part,
        MessageInputPart::Text {
            text,
            source: Some(TextSource::Transcript)
        } if text == "[语音转文字] 请提醒我下午开会"
    )));
}

#[test]
fn injects_multiple_audio_transcripts_once_and_preserves_special_text() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "voice-many",
            "author": {"user_openid": "user-1"},
            "attachments": [
                {
                    "content_type": "audio/ogg",
                    "filename": "one.ogg",
                    "asr_refer_text": "第一段\n#ops status <tag>"
                },
                {
                    "content_type": "audio/wav",
                    "filename": "two.wav",
                    "asr_refer_text": "第二段"
                },
                {
                    "content_type": "audio/wav",
                    "filename": "duplicate.wav",
                    "asr_refer_text": "第二段"
                }
            ]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();
    let transcripts = message
        .input_parts
        .iter()
        .filter_map(MessageInputPart::text_content)
        .collect::<Vec<_>>();

    assert_eq!(
        transcripts,
        vec![
            "[语音转文字] 第一段\n#ops status <tag>",
            "[语音转文字] 第二段"
        ]
    );
    assert_eq!(
        message
            .input_parts
            .iter()
            .filter(|part| matches!(part, MessageInputPart::File { .. }))
            .count(),
        3
    );
}

#[test]
fn ignores_empty_asr_and_asr_on_non_audio_attachments() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "voice-empty",
            "author": {"user_openid": "user-1"},
            "attachments": [
                {
                    "content_type": "audio/wav",
                    "filename": "empty.wav",
                    "asr_refer_text": "  \n "
                },
                {
                    "content_type": "application/pdf",
                    "filename": "report.pdf",
                    "asr_refer_text": "不得注入"
                }
            ]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(message.input_parts.len(), 2);
    assert!(
        message
            .input_parts
            .iter()
            .all(|part| part.text_content().is_none())
    );
}

#[test]
fn c2c_img_file_url_content_is_sanitized_and_kept_unreadable() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-file-image",
            "author": {"user_openid": "user-1"},
            "content": r#"<img src="file://C:\Users\ThinkPad\Documents\Tencent Files\123\Image\a.jpg" />抱抱你"#
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();
    let fallback = message
        .input_parts
        .iter()
        .map(MessageInputPart::fallback_text)
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(message.content, "[图片 image/jpeg: a.jpg]抱抱你");
    assert_eq!(fallback, "[图片 image/jpeg: a.jpg]抱抱你");
    assert!(!message.content.contains("C:\\Users"));
    assert!(!fallback.contains("Tencent Files"));
    assert!(matches!(
        &message.input_parts[0],
        MessageInputPart::Image { media }
            if media.filename.as_deref() == Some("a.jpg")
                && media.remote_url().is_none()
                && media.status == MediaStatus::MissingReadableUrl
    ));
    assert_eq!(message.input_parts[1].text_content(), Some("抱抱你"));
}

#[test]
fn c2c_img_placeholders_reuse_attachment_slots_without_duplicates() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-mixed-images",
            "author": {"user_openid": "user-1"},
            "content": r#"前<img src="file://C:\Images\a.png" />中<img src="file://C:\Images\b.webp" />后"#,
            "attachments": [
                {
                    "content_type": "image",
                    "filename": "a.png",
                    "url": "https://example.test/a.png"
                },
                {
                    "content_type": "image/webp",
                    "filename": "b.webp",
                    "url": "https://example.test/b.webp"
                }
            ]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(message.input_parts.len(), 5);
    assert_eq!(message.input_parts[0].text_content(), Some("前"));
    assert_eq!(message.input_parts[2].text_content(), Some("中"));
    assert_eq!(message.input_parts[4].text_content(), Some("后"));
    assert_eq!(
        message
            .input_parts
            .iter()
            .filter(|part| matches!(part, MessageInputPart::Image { .. }))
            .count(),
        2
    );
    let MessageInputPart::Image { media: first } = &message.input_parts[1] else {
        panic!("expected first image part");
    };
    let MessageInputPart::Image { media: second } = &message.input_parts[3] else {
        panic!("expected second image part");
    };
    assert_eq!(first.remote_url(), Some("https://example.test/a.png"));
    assert_eq!(first.mime_type.as_deref(), Some("image"));
    assert_eq!(second.remote_url(), Some("https://example.test/b.webp"));
    assert_eq!(second.mime_type.as_deref(), Some("image/webp"));
}
