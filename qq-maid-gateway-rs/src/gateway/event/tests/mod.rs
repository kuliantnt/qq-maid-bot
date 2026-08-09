use super::*;
use serde_json::json;
use std::sync::Arc;

mod media;
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
                {"id": "owner-id", "member_role": "owner"},
                {"id": "appid", "is_you": true, "bot": true, "member_role": "admin"},
                {"user_openid": "user-openid", "member_role": "member"},
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
                is_current_bot: false,
                member_role: Some(GroupMemberRole::Owner),
                target_id: Some("owner-id".to_owned())
            },
            GroupMention {
                is_current_bot: true,
                member_role: Some(GroupMemberRole::Admin),
                target_id: Some("appid".to_owned())
            },
            GroupMention {
                is_current_bot: false,
                member_role: Some(GroupMemberRole::Member),
                target_id: Some("user-openid".to_owned())
            },
            GroupMention {
                is_current_bot: false,
                member_role: Some(GroupMemberRole::Unknown),
                target_id: Some("member-openid".to_owned())
            }
        ]
    );
}

#[test]
fn normalizes_full_group_bot_mention_without_touching_plain_members() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-full-group-mention",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=member-1) 帮我问一下 [@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=bot-openid) 原始数据",
            "mentions": [
                {"is_you": false, "member_openid": "member-1", "username": "张三"},
                {"is_you": true, "bot": true, "member_openid": "bot-openid", "username": "汐雨"}
            ]
        }),
    };

    let mut message = parse_group_message(&envelope).unwrap().unwrap();
    assert!(message.content.contains("mqqapi://markdown/mention"));
    crate::gateway::group_filter::normalize_current_bot_mentions(
        &mut message,
        // 模拟 READY 未学习 member_openid：QQ 的结构化当前机器人标记仍是有效证据。
        &Arc::new(crate::gateway::bot_identity::BotIdentity::new(
            "app-id",
            &[],
        )),
    );

    assert_eq!(
        message.content,
        "[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=member-1) 帮我问一下 原始数据"
    );
    assert_eq!(
        message.input_parts[0].text_content(),
        Some("[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=member-1) 帮我问一下 原始数据")
    );
    assert!(
        !message
            .content
            .contains("mqqapi://markdown/mention?at_type=1&at_tinyid=bot-openid")
    );
    assert!(!message.mentions[0].is_current_bot);
    assert!(message.mentions[1].is_current_bot);

    let inbound = crate::respond::normalized_group_inbound(&message, &[]);
    assert_eq!(
        inbound.text,
        "[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=member-1) 帮我问一下 原始数据"
    );
    assert_eq!(
        inbound.input_parts[0].text_content(),
        Some(inbound.text.as_str())
    );
    assert_eq!(inbound.text.matches("原始数据").count(), 1);
    assert!(!inbound.text.contains("at_tinyid=bot-openid"));
}

#[test]
fn does_not_accept_is_you_for_a_mention_explicitly_marked_as_member() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-member-mark",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "hello",
            "mentions": [{
                "is_you": true,
                "bot": false,
                "member_openid": "member-2"
            }]
        }),
    };

    let message = parse_group_message(&envelope).unwrap().unwrap();

    assert!(!message.mentions[0].is_current_bot);
}

#[test]
fn does_not_treat_everyone_mention_as_current_bot() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-everyone-mention",
            "group_openid": "group-1",
            "author": {"member_openid": "member-1"},
            "content": "通知大家",
            "mentions": [{
                "username": "全体成员",
                "scope": "all",
                "is_you": true,
                "member_openid": "bot-openid"
            }]
        }),
    };

    let mut message = parse_group_message(&envelope).unwrap().unwrap();
    let identity = Arc::new(crate::gateway::bot_identity::BotIdentity::new(
        "app-id",
        &["bot-openid".to_owned()],
    ));
    crate::gateway::group_filter::normalize_current_bot_mentions(&mut message, &identity);

    assert_eq!(message.mentions.len(), 1);
    assert!(!message.mentions[0].is_current_bot);
    assert_eq!(message.mentions[0].target_id, None);
    assert!(!crate::gateway::group_filter::mentions_current_bot(
        &message
    ));
}

#[test]
fn group_at_message_keeps_qq_cleaned_body() {
    let envelope = GatewayEnvelope {
        op: 0,
        s: None,
        t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
        id: None,
        d: json!({
            "id": "msg-group-at-clean",
            "group_openid": "group-1",
            "content": "  原始数据",
            "mentions": []
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

mod quote_payload;
