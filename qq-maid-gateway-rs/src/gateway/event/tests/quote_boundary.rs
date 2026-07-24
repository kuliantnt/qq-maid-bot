use super::*;

/// 官方单聊引用结构：顶层 content 为当前正文，msg_elements 为引用正文。
/// 两者各出现一次，不会混合。
#[test]
fn official_c2c_quote_structure_keeps_current_and_quoted_separate() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "author": {"user_openid": "user-1"},
            "content": "这个建议很有帮助，谢谢你！",
            "message_type": 103,
            "message_scene": {
                "ext": [
                    "msg_idx=REFIDX_current",
                    "ref_msg_idx=REFIDX_quoted"
                ]
            },
            "msg_elements": [
                {
                    "msg_idx": "REFIDX_quoted",
                    "content": "每天坚持阅读半小时"
                }
            ]
        }),
    };

    let message = parse_c2c_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    // 当前正文只来自顶层 content，出现一次。
    assert_eq!(message.content, "这个建议很有帮助，谢谢你！");
    assert_eq!(message.input_parts.len(), 1);
    assert_eq!(
        message.input_parts[0].text_content(),
        Some("这个建议很有帮助，谢谢你！")
    );

    // 引用正文只来自 msg_elements，出现一次。
    assert_eq!(reply.content.as_deref(), Some("每天坚持阅读半小时"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(
        reply.input_parts[0].text_content(),
        Some("每天坚持阅读半小时")
    );
}

/// 官方群聊引用结构：msg_elements 元素不携带 msg_idx 时仍可正常解析。
#[test]
fn group_quote_without_element_msg_idx_parses_content() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "继续解释",
            "message_type": 103,
            "message_scene": {
                "ext": [
                    "msg_idx=REFIDX_current",
                    "ref_msg_idx=TMP_quoted"
                ]
            },
            "msg_elements": [
                {
                    "content": "被引用的群聊消息"
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "继续解释");
    // 即使 element 没有 msg_idx，引用正文仍被解析。
    assert_eq!(reply.content.as_deref(), Some("被引用的群聊消息"));
    assert_eq!(reply.ref_msg_idx.as_deref(), Some("TMP_quoted"));
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(
        reply.input_parts[0].text_content(),
        Some("被引用的群聊消息")
    );
}

/// ref_msg_idx 缺失时，引用 payload 仍保留，RefIndex 查询由上层降级。
#[test]
fn missing_ref_msg_idx_keeps_quoted_payload() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "继续",
            "message_type": 103,
            "msg_elements": [
                {
                    "content": "引用内容"
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    assert_eq!(message.content, "继续");
    // ref_msg_idx 缺失时引用 payload 仍保留。
    assert_eq!(reply.content.as_deref(), Some("引用内容"));
    assert_eq!(reply.ref_msg_idx, None);
    assert_eq!(reply.input_parts.len(), 1);
    assert_eq!(reply.input_parts[0].text_content(), Some("引用内容"));
}

/// 嵌套图文引用：验证递归顺序和媒体归属。
#[test]
fn nested_text_image_quote_keeps_order_and_does_not_mix_with_current_attachments() {
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
                    "content": "图前文字",
                    "msg_elements": [
                        {
                            "content": "[图片]图中图片",
                            "attachments": [{
                                "content_type": "image/png",
                                "filename": "quoted.png",
                                "url": "https://example.test/quoted.png"
                            }]
                        },
                        {"content": "图后文字"}
                    ]
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    // 当前正文和附件不进入引用。
    assert_eq!(message.content, "解释引用图文");
    assert!(
        message
            .attachments
            .iter()
            .any(|item| item.filename.as_deref() == Some("current.png"))
    );

    // 引用内容按递归顺序：图前文字 → 图中图片 → 图后文字。
    assert_eq!(
        reply.content.as_deref(),
        Some("图前文字\n图中图片\n图后文字")
    );
    assert_eq!(reply.input_parts[0].text_content(), Some("图前文字"));
    assert_eq!(reply.input_parts[1].text_content(), Some("图中图片"));
    assert_eq!(
        reply.input_parts[2]
            .media()
            .and_then(|media| media.filename.as_deref()),
        Some("quoted.png")
    );
    assert_eq!(reply.input_parts[3].text_content(), Some("图后文字"));

    // 引用附件不会进入当前消息。
    assert!(!reply.input_parts.iter().any(|part| {
        part.media().and_then(|media| media.filename.as_deref()) == Some("current.png")
    }));
}

/// 引用消息中无文字只有附件时，仍保留媒体摘要。
#[test]
fn quote_with_only_attachments_keeps_media() {
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
    assert_eq!(reply.media_summaries.len(), 1);
}

/// 多元素引用消息：无 msg_idx 筛选时按原始顺序全部纳入。
#[test]
fn multiple_elements_all_parsed_as_quote_content() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
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
            "msg_elements": [
                {"content": "被引用原文"},
                {"content": "第二条引用文字"},
                {
                    "attachments": [{
                        "content_type": "image/png",
                        "filename": "quoted.png",
                        "url": "https://example.test/quoted.png"
                    }]
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(message.content, "查看这条");
    assert_eq!(reply.content.as_deref(), Some("被引用原文\n第二条引用文字"));
    assert_eq!(reply.input_parts.len(), 3);
    assert_eq!(reply.input_parts[0].text_content(), Some("被引用原文"));
    assert_eq!(reply.input_parts[1].text_content(), Some("第二条引用文字"));
    assert!(matches!(
        reply.input_parts[2],
        MessageInputPart::Image { .. }
    ));
    assert_eq!(reply.media_summaries.len(), 1);
}

/// 非引用消息（message_type != 103）不把 msg_elements 当作引用上下文。
#[test]
fn non_quote_message_ignores_msg_elements() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "普通消息",
            "message_type": 0,
            "msg_elements": [
                {"content": "这段不应成为引用"}
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert_eq!(message.content, "普通消息");
    assert!(message.reply.is_none());
}

/// msg_elements 引用文字与当前正文混合时，事件解析层保留原始 payload。
///
/// 污染检测已移至群聊/C2C 处理层（正文归一化后、RefIndex enrich 前），
/// 事件解析层不再执行剥离。
#[test]
fn contaminated_element_content_preserved_at_event_parse_level() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "引用内容查看",
            "message_type": 103,
            "message_scene": {"ext": ["msg_idx=REFIDX_current"]},
            "msg_elements": [
                {
                    "content": "测试引用内容查看",
                    "attachments": [{
                        "content_type": "image/png",
                        "filename": "quoted.png",
                        "url": "https://example.test/quoted.png"
                    }]
                }
            ]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.as_ref().unwrap();

    // 当前正文只出现一次。
    assert_eq!(message.content, "引用内容查看");

    // 事件解析层不再剥离污染文字；payload 保留原始引用内容。
    // 污染检测在群聊处理层执行（正文归一化后）。
    assert!(reply.content.is_some());
    assert!(
        reply
            .input_parts
            .iter()
            .any(|part| matches!(part, MessageInputPart::Text { .. }))
    );

    // 引用图片保留。
    assert!(
        reply
            .input_parts
            .iter()
            .any(|part| matches!(part, MessageInputPart::Image { .. }))
    );
    assert!(!reply.media_summaries.is_empty());
}
