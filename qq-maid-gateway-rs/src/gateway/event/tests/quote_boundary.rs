use super::*;

#[test]
fn group_quote_selects_matching_element_when_it_is_not_first() {
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
                "ref_msg_idx=REFIDX_quoted"
            ]},
            "msg_elements": [
                {"msg_idx": "REFIDX_unrelated", "content": "无关文字"},
                {"msg_idx": "REFIDX_quoted", "content": "测试"}
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "引用内容查看");
    assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
    assert_eq!(reply.content.as_deref(), Some("测试"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(reply.input_parts[0].text_content(), Some("测试"));
    assert!(
        reply
            .input_parts
            .iter()
            .all(|part| !part.fallback_text().contains("无关文字"))
    );
}

#[test]
fn unmatched_quote_element_does_not_send_unconfirmed_text() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "引用内容查看",
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=REFIDX_current",
                "ref_msg_idx=REFIDX_missing"
            ]},
            "parallel_message": {"msg_nodes": [{"content": "无索引兜底"}]},
            "msg_elements": [{"msg_idx": "REFIDX_other", "content": "无法确认归属"}]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "引用内容查看");
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_missing"));
    assert_eq!(reply.content, None);
    assert!(reply.input_parts.is_empty());
    assert!(reply.media_summaries.is_empty());
}

#[test]
fn indexed_parallel_message_is_low_priority_text_fallback() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "继续",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_quoted"]},
            "parallel_message": {"msg_nodes": [
                {"msg_idx": "REFIDX_other", "content": "无关文字"},
                {"msg_idx": "REFIDX_quoted", "content": "索引兜底"}
            ]},
            "msg_elements": [{"msg_idx": "REFIDX_quoted"}]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(reply.content.as_deref(), Some("索引兜底"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(reply.input_parts[0].text_content(), Some("索引兜底"));
}

#[test]
fn matching_quote_keeps_nested_text_and_media_order_only() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "解释图文",
            "message_type": 103,
            "message_scene": {"ext": [
                "msg_idx=REFIDX_current",
                "ref_msg_idx=REFIDX_quoted"
            ]},
            "msg_elements": [
                {
                    "msg_idx": "REFIDX_unrelated",
                    "content": "无关顶层文字",
                    "attachments": [{
                        "content_type": "image/png",
                        "filename": "unrelated.png",
                        "url": "https://example.test/unrelated.png"
                    }]
                },
                {
                    "msg_idx": "REFIDX_quoted",
                    "content": "图前",
                    "msg_elements": [
                        {
                            "content": "[图片]图中",
                            "attachments": [{
                                "content_type": "image/png",
                                "filename": "quoted.png",
                                "url": "https://example.test/quoted.png"
                            }]
                        },
                        {"content": "图后"}
                    ]
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(reply.content.as_deref(), Some("图前\n图中\n图后"));
    assert_eq!(reply.input_parts[0].text_content(), Some("图前"));
    assert_eq!(reply.input_parts[1].text_content(), Some("图中"));
    assert_eq!(
        reply.input_parts[2]
            .media()
            .and_then(|media| media.filename.as_deref()),
        Some("quoted.png")
    );
    assert_eq!(reply.input_parts[3].text_content(), Some("图后"));
    assert!(!reply.input_parts.iter().any(|part| {
        part.media().and_then(|media| media.filename.as_deref()) == Some("unrelated.png")
    }));
}
