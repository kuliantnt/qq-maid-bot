use super::*;
use crate::event::{
    Attachment, C2cMessage, GroupEventType, GroupMemberRole, GroupMention, GroupMessage,
    MessageReply,
};
use crate::gateway::event::strip_contaminated_quote_from_context;
use qq_maid_common::input_part::MessageMedia;
use qq_maid_common::input_part::QuotedMessageContext;
use qq_maid_common::input_part::TextSource;
use qq_maid_core::service::{
    CoreConversation, CoreGroupMemberRole, CoreHealthSnapshot, CoreInboundClassification,
    CoreRequest, CoreRespondOutput, Platform, UpstreamStatusSnapshot,
};

#[derive(Default)]
struct NoopCore;

#[async_trait::async_trait]
impl CoreService for NoopCore {
    async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        unreachable!("respond is not used in mapping tests")
    }

    async fn classify_inbound(
        &self,
        _request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        unreachable!("classify is not used in mapping tests")
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "test".to_owned(),
            model: "test".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

fn mapping_client() -> RespondClient {
    RespondClient::new(Arc::new(NoopCore))
}

fn c2c_message(content: &str) -> C2cMessage {
    C2cMessage {
        message_id: "m1".to_owned(),
        current_msg_idx: None,
        event_id: Some("e1".to_owned()),
        source_message_ids: vec!["m1".to_owned()],
        source_event_ids: vec!["e1".to_owned()],
        user_openid: "u1".to_owned(),
        content: content.to_owned(),
        reply: None,
        timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        first_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        last_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        input_parts: if content.trim().is_empty() {
            Vec::new()
        } else {
            vec![qq_maid_common::input_part::MessageInputPart::text(content)]
        },
        attachments: Vec::new(),
    }
}

fn group_message(content: &str, member: Option<&str>) -> GroupMessage {
    GroupMessage {
        message_id: "gm1".to_owned(),
        current_msg_idx: None,
        group_openid: "g1".to_owned(),
        member_openid: member.map(str::to_owned),
        member_role: None,
        content: content.to_owned(),
        mentions: Vec::new(),
        reply: None,
        timestamp: None,
        input_parts: if content.trim().is_empty() {
            Vec::new()
        } else {
            vec![qq_maid_common::input_part::MessageInputPart::text(content)]
        },
        attachments: Vec::new(),
        event_type: GroupEventType::GroupAtMessage,
        author_is_bot: false,
        author_is_self: false,
    }
}

#[test]
fn c2c_message_maps_to_private_core_request() {
    let request =
        mapping_client().core_request_from_c2c_message(&c2c_message("/todo"), "/todo".to_owned());

    assert_eq!(request.text, "/todo");
    assert_eq!(request.platform, Platform::QqOfficial);
    assert_eq!(request.actor.user_id.as_deref(), Some("u1"));
    assert_eq!(
        request.conversation,
        CoreConversation::Private {
            peer_id: "u1".to_owned()
        }
    );
}

#[test]
fn group_message_maps_to_group_scope_without_member_split() {
    let client = mapping_client();
    let request = client.core_request_from_group_message(
        &group_message("/rss", Some("member1")),
        "/rss".to_owned(),
    );

    assert_eq!(request.actor.user_id.as_deref(), Some("member1"));
    assert_eq!(
        request.conversation,
        CoreConversation::Group {
            group_id: "g1".to_owned()
        }
    );

    let missing_member =
        client.core_request_from_group_message(&group_message("/rss", None), "/rss".to_owned());
    assert_eq!(missing_member.actor.user_id, None);
    assert_eq!(
        missing_member.scope_key(),
        "platform:qq_official:account:-:group:g1"
    );
}

#[test]
fn qq_group_mapping_marks_structured_at_and_active_keyword_as_addressed() {
    let keywords = vec!["召唤词".to_owned()];

    let structured = normalized_group_inbound_with_prefix(
        &group_message("/unknown", Some("member1")),
        &keywords,
        CommandPrefix::default(),
    );
    let structured_request =
        platform::to_core_request(&structured, structured.text.clone()).unwrap();
    assert!(structured_request.addressed_to_bot);

    let mut keyword_message = group_message("召唤词 /unknown", Some("member1"));
    keyword_message.event_type = GroupEventType::GroupMessage;
    let keyword =
        normalized_group_inbound_with_prefix(&keyword_message, &keywords, CommandPrefix::default());
    let keyword_request = platform::to_core_request(&keyword, keyword.text.clone()).unwrap();
    assert_eq!(keyword_request.text, "/unknown");
    assert!(keyword_request.addressed_to_bot);

    let mut direct_message = group_message("/unknown", Some("member1"));
    direct_message.event_type = GroupEventType::GroupMessage;
    let direct =
        normalized_group_inbound_with_prefix(&direct_message, &keywords, CommandPrefix::default());
    let direct_request = platform::to_core_request(&direct, direct.text.clone()).unwrap();
    assert!(!direct_request.addressed_to_bot);
}

#[test]
fn respond_client_injects_qq_account_into_scope_key() {
    let client = RespondClient::new(Arc::new(NoopCore)).with_qq_official_account_id("app-123");
    let message = c2c_message("你好");

    assert_eq!(
        client.scope_key_from_c2c_message(&message),
        "platform:qq_official:account:app-123:private:u1"
    );
    let request = client.core_request_from_c2c_message(&message, "你好".to_owned());
    assert_eq!(request.account_id.as_deref(), Some("app-123"));
    assert_eq!(request.actor.user_id.as_deref(), Some("u1"));

    let group = group_message("/rss", Some("member1"));
    assert_eq!(
        client.scope_key_from_group_message(&group),
        "platform:qq_official:account:app-123:group:g1"
    );
    let request = client.core_request_from_group_message(&group, "/rss".to_owned());
    assert_eq!(request.account_id.as_deref(), Some("app-123"));
    assert_eq!(request.actor.user_id.as_deref(), Some("member1"));
}

#[test]
fn same_qq_actor_keeps_private_and_group_conversation_scopes_separate() {
    let client = RespondClient::new(Arc::new(NoopCore)).with_qq_official_account_id("app-123");
    let private = c2c_message("继续");
    let group = group_message("继续", Some("u1"));

    let private_request = client.core_request_from_c2c_message(&private, "继续".to_owned());
    let group_request = client.core_request_from_group_message(&group, "继续".to_owned());

    assert_eq!(private_request.actor.user_id.as_deref(), Some("u1"));
    assert_eq!(group_request.actor.user_id.as_deref(), Some("u1"));
    assert_eq!(
        private_request.scope_key(),
        "platform:qq_official:account:app-123:private:u1"
    );
    assert_eq!(
        group_request.scope_key(),
        "platform:qq_official:account:app-123:group:g1"
    );
    assert_ne!(private_request.scope_key(), group_request.scope_key());
}

#[test]
fn prepare_inbound_injects_account_before_core_scope_mapping() {
    let client = RespondClient::new(Arc::new(NoopCore)).with_qq_official_account_id("app-123");
    let c2c = client.prepare_inbound(platform::qq_official::inbound_from_c2c(&c2c_message(
        "你好",
    )));
    let group = client.prepare_inbound(platform::qq_official::inbound_from_group(&group_message(
        "/rss",
        Some("member1"),
    )));

    assert_eq!(c2c.account_id.as_deref(), Some("app-123"));
    assert_eq!(
        platform::core_scope_key(&c2c).unwrap(),
        "platform:qq_official:account:app-123:private:u1"
    );
    assert_eq!(group.account_id.as_deref(), Some("app-123"));
    assert_eq!(
        platform::core_scope_key(&group).unwrap(),
        "platform:qq_official:account:app-123:group:g1"
    );
}

#[test]
fn group_member_role_maps_to_core_actor() {
    let mut message = group_message("/rss add https://example.test/feed.xml", Some("member1"));
    message.member_role = Some(GroupMemberRole::Admin);

    let request =
        mapping_client().core_request_from_group_message(&message, message.content.clone());

    assert_eq!(
        request.actor.group_member_role,
        Some(CoreGroupMemberRole::Admin)
    );
    let respond: qq_maid_core::runtime::respond::RespondRequest = request.into();
    assert_eq!(respond.group_member_role.as_deref(), Some("admin"));
}

#[test]
fn group_command_content_strips_platform_prefixes() {
    let keywords = vec![
        "召唤词".to_owned(),
        "小女仆".to_owned(),
        "脸脸家的小女仆".to_owned(),
    ];

    for input in [
        "@脸脸家的小女仆 /help",
        "[CQ:at,qq=123] /help",
        "<@member-1> /help",
        "@脸脸家的小女仆 ／help",
        "[CQ:at,qq=123] ／help",
        "召唤词 /rss add https://hnrss.org/newcomments",
        "召唤词：/rss",
        "召唤词/rss recent",
        "小女仆/rss recent",
        "召唤词：／rss",
        "召唤词： /rss \n",
        "召唤词： ／rss \n",
        "@脸脸家的小女仆 ／memory profile 在这个群叫我棒冰",
        "[CQ:at,qq=123] /记忆 group list",
    ] {
        let content =
            build_group_respond_content(&group_message(input, Some("member1")), &keywords);

        assert!(
            content.starts_with('/'),
            "input should normalize to slash command: {input} -> {content}"
        );
        assert_eq!(
            content,
            content.trim(),
            "normalized command should be trimmed"
        );
    }
}

#[test]
fn group_content_normalization_uses_configured_prefix_only() {
    let prefix = CommandPrefix::parse("#").unwrap();
    let keywords = vec!["机器人".to_owned()];

    let render = |text| {
        build_group_respond_content_with_prefix(
            &group_message(text, Some("member1")),
            &keywords,
            prefix,
        )
    };

    assert_eq!(render("#help"), "#help");
    assert_eq!(render("/help"), "/help");
    assert_eq!(render("你好 #help"), "你好 #help");
    assert_eq!(render("##help"), "##help");
}

#[test]
fn group_address_prefixes_expose_pending_reply_body() {
    let keywords = vec!["召唤词".to_owned(), "脸脸家的小女仆".to_owned()];

    for (input, expected) in [
        ("@脸脸家的小女仆 确认", "确认"),
        ("<@bot-id> 确认", "确认"),
        ("[CQ:at,qq=123] 确认", "确认"),
        ("召唤词：确认", "确认"),
        ("@脸脸家的小女仆 取消", "取消"),
        ("@脸脸家的小女仆 个人", "个人"),
        ("@脸脸家的小女仆 画像", "画像"),
        ("@脸脸家的小女仆 群组", "群组"),
    ] {
        let content =
            build_group_respond_content(&group_message(input, Some("member1")), &keywords);

        assert_eq!(content, expected, "input={input}");
    }
}

#[test]
fn structured_bot_suffix_mentions_expose_pending_reply_body() {
    for body in ["个人", "画像", "确认", "取消", "群组"] {
        for suffix in ["@机器人", "<@bot-id>", "[CQ:at,qq=bot-id]"] {
            let input = format!("{body}{suffix}");
            let mut message = group_message(&input, Some("member1"));
            message.event_type = GroupEventType::GroupMessage;
            message.mentions = vec![GroupMention {
                is_current_bot: true,
                member_role: None,
                target_id: suffix.contains("bot-id").then(|| "bot-id".to_owned()),
            }];

            let content = build_group_respond_content(&message, &["机器人".to_owned()]);

            assert_eq!(content, body, "input={input}");
        }
    }
}

#[test]
fn group_mention_memory_command_and_fullwidth_slash_remain_compatible() {
    let keywords = vec!["召唤词".to_owned(), "脸脸家的小女仆".to_owned()];

    for (input, expected) in [
        ("@脸脸家的小女仆 /记忆 群 delete 1", "/记忆 群 delete 1"),
        ("<@bot-id> ／记忆 群 delete 1", "/记忆 群 delete 1"),
        ("召唤词：／记忆 群 delete 1", "/记忆 群 delete 1"),
    ] {
        let content =
            build_group_respond_content(&group_message(input, Some("member1")), &keywords);

        assert_eq!(content, expected, "input={input}");
    }
}

#[test]
fn group_active_keyword_prefix_with_chinese_text_does_not_panic() {
    let keywords = vec!["小女仆".to_owned()];
    let content = build_group_respond_content(
        &group_message("小女仆 at你咋没响应啊", Some("member1")),
        &keywords,
    );

    assert_eq!(content, "at你咋没响应啊");
}

#[test]
fn group_non_command_content_strips_trigger_prefix() {
    let keywords = vec!["召唤词".to_owned()];
    let content =
        build_group_respond_content(&group_message("召唤词 你在吗", Some("member1")), &keywords);

    assert_eq!(content, "你在吗");
}

#[test]
fn group_active_keyword_requires_address_boundary() {
    let keywords = vec!["克拉拉".to_owned(), "小女仆".to_owned()];

    for (input, expected) in [
        ("克拉拉：确认", "确认"),
        ("克拉拉 确认", "确认"),
        ("克拉拉汀是什么药", "克拉拉汀是什么药"),
        ("小女仆装好看吗", "小女仆装好看吗"),
    ] {
        let content =
            build_group_respond_content(&group_message(input, Some("member1")), &keywords);

        assert_eq!(content, expected, "input={input}");
    }
}

fn media_message(content: &str, mention: GroupMention) -> GroupMessage {
    let mut message = group_message(content, Some("member1"));
    message.event_type = GroupEventType::GroupMessage;
    message.mentions = vec![mention];
    let media = MessageMedia {
        mime_type: Some("image/png".to_owned()),
        filename: Some("confirm.png".to_owned()),
        url: Some("https://example.test/confirm.png".to_owned()),
        platform: Some("qq_official".to_owned()),
        ..MessageMedia::default()
    };
    message.input_parts = vec![
        MessageInputPart::text(content),
        MessageInputPart::image(media),
    ];
    message
}

#[test]
fn structured_bot_display_mention_with_image_updates_text_part_in_place() {
    let mut message = media_message(
        "@机器人 确认",
        GroupMention {
            is_current_bot: true,
            member_role: None,
            target_id: None,
        },
    );
    message.reply = Some(MessageReply {
        message_id: "quoted-1".to_owned(),
        ref_msg_idx: None,
        content: Some("引用内容".to_owned()),
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    let inbound = normalized_group_inbound(&message, &["机器人".to_owned()]);
    let rendered = build_group_respond_content(&message, &["机器人".to_owned()]);

    assert_eq!(inbound.text, "确认");
    assert_eq!(inbound.input_parts.len(), 2);
    assert_eq!(inbound.input_parts[0].text_content(), Some("确认"));
    assert!(matches!(
        &inbound.input_parts[1],
        MessageInputPart::Image { media }
            if media.filename.as_deref() == Some("confirm.png")
                && media.url.as_deref() == Some("https://example.test/confirm.png")
    ));
    assert!(rendered.starts_with("确认\n[图片"));
    assert!(!rendered.contains("@机器人"));
    assert!(!rendered.contains("引用内容"));
    assert_eq!(rendered.matches("[图片").count(), 1);
    assert_eq!(
        inbound
            .quoted
            .as_ref()
            .and_then(|quoted| quoted.text_summary.as_deref()),
        Some("引用内容")
    );
}

#[test]
fn encoded_bot_mentions_with_media_preserve_part_order_without_duplicates() {
    for input in ["[CQ:at,qq=bot-id] 确认", "<@bot-id> 确认"] {
        let message = media_message(
            input,
            GroupMention {
                is_current_bot: true,
                member_role: None,
                target_id: Some("bot-id".to_owned()),
            },
        );

        let inbound = normalized_group_inbound(&message, &[]);
        let rendered = build_group_respond_content(&message, &[]);

        assert_eq!(inbound.input_parts.len(), 2, "input={input}");
        assert_eq!(
            inbound.input_parts[0].text_content(),
            Some("确认"),
            "input={input}"
        );
        assert!(matches!(
            inbound.input_parts[1],
            MessageInputPart::Image { .. }
        ));
        assert_eq!(
            inbound
                .input_parts
                .iter()
                .filter(|part| part.is_non_text())
                .count(),
            1,
            "input={input}"
        );
        assert_eq!(rendered.matches("[图片").count(), 1, "input={input}");
        assert!(!rendered.contains("bot-id"), "input={input}");
    }
}

#[test]
fn other_member_mentions_are_not_removed() {
    for input in [
        "@其他成员 确认",
        "[CQ:at,qq=member-2] 确认",
        "<@member-2> 确认",
    ] {
        let message = media_message(
            input,
            GroupMention {
                is_current_bot: false,
                member_role: None,
                target_id: Some("member-2".to_owned()),
            },
        );

        let inbound = normalized_group_inbound(&message, &["小女仆".to_owned()]);

        assert_eq!(inbound.text, input);
        assert_eq!(inbound.input_parts[0].text_content(), Some(input));
        assert!(matches!(
            inbound.input_parts[1],
            MessageInputPart::Image { .. }
        ));
    }
}

#[test]
fn unstructured_other_display_mention_is_not_treated_as_bot_address() {
    let mut message = media_message(
        "@其他成员 小女仆帮我看图",
        GroupMention {
            is_current_bot: false,
            member_role: None,
            target_id: None,
        },
    );
    message.mentions.clear();

    let inbound = normalized_group_inbound(&message, &["小女仆".to_owned()]);

    assert_eq!(inbound.text, "@其他成员 小女仆帮我看图");
    assert_eq!(
        inbound.input_parts[0].text_content(),
        Some("@其他成员 小女仆帮我看图")
    );
}

#[test]
fn leading_other_structured_mention_is_preserved_when_self_mention_follows() {
    let mut message = media_message(
        "<@member-2> <@bot-id> 确认",
        GroupMention {
            is_current_bot: false,
            member_role: None,
            target_id: Some("member-2".to_owned()),
        },
    );
    message.mentions.push(GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: Some("bot-id".to_owned()),
    });

    let inbound = normalized_group_inbound(&message, &[]);

    assert_eq!(inbound.text, "<@member-2> <@bot-id> 确认");
    assert_eq!(
        inbound.input_parts[0].text_content(),
        Some("<@member-2> <@bot-id> 确认")
    );
}

#[test]
fn mention_only_media_message_keeps_media_when_normalized_body_is_empty() {
    let message = media_message(
        "<@bot-id>",
        GroupMention {
            is_current_bot: true,
            member_role: None,
            target_id: Some("bot-id".to_owned()),
        },
    );

    let inbound = normalized_group_inbound(&message, &[]);

    assert!(inbound.text.is_empty());
    assert_eq!(inbound.input_parts.len(), 1);
    assert!(matches!(
        inbound.input_parts[0],
        MessageInputPart::Image { .. }
    ));
}

#[test]
fn group_unaddressed_content_is_not_rewritten() {
    let keywords = vec!["召唤词".to_owned()];
    let content =
        build_group_respond_content(&group_message("  普通群消息  ", Some("member1")), &keywords);

    assert_eq!(content, "  普通群消息  ");
}

#[test]
fn quote_context_is_not_rendered_into_gateway_text_protocol() {
    let mut message = c2c_message("正文");
    message.reply = Some(MessageReply {
        message_id: "reply-1".to_owned(),
        ref_msg_idx: None,
        content: Some("被回复内容".to_owned()),
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    message.attachments = vec![Attachment {
        content_type: Some("image/png".to_owned()),
        filename: Some("a.png".to_owned()),
        url: Some("https://example.test/a.png".to_owned()),
        size_bytes: None,
        media_id: None,
        file_id: None,
        attachment_id: None,
        asr_refer_text: None,
        voice_wav_url: None,
    }];
    message
        .input_parts
        .push(message.attachments[0].to_input_part("qq_official"));

    let content = build_respond_content(&message);

    assert!(content.starts_with("正文"));
    assert!(content.contains("[图片 image/png: a.png]"));
}

#[test]
fn inbound_log_context_masks_private_user() {
    let inbound = platform::qq_official::inbound_from_c2c(&c2c_message("你好"));

    let (user, group) = masked_log_context_from_inbound(&inbound);

    assert_eq!(user.as_deref(), Some("******"));
    assert_eq!(group, None);
}

#[test]
fn inbound_log_context_masks_wechat_service_user() {
    let inbound = platform::wechat_service::inbound_from_text_message(
        &platform::wechat_service::WechatTextMessage {
            to_user_name: "gh_service".to_owned(),
            from_user_name: "wechat_user_openid_abcdef".to_owned(),
            create_time: Some("1460537339".to_owned()),
            content: "你好".to_owned(),
            msg_id: "msg-1".to_owned(),
        },
    );

    let (user, group) = masked_log_context_from_inbound(&inbound);

    assert_eq!(user.as_deref(), Some("******abcdef"));
    assert_ne!(user.as_deref(), Some("wechat_user_openid_abcdef"));
    assert_eq!(group, None);
}

#[test]
fn inbound_log_context_masks_group_target_without_member_user() {
    let mut message = group_message("你好", Some("member_openid_abcdef"));
    message.group_openid = "group_openid_123456".to_owned();
    let inbound = platform::qq_official::inbound_from_group(&message);

    let (user, group) = masked_log_context_from_inbound(&inbound);

    assert_eq!(user, None);
    assert_eq!(group.as_deref(), Some("******123456"));
    assert_ne!(group.as_deref(), Some("group_openid_123456"));
}

#[test]
fn unsafe_error_detail_is_not_shown_to_user() {
    let _response = CoreResponse {
        output: None,
        handled: Some(false),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    };

    let text = respond_error_to_qq_text(&RespondError::Core(CoreError::new(
        "bad_request",
        "provider",
        "Authorization Bearer sk-secret token leaked",
    )));

    assert_eq!(text, "请求格式有误，请调整后再试");
    assert!(!text.contains("sk-secret"));
}

#[test]
fn timeout_error_is_not_rendered_as_service_unavailable() {
    let text = respond_error_to_qq_text(&RespondError::Core(CoreError::new(
        "timeout",
        "stream_read",
        "internal timeout detail",
    )));

    assert_eq!(text, "LLM 请求超时，请稍后重试。");
    assert!(text.contains("超时"));
    assert!(!text.contains("不可用"));
}

/// 群聊寻址前缀下的污染检测：raw.content 包含 @机器人 前缀，
/// 归一化后正文才能正确匹配 msg_elements 引用文字的混合串。
///
/// 流程：事件解析 → 群聊正文归一化 → 污染检测 → 验证。
/// 完整 RefIndex 命中时由索引原文覆盖；被动观察命中时索引正文仍优先于事件文字。
#[test]
fn group_addressed_prefix_contamination_detected_after_normalization() {
    let mut message = group_message("@机器人 引用内容查看", Some("member-1"));
    message.event_type = GroupEventType::GroupAtMessage;
    message.reply = Some(MessageReply {
        message_id: String::new(),
        ref_msg_idx: Some("REFIDX_quoted".to_owned()),
        content: Some("测试引用内容查看".to_owned()),
        input_parts: vec![
            MessageInputPart::Text {
                text: "测试引用内容查看".to_owned(),
                source: Some(TextSource::Quote),
            },
            MessageInputPart::image(MessageMedia {
                mime_type: Some("image/png".to_owned()),
                filename: Some("quoted.png".to_owned()),
                url: Some("https://example.test/quoted.png".to_owned()),
                ..Default::default()
            }),
        ],
        media_summaries: Vec::new(),
    });

    // 群聊正文归一化后 "@机器人" 被移除。
    let mut inbound = normalized_group_inbound_with_prefix(
        &message,
        &["机器人".to_owned()],
        CommandPrefix::default(),
    );
    assert_eq!(inbound.text, "引用内容查看");

    // 污染检测使用归一化后的当前正文。
    if let Some(ref mut quoted) = inbound.quoted {
        strip_contaminated_quote_from_context(quoted, &inbound.text);
    }
    let quoted = inbound.quoted.as_ref().unwrap();

    // 被污染的引用文字 "测试引用内容查看" 已丢弃。
    assert_eq!(quoted.text_summary, None);
    assert!(
        quoted
            .input_parts
            .iter()
            .all(|part| !matches!(part, MessageInputPart::Text { .. }))
    );

    // 引用图片保留。
    assert_eq!(quoted.input_parts.len(), 1);
    assert!(matches!(
        quoted.input_parts[0],
        MessageInputPart::Image { .. }
    ));
}

/// 反例：引用正文以当前正文结尾但为独立语义时，不应判定为污染。
///
/// 引用正文 "这个方案很好" 以当前正文 "好" 结尾，但二者是独立内容，
/// 不属于 QQ msg_elements 混合串污染形态。
#[test]
fn short_current_body_ending_match_is_not_contamination() {
    let mut quoted = QuotedMessageContext {
        text_summary: Some("这个方案很好".to_owned()),
        input_parts: vec![MessageInputPart::Text {
            text: "这个方案很好".to_owned(),
            source: Some(TextSource::Quote),
        }],
        ..Default::default()
    };

    strip_contaminated_quote_from_context(&mut quoted, "好");

    // 引用正文不应被删除。
    assert_eq!(quoted.text_summary.as_deref(), Some("这个方案很好"));
    assert_eq!(quoted.input_parts.len(), 1);
    assert_eq!(quoted.input_parts[0].text_content(), Some("这个方案很好"));
}

/// 多 Text part 的引用上下文不触发污染检测。
///
/// 多个文字段落说明不是 QQ 引用消息的单一混合串形态，
/// 即使某一段落以后缀命中也不应删除全部引用文字。
#[test]
fn multiple_text_parts_does_not_trigger_contamination() {
    let mut quoted = QuotedMessageContext {
        text_summary: Some("第一段文字\n第二段 引用内容查看".to_owned()),
        input_parts: vec![
            MessageInputPart::Text {
                text: "第一段文字".to_owned(),
                source: Some(TextSource::Quote),
            },
            MessageInputPart::Text {
                text: "第二段 引用内容查看".to_owned(),
                source: Some(TextSource::Quote),
            },
        ],
        ..Default::default()
    };

    strip_contaminated_quote_from_context(&mut quoted, "引用内容查看");

    // 多段落不触发，引用文字全部保留。
    assert_eq!(quoted.input_parts.len(), 2);
    assert_eq!(quoted.input_parts[0].text_content(), Some("第一段文字"));
    assert_eq!(
        quoted.input_parts[1].text_content(),
        Some("第二段 引用内容查看")
    );
}
