use super::*;
use qq_maid_common::input_part::MessageInputPart;
use std::sync::Arc;

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

#[test]
fn full_group_mention_quote_keeps_current_and_quoted_body_separate() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-full-group-quote",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "[@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=bot-openid) 原始数据",
            "mentions": [
                {"is_you": true, "member_openid": "bot-openid", "username": "汐雨"}
            ],
            "message_type": 103,
            "msg_elements": [{"content": " 测试"}]
        }),
    };

    let mut message = parse_group_message(&envelope).unwrap().unwrap();
    crate::gateway::group_filter::normalize_current_bot_mentions(
        &mut message,
        &Arc::new(crate::gateway::bot_identity::BotIdentity::new(
            "app-id",
            &[],
        )),
    );

    assert_eq!(message.content, "原始数据");
    assert_eq!(message.input_parts[0].text_content(), Some("原始数据"));
    let reply = message.reply.as_ref().unwrap();
    assert_eq!(reply.content.as_deref(), Some("测试"));
    assert_eq!(reply.input_parts[0].text_content(), Some("测试"));

    let inbound = crate::respond::normalized_group_inbound(&message, &[]);
    assert_eq!(inbound.text, "原始数据");
    assert_eq!(inbound.input_parts[0].text_content(), Some("原始数据"));
    let quoted = inbound.quoted.as_ref().unwrap();
    assert_eq!(quoted.text_summary.as_deref(), Some("测试"));
    assert!(!inbound.text.contains("mqqapi://markdown/mention"));
    assert!(
        !quoted
            .input_parts
            .iter()
            .filter_map(MessageInputPart::text_content)
            .any(|text| text.contains("mqqapi://markdown/mention"))
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

/// 嵌套结构化图文引用：只保留直接层正文，不递归恢复内层正文或媒体。
#[test]
fn nested_text_image_quote_stops_before_historical_media() {
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

    // 直接层正文保留，子元素不递归解析；内层结构化媒体不会被恢复。
    assert_eq!(reply.content.as_deref(), Some("图前文字"));
    assert_eq!(reply.input_parts[0].text_content(), Some("图前文字"));
    assert_eq!(reply.input_parts.len(), 1);
    assert!(reply.media_summaries.is_empty());
    assert!(
        !reply
            .input_parts
            .iter()
            .map(MessageInputPart::fallback_text)
            .any(|text| text.contains("example.test") || text.contains("quoted.png"))
    );

    // 引用附件不会进入当前消息。
    assert!(!reply.input_parts.iter().any(|part| {
        part.media().and_then(|media| media.filename.as_deref()) == Some("current.png")
    }));
}

/// QQ 的 `[图片]` 占位与 attachments 一一对应；直接层图文必须按占位顺序进入模型。
#[test]
fn direct_text_and_multiple_images_keep_placeholder_order() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "按顺序解释",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=TMP_quoted"]},
            "msg_elements": [{
                "content": "图前[图片]图中[图片]图后",
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
                    }
                ]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let parts = message.reply.unwrap().input_parts;

    assert_eq!(parts.len(), 5);
    assert_eq!(parts[0].text_content(), Some("图前"));
    assert_eq!(
        parts[1].media().and_then(|media| media.file_id.as_deref()),
        Some("file-1")
    );
    assert_eq!(parts[2].text_content(), Some("图中"));
    assert_eq!(
        parts[3].media().and_then(|media| media.file_id.as_deref()),
        Some("file-2")
    );
    assert_eq!(parts[4].text_content(), Some("图后"));
}

/// 单张结构化图片同样启用占位顺序解析，避免收紧条件后退化为尾部附件。
#[test]
fn direct_text_and_single_image_keeps_placeholder_order() {
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
            "message_scene": {"ext": ["ref_msg_idx=TMP_quoted"]},
            "msg_elements": [{
                "content": "图前[图片]图后",
                "attachments": [{
                    "content_type": "image/png",
                    "filename": "quoted.png",
                    "url": "https://example.test/quoted.png",
                    "fileid": "file-quoted"
                }]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let parts = message.reply.unwrap().input_parts;

    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].text_content(), Some("图前"));
    assert_eq!(
        parts[1].media().and_then(|media| media.file_id.as_deref()),
        Some("file-quoted")
    );
    assert_eq!(parts[2].text_content(), Some("图后"));
}

/// 文件或音频附件不代表正文中的 `[图片]` 有对应结构化图片，必须按普通文本保留。
#[test]
fn non_image_attachments_do_not_enable_image_placeholder_parsing() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "检查附件",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=TMP_quoted"]},
            "msg_elements": [{
                "content": "原文[图片]结尾",
                "attachments": [
                    {
                        "content_type": "application/pdf",
                        "filename": "document.pdf",
                        "url": "https://example.test/document.pdf"
                    },
                    {
                        "content_type": "audio/ogg",
                        "filename": "voice.ogg",
                        "url": "https://example.test/voice.ogg"
                    }
                ]
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(reply.content.as_deref(), Some("原文[图片]结尾"));
    assert_eq!(reply.input_parts[0].text_content(), Some("原文[图片]结尾"));
    assert_eq!(
        reply
            .input_parts
            .iter()
            .filter(|part| matches!(part, MessageInputPart::File { .. }))
            .count(),
        2
    );
    assert!(
        !reply
            .input_parts
            .iter()
            .any(|part| matches!(part, MessageInputPart::Image { .. }))
    );
}

#[test]
fn direct_quote_media_keeps_existing_normalization_count_limit() {
    let attachments = (0..34)
        .map(|index| {
            json!({
                "content_type": "image/png",
                "filename": format!("{index}.png"),
                "url": format!("https://example.test/{index}.png")
            })
        })
        .collect::<Vec<_>>();
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-current",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "看看这些图",
            "message_type": 103,
            "message_scene": {"ext": ["ref_msg_idx=REFIDX_quoted"]},
            "msg_elements": [{"attachments": attachments}]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();
    let reply = message.reply.unwrap();

    assert_eq!(
        reply
            .input_parts
            .iter()
            .filter(|part| part.is_non_text())
            .count(),
        32
    );
    assert_eq!(reply.media_summaries.len(), 32);
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
