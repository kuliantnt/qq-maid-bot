use super::*;
use serde_json::json;

mod quote_boundary;

#[test]
fn parses_c2c_message_create() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-1",
            "author": {"user_openid": "user-1"},
            "content": "你好",
            "timestamp": "2026-06-10T12:00:00+08:00",
            "attachments": [{
                "content_type": "image/jpeg",
                "filename": "a.jpg",
                "url": "https://example.test/a.jpg"
            }]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(message.message_id, "msg-1");
    assert_eq!(message.user_openid, "user-1");
    assert_eq!(message.content, "你好");
    assert_eq!(message.reply, None);
    assert_eq!(
        message.timestamp.as_deref(),
        Some("2026-06-10T12:00:00+08:00")
    );
    assert_eq!(
        message.first_message_timestamp.as_deref(),
        Some("2026-06-10T12:00:00+08:00")
    );
    assert_eq!(
        message.last_message_timestamp.as_deref(),
        Some("2026-06-10T12:00:00+08:00")
    );
    assert_eq!(message.attachments.len(), 1);
}

#[test]
fn normalizes_ark_parallel_and_chat_history_without_turning_them_into_commands() {
    let ark = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "ark-1",
            "author": {"user_openid": "user-1"},
            "content": "",
            "message_type": 3,
            "message_scene": {"ext": ["msg_idx=idx-ark", "auth_token=should-not-propagate"]},
            "ark_data": {"prompt": "分享", "type": "news", "ark_name": "图文", "fields": {"title": "标题", "jump_url": "https://example.test/card?token=secret"}}
        }),
    };
    let ark_message = parse_c2c_message(&ark).unwrap().unwrap();
    assert!(ark_message.content.is_empty());
    let ark_summary = ark_message.input_parts[0].text_content().unwrap();
    assert!(ark_summary.contains("[ARK 卡片]"));
    assert!(ark_summary.contains("url: https://example.test/card?token=***"));
    assert!(!ark_summary.contains("auth_token"));
    assert!(!ark_summary.contains("token=secret"));

    let parallel = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "parallel-1",
            "author": {"user_openid": "user-1"},
            "content": "当前正文",
            "message_type": 101,
            "msg_elements": [{"content": "第一段"}, {"content": "第二段", "msg_elements": [{"content": "第三段"}]}]
        }),
    };
    let parallel_message = parse_c2c_message(&parallel).unwrap().unwrap();
    let texts = parallel_message
        .input_parts
        .iter()
        .filter_map(MessageInputPart::text_content)
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["当前正文", "第一段", "第二段", "第三段"]);

    let history = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "history-1",
            "author": {"user_openid": "user-1"},
            "message_type": 102,
            "msg_elements": [{"content": "聊天记录"}]
        }),
    };
    let history_message = parse_c2c_message(&history).unwrap().unwrap();
    assert_eq!(
        history_message.input_parts[0].text_content(),
        Some("聊天记录")
    );
}

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

#[test]
fn ignores_other_events() {
    let envelope = GatewayEnvelope {
        op: 0,
        d: json!({}),
        s: None,
        t: Some("READY".to_owned()),
        id: None,
    };

    assert!(parse_c2c_message(&envelope).unwrap().is_none());
}

#[test]
fn parses_group_at_message_create() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-1",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "/rss"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.message_id, "msg-1");
    assert_eq!(message.group_openid, "group-1");
    assert_eq!(message.member_openid.as_deref(), Some("member-1"));
    assert_eq!(message.content, "/rss");
    assert_eq!(message.event_type, GroupEventType::GroupAtMessage);
}

#[test]
fn parses_group_message_member_openid_from_top_level() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-top-member",
            "group_openid": "group-1",
            "member_openid": "member-2",
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.member_openid.as_deref(), Some("member-2"));
}

#[test]
fn parses_group_message_with_top_member_and_user_openid() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-top-both",
            "group_openid": "group-1",
            "member_openid": "member-top",
            "user_openid": "user-top",
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.member_openid.as_deref(), Some("member-top"));
}

#[test]
fn prefers_author_member_openid_over_top_level_group_identity() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-author-priority",
            "group_openid": "group-1",
            "member_openid": "member-top",
            "user_openid": "user-top",
            "author": {"member_openid": "member-author"},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.member_openid.as_deref(), Some("member-author"));
}

#[test]
fn parses_group_message_with_legacy_author_id_fallback() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-legacy-author-id",
            "group_openid": "group-1",
            "author": {"id": "legacy-author-id"},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.member_openid.as_deref(), Some("legacy-author-id"));
}

#[test]
fn group_message_allows_missing_member_identity() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-no-member",
            "group_openid": "group-1",
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.member_openid, None);
}

#[test]
fn parses_plain_group_message_create_with_bot_flags() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-2",
            "group_openid": "group-1",
            "author": {"member_openid": "member-2", "is_bot": true},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.message_id, "msg-2");
    assert_eq!(message.member_openid.as_deref(), Some("member-2"));
    assert_eq!(message.event_type, GroupEventType::GroupMessage);
    assert!(message.author_is_bot);
    assert!(!message.author_is_self);
}

#[test]
fn parses_group_message_structured_mentions() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-mentions",
            "group_openid": "group-1",
            "author": {"member_openid": "member-2", "member_role": "owner"},
            "content": " /help ",
            "mentions": [
                {"id": "owner-id", "is_you": false, "member_role": "owner"},
                {"id": "appid", "is_you": true, "member_role": "admin"},
                {"user_openid": "user-openid", "is_you": false, "member_role": "member"},
                {"member_openid": "member-openid", "member_role": "future-role"}
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.content, "/help");
    assert_eq!(message.member_role, Some(GroupMemberRole::Owner));
    assert_eq!(
        message.mentions,
        vec![
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Owner),
                target_id: Some("owner-id".to_owned())
            },
            GroupMention {
                is_you: true,
                member_role: Some(GroupMemberRole::Admin),
                target_id: Some("appid".to_owned())
            },
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Member),
                target_id: Some("user-openid".to_owned())
            },
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Unknown),
                target_id: Some("member-openid".to_owned())
            }
        ]
    );
}

#[test]
fn parses_group_message_self_flag_from_top_level() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-3",
            "group_openid": "group-1",
            "author": {"member_openid": "member-3"},
            "content": "hello",
            "is_self": true
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert!(message.author_is_self);
}

#[test]
fn parses_group_at_message_with_duplicate_openid_fields() {
    // QQ API 有时同时发送 group_openid 和 openid，openid 不应被当作 group_openid 的别名
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-dup",
            "group_openid": "group-1",
            "openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.group_openid, "group-1");
    assert_eq!(message.member_openid.as_deref(), Some("member-1"));
}

#[test]
fn parses_group_message_from_legacy_group_id_field() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-legacy",
            "group_id": "group-legacy",
            "author": {"member_openid": "member-1"},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.group_openid, "group-legacy");
    assert_eq!(message.member_openid.as_deref(), Some("member-1"));
}

#[test]
fn prefers_group_openid_when_group_id_is_also_present() {
    // QQ API 兼容期内可能同时下发新旧群字段，主字段应优先使用 group_openid。
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-both-group-fields",
            "group_openid": "group-new",
            "group_id": "group-old",
            "author": {"member_openid": "member-1"},
            "content": "hello"
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.group_openid, "group-new");
    assert_eq!(message.member_openid.as_deref(), Some("member-1"));
}

#[test]
fn parses_reply_message_id_from_cq_code() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-1",
            "author": {"user_openid": "user-1"},
            "content": "[CQ:reply,id=quoted-1]你好"
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(
        message.reply,
        Some(MessageReply {
            message_id: "quoted-1".to_owned(),
            ref_msg_idx: None,
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        })
    );
}

#[test]
fn parses_reply_message_id_from_explicit_reply_field() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-1",
            "author": {"user_openid": "user-1"},
            "content": "你好",
            "reply": {
                "message_id": "quoted-2"
            }
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(
        message.reply,
        Some(MessageReply {
            message_id: "quoted-2".to_owned(),
            ref_msg_idx: None,
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        })
    );
}

#[test]
fn parses_reply_message_id_from_quote_field() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-1",
            "author": {"user_openid": "user-1"},
            "content": "你好",
            "quote": {
                "message_id": "quoted-3"
            }
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();

    assert_eq!(
        message.reply,
        Some(MessageReply {
            message_id: "quoted-3".to_owned(),
            ref_msg_idx: None,
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        })
    );
}

#[test]
fn parses_group_refidx_from_message_scene_ext() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "这条是什么意思",
            "message_scene": {
                "ext": [
                    "msg_idx=REFIDX_current",
                    "ref_msg_idx=REFIDX_quoted"
                ]
            }
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(
        message.reply,
        Some(MessageReply {
            message_id: "REFIDX_quoted".to_owned(),
            ref_msg_idx: Some("REFIDX_quoted".to_owned()),
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        })
    );
}

#[test]
fn parses_qq_quote_msg_element_as_payload_fallback() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: Some(42),
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "查看这条",
            "message_type": 103,
            "message_scene": {
                "ext": [
                    "msg_idx=REFIDX_current",
                    "ref_msg_idx=REFIDX_quoted"
                ]
            },
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "content": "被引用原文",
                "attachments": [{
                    "content_type": "image/png",
                    "filename": "quoted.png",
                    "url": "https://example.test/quoted.png"
                }]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
    assert_eq!(reply.message_id, "REFIDX_quoted");
    assert_eq!(reply.content.as_deref(), Some("被引用原文"));
    assert_eq!(reply.input_parts.len(), 2);
    assert_eq!(reply.media_summaries.len(), 1);
    assert!(matches!(
        reply.input_parts[1],
        MessageInputPart::Image { .. }
    ));
}

#[test]
fn parses_plain_group_quote_from_structured_msg_elements() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1", "member_role": "admin"},
            "content": " 取event",
            "message_scene": {
                "ext": [
                    "ref_msg_idx=REFIDX_quoted",
                    "msg_idx=REFIDX_current",
                    "auth_token=redacted-test-token"
                ]
            },
            "message_type": 103,
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "message_type": 103,
                "content": "感谢"
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(message.content, "取event");
    assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
    assert_eq!(reply.content.as_deref(), Some("感谢"));
    assert_eq!(reply.input_parts.len(), 1);
    assert!(matches!(
        &reply.input_parts[0],
        MessageInputPart::Text { text, source: Some(TextSource::Quote) } if text == "感谢"
    ));
    assert!(reply.media_summaries.is_empty());
}

#[test]
fn msg_elements_are_all_treated_as_quote_content() {
    // 根据 QQ 最新文档，msg_elements 中的全部元素均属于引用内容。
    // 当前正文只从顶层 content 取得。
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "author": {"user_openid": "user-1"},
            "content": "这条正常么？",
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=REFIDX_current",
                "ref_msg_idx=REFIDX_quoted"
            ]},
            "msg_elements": [
                {"msg_idx": "REFIDX_quoted", "content": "OK"}
            ]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "这条正常么？");
    assert_eq!(message.input_parts[0].text_content(), Some("这条正常么？"));
    assert_eq!(reply.content.as_deref(), Some("OK"));
    assert_eq!(reply.input_parts[0].text_content(), Some("OK"));
}

#[test]
fn nested_quoted_elements_from_single_root_keep_text_and_media_order() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "解释引用图文",
            "attachments": [{
                "content_type": "image/png",
                "filename": "current.png",
                "url": "https://example.test/current.png"
            }],
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=REFIDX_current",
                "ref_msg_idx=REFIDX_quoted"
            ]},
            "msg_elements": [
                {
                    "msg_idx": "REFIDX_quoted",
                    "content": "引用第一段",
                    "msg_elements": [
                        {
                            "content": "[图片]引用第二段",
                            "attachments": [{
                                "content_type": "image/png",
                                "filename": "quoted.png",
                                "url": "https://example.test/quoted.png"
                            }]
                        }
                    ]
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "解释引用图文");
    assert!(
        message
            .attachments
            .iter()
            .any(|item| item.filename.as_deref() == Some("current.png"))
    );
    assert_eq!(reply.content.as_deref(), Some("引用第一段\n引用第二段"));
    assert_eq!(reply.input_parts[0].text_content(), Some("引用第一段"));
    assert_eq!(reply.input_parts[1].text_content(), Some("引用第二段"));
    assert_eq!(
        reply.input_parts[2]
            .media()
            .and_then(|media| media.filename.as_deref()),
        Some("quoted.png")
    );
    assert!(!reply.input_parts.iter().any(|part| {
        part.media().and_then(|media| media.filename.as_deref()) == Some("current.png")
    }));
}

#[test]
fn quoted_images_keep_original_order() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "解释这些图",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_quoted"]},
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "content": "[图片][图片][图片] 结构化正文",
                "attachments": [
                    {
                        "content_type": "image/png",
                        "filename": "same.png",
                        "size": 123,
                        "url": "https://example.test/1.png",
                        "fileid": "file-1"
                    },
                    {
                        "content_type": "image/png",
                        "filename": "same.png",
                        "size": 123,
                        "url": "https://example.test/2.png",
                        "fileid": "file-2"
                    },
                    {
                        "content_type": "image/png",
                        "filename": "same.png",
                        "size": 123,
                        "url": "https://example.test/3.png",
                        "fileid": "file-3"
                    }
                ]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(reply.content.as_deref(), Some("结构化正文"));
    let images = reply
        .input_parts
        .iter()
        .filter_map(MessageInputPart::media)
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 3);
    assert_eq!(
        images
            .iter()
            .filter_map(|media| media.file_id.as_deref())
            .collect::<Vec<_>>(),
        vec!["file-1", "file-2", "file-3"]
    );
    assert_eq!(reply.media_summaries.len(), 3);
}

#[test]
fn msg_elements_with_only_attachments_no_text_is_not_empty() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "解释图片",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_quoted"]},
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "attachments": [{
                    "content_type": "image/png",
                    "filename": "quoted.png",
                    "url": "https://example.test/quoted.png"
                }]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(reply.content, None);
    assert_eq!(reply.input_parts.len(), 1);
    assert!(matches!(
        reply.input_parts[0],
        MessageInputPart::Image { .. }
    ));
}

#[test]
fn parses_quoted_audio_asr_as_quoted_user_content() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "current-message",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "这段说了什么",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_voice"]},
            "msg_elements": [{
                "msg_idx": "REFIDX_voice",
                "attachments": [{
                    "content_type": "audio/wav",
                    "filename": "quoted.wav",
                    "asr_refer_text": "引用语音内容"
                }]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert!(matches!(
        &reply.input_parts[1],
        MessageInputPart::Text { text, source: Some(TextSource::Quote) }
            if text == "[语音转文字] 引用语音内容"
    ));
    assert!(
        reply
            .media_summaries
            .iter()
            .any(|summary| { summary.kind == qq_maid_common::input_part::QuotedMediaKind::File })
    );
}
