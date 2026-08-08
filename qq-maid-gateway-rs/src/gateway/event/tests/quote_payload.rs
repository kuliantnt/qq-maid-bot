use super::*;

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
fn nested_quoted_elements_from_single_root_only_keep_direct_layer() {
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
    assert_eq!(reply.content.as_deref(), Some("引用第一段"));
    assert_eq!(reply.input_parts[0].text_content(), Some("引用第一段"));
    assert_eq!(reply.input_parts.len(), 1);
    assert!(reply.media_summaries.is_empty());
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
                        "filename": "first.png",
                        "size": 123,
                        "url": "https://example.test/1.png",
                        "fileid": "file-1"
                    },
                    {
                        "content_type": "image/png",
                        "filename": "second.png",
                        "size": 123,
                        "url": "https://example.test/2.png",
                        "fileid": "file-2"
                    },
                    {
                        "content_type": "image/png",
                        "filename": "third.png",
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
    assert_eq!(reply.input_parts.len(), 4);
    assert_eq!(
        reply.input_parts[0]
            .media()
            .and_then(|media| media.file_id.as_deref()),
        Some("file-1")
    );
    assert_eq!(
        reply.input_parts[1]
            .media()
            .and_then(|media| media.file_id.as_deref()),
        Some("file-2")
    );
    assert_eq!(
        reply.input_parts[2]
            .media()
            .and_then(|media| media.file_id.as_deref()),
        Some("file-3")
    );
    assert_eq!(reply.input_parts[3].text_content(), Some("结构化正文"));
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
