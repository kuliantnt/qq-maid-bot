use super::*;

#[test]
fn group_quote_recovers_text_when_parallel_contains_only_ref_node() {
    let raw_content = "<@!12345678901234567890123456789012>引用内容查看";
    assert_eq!(raw_content.chars().count(), 42);
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": raw_content,
            "mentions": [{"is_you": true, "member_openid": "12345678901234567890123456789012"}],
            "message_type": 103,
            "message_scene": {"ext": ["msg_idx=REFIDX_current"]},
            "parallel_message": {
                "msg_nodes": [
                    {"msg_idx": "REFIDX_quoted", "content": "测试"}
                ]
            },
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "content": "测试引用内容查看"
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content.chars().count(), 42);
    assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
    assert_eq!(reply.content.as_deref(), Some("测试"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(reply.input_parts[0].text_content(), Some("测试"));
    assert!(
        reply
            .input_parts
            .iter()
            .all(|part| part.fallback_text() != "测试引用内容查看")
    );
}

#[test]
fn group_quote_recovers_text_without_parallel_current_node() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "<@!12345678901234567890123456789012>引用内容查看",
            "mentions": [{"is_you": true, "member_openid": "12345678901234567890123456789012"}],
            "message_type": 103,
            "message_scene": {"ext": ["msg_idx=REFIDX_current"]},
            "parallel_message": {
                "msg_nodes": [
                    {"msg_idx": "REFIDX_quoted", "content": "测试"},
                    {"msg_idx": "REFIDX_unrelated", "content": "无关节点"}
                ]
            },
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "content": "测试引用内容查看"
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(reply.content.as_deref(), Some("测试"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(reply.input_parts[0].text_content(), Some("测试"));
    assert!(
        reply
            .input_parts
            .iter()
            .all(|part| part.fallback_text() != "测试引用内容查看")
    );
}

#[test]
fn group_quote_without_parallel_drops_mixed_root_text_but_keeps_media() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "<@!12345678901234567890123456789012>引用内容查看",
            "message_type": 103,
            "message_scene": {"ext": ["msg_idx=REFIDX_current"]},
            "msg_elements": [{
                "msg_idx": "REFIDX_quoted",
                "content": "测试引用内容查看",
                "attachments": [{
                    "content_type": "image/png",
                    "filename": "quoted.png",
                    "url": "https://example.test/quoted.png"
                }]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(reply.content, None);
    assert!(reply.input_parts.iter().any(|part| {
        matches!(
            part,
            MessageInputPart::Text {
                source: Some(TextSource::QuoteContaminated),
                ..
            }
        )
    }));
    assert!(
        reply
            .input_parts
            .iter()
            .any(|part| matches!(part, MessageInputPart::Image { .. }))
    );
    assert!(
        reply
            .input_parts
            .iter()
            .all(|part| part.fallback_text() != "测试引用内容查看")
    );
}

#[test]
fn explicit_scene_ref_conflict_does_not_trust_first_element_text() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: Some("event-current".to_owned()),
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "引用内容查看",
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=REFIDX_current",
                "ref_msg_idx=REFIDX_scene"
            ]},
            "msg_elements": [{
                "msg_idx": "REFIDX_element",
                "content": "测试"
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_scene"));
    assert_eq!(reply.content, None);
    assert!(reply.input_parts.iter().all(|part| {
        !matches!(
            part,
            MessageInputPart::Text {
                text,
                source: Some(TextSource::Quote),
            } if text == "测试"
        )
    }));
}
