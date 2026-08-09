use super::*;
use crate::gateway::bot_identity::BotIdentity;
use crate::gateway::event::{GroupMention, MessageReply};

fn bot_identity() -> SharedBotIdentity {
    Arc::new(BotIdentity::new("appid", &[]))
}

fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
    GroupMessage {
        message_id: "group-msg-1".to_owned(),
        current_msg_idx: None,
        group_openid: "group-1".to_owned(),
        member_openid: Some("member-1".to_owned()),
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
        event_type,
        author_is_bot: false,
        author_is_self: false,
    }
}

fn official_bot_mention() -> GroupMention {
    GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: None,
    }
}

#[test]
fn group_message_mode_policy_matches_triggers() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let ordinary = group_message("hello", GroupEventType::GroupMessage);
    let command = group_message("/rss", GroupEventType::GroupMessage);
    let mut bot_mention = group_message("@脸脸家的小女仆 hello", GroupEventType::GroupMessage);
    bot_mention.mentions = vec![official_bot_mention()];
    let active_keyword = group_message("小女仆在吗", GroupEventType::GroupMessage);
    let at_event = group_message("hello", GroupEventType::GroupAtMessage);

    assert!(!should_process_group_message(
        GroupMessageMode::Off,
        &active_keywords,
        &ordinary,
        &ordinary.content,
        &bot_identity(),
        &cache
    ));
    assert!(!should_process_group_message(
        GroupMessageMode::Off,
        &active_keywords,
        &at_event,
        &at_event.content,
        &bot_identity(),
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Command,
        &active_keywords,
        &command,
        &command.content,
        &bot_identity(),
        &cache
    ));
    for mode in [GroupMessageMode::Mention, GroupMessageMode::Active] {
        assert!(
            should_process_group_message(
                mode,
                &active_keywords,
                &command,
                &command.content,
                &bot_identity(),
                &cache
            ),
            "{mode:?} should forward slash candidates before wake filtering"
        );
    }
    assert!(!should_process_group_message(
        GroupMessageMode::Off,
        &active_keywords,
        &command,
        &command.content,
        &bot_identity(),
        &cache
    ));
    assert!(!should_process_group_message(
        GroupMessageMode::Command,
        &active_keywords,
        &bot_mention,
        &bot_mention.content,
        &bot_identity(),
        &cache
    ));
    assert!(!should_process_group_message(
        GroupMessageMode::Mention,
        &active_keywords,
        &ordinary,
        &ordinary.content,
        &bot_identity(),
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &active_keywords,
        &bot_mention,
        &bot_mention.content,
        &bot_identity(),
        &cache
    ));
    assert!(!should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &ordinary,
        &ordinary.content,
        &bot_identity(),
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &active_keyword,
        &active_keyword.content,
        &bot_identity(),
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &at_event,
        &at_event.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn structured_mention_slash_command_uses_normalized_content() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let mut message = group_message("@脸脸家的小女仆 /help", GroupEventType::GroupMessage);
    message.mentions = vec![official_bot_mention()];
    let respond_content = "/help";

    for mode in [
        GroupMessageMode::Command,
        GroupMessageMode::Mention,
        GroupMessageMode::Active,
    ] {
        assert!(
            should_process_group_message(
                mode,
                &active_keywords,
                &message,
                respond_content,
                &bot_identity(),
                &cache
            ),
            "{mode:?} should accept structured mention slash command"
        );
    }
}

#[test]
fn command_mode_uses_configured_prefix_for_direct_and_mentioned_commands() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let prefix = CommandPrefix::parse("#").unwrap();
    let direct = group_message("#help", GroupEventType::GroupMessage);
    let old = group_message("/help", GroupEventType::GroupMessage);
    let mut mentioned = group_message("@机器人 #help", GroupEventType::GroupMessage);
    mentioned.mentions = vec![official_bot_mention()];

    assert!(should_process_group_message_with_prefix(
        GroupMessageMode::Command,
        &[],
        prefix,
        &direct,
        "#help",
        &bot_identity(),
        &cache,
    ));
    assert!(!should_process_group_message_with_prefix(
        GroupMessageMode::Command,
        &[],
        prefix,
        &old,
        "/help",
        &bot_identity(),
        &cache,
    ));
    assert!(should_process_group_message_with_prefix(
        GroupMessageMode::Command,
        &[],
        prefix,
        &mentioned,
        "#help",
        &bot_identity(),
        &cache,
    ));
}

#[test]
fn structured_mention_slash_command_requires_current_bot_mention() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let mut message = group_message("@其他成员 /help", GroupEventType::GroupMessage);
    message.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: None,
    }];
    let respond_content = "/help";

    for mode in [
        GroupMessageMode::Command,
        GroupMessageMode::Mention,
        GroupMessageMode::Active,
    ] {
        assert!(
            !should_process_group_message(
                mode,
                &active_keywords,
                &message,
                respond_content,
                &bot_identity(),
                &cache
            ),
            "{mode:?} should ignore slash command aimed at another structured mention"
        );
    }
}

#[test]
fn active_mode_accepts_official_bot_mention() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let mut structured =
        group_message("@脸脸家的小女仆 实在是睡不着", GroupEventType::GroupMessage);
    structured.mentions = vec![official_bot_mention()];

    assert!(should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &structured,
        &structured.content,
        &bot_identity(),
        &cache
    ));

    let display = group_message("@机器人 实在是睡不着", GroupEventType::GroupMessage);
    assert!(!should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &display,
        &display.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn configured_bot_mention_id_no_longer_triggers_without_is_current_bot() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let message = group_message("@机器人 实在是睡不着", GroupEventType::GroupMessage);

    for mode in [GroupMessageMode::Mention, GroupMessageMode::Active] {
        assert!(
            !should_process_group_message(
                mode,
                &active_keywords,
                &message,
                &message.content,
                &bot_identity(),
                &cache
            ),
            "{mode:?} should ignore configured mention ids without official is_current_bot"
        );
    }
}

#[test]
fn content_mentions_do_not_trigger_without_official_is_current_bot() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];

    for input in [
        "[CQ:at,qq=other-user] hello",
        "[CQ:at,qq=appid] hello",
        "<@other-user> hello",
        "<@appid> hello",
        "@机器人 hello",
    ] {
        let message = group_message(input, GroupEventType::GroupMessage);
        for mode in [GroupMessageMode::Mention, GroupMessageMode::Active] {
            assert!(
                !should_process_group_message(
                    mode,
                    &active_keywords,
                    &message,
                    &message.content,
                    &bot_identity(),
                    &cache
                ),
                "{mode:?} should ignore non-bot mention: {input}"
            );
        }
    }
}

#[test]
fn group_at_event_trusts_official_event_type() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let mut message = group_message("@其他成员 hello", GroupEventType::GroupAtMessage);
    message.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: None,
    }];

    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &active_keywords,
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn group_at_event_with_empty_content_is_not_ignored() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let message = group_message("", GroupEventType::GroupAtMessage);

    assert!(!should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
}

#[test]
fn plain_group_message_with_empty_content_is_ignored() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let message = group_message("", GroupEventType::GroupMessage);

    assert!(should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
}

#[test]
fn quote_only_reply_to_cached_bot_message_is_not_ignored() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache.lock().unwrap().insert(Some("bot-msg-1".to_owned()));
    let mut message = group_message("", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "bot-msg-1".to_owned(),
        ref_msg_idx: None,
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(!should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        "",
        &bot_identity(),
        &cache
    ));
}

#[test]
fn quote_only_reply_to_cached_bot_refidx_is_not_ignored() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache
        .lock()
        .unwrap()
        .insert_ref_index_id(Some("REFIDX_bot_msg_1".to_owned()));
    let mut message = group_message("", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "msg-current-or-unknown".to_owned(),
        ref_msg_idx: Some("REFIDX_bot_msg_1".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(!should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        "",
        &bot_identity(),
        &cache
    ));
    assert!(!cache.lock().unwrap().contains("REFIDX_bot_msg_1"));
}

#[test]
fn quote_only_reply_message_id_does_not_match_refidx_cache_without_ref_msg_idx() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache
        .lock()
        .unwrap()
        .insert_ref_index_id(Some("REFIDX_bot_msg_1".to_owned()));
    let mut message = group_message("", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "REFIDX_bot_msg_1".to_owned(),
        ref_msg_idx: None,
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
    assert!(!should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        "",
        &bot_identity(),
        &cache
    ));
}

#[test]
fn group_at_event_with_other_content_mention_trusts_official_event_type() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let message = group_message(
        "[CQ:at,qq=other-user] hello",
        GroupEventType::GroupAtMessage,
    );

    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &active_keywords,
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn mention_mode_accepts_structured_bot_mention_only_for_official_is_current_bot() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let mut message = group_message("hello", GroupEventType::GroupMessage);
    message.mentions = vec![official_bot_mention()];

    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));

    message.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: None,
    }];
    assert!(!should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn normalizes_group_mentions_from_event_or_stable_bot_identity() {
    let identity = Arc::new(BotIdentity::new("appid", &["bot-openid".to_owned()]));
    let mut stable_match = group_message("hello", GroupEventType::GroupMessage);
    stable_match.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("bot-openid".to_owned()),
    }];
    normalize_current_bot_mentions(&mut stable_match, &identity);
    assert!(mentions_current_bot(&stable_match));

    let mut stable_mismatch = group_message("hello", GroupEventType::GroupMessage);
    stable_mismatch.mentions = vec![GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: Some("another-member".to_owned()),
    }];
    normalize_current_bot_mentions(&mut stable_mismatch, &identity);
    assert!(mentions_current_bot(&stable_mismatch));

    let mut legacy = group_message("hello", GroupEventType::GroupMessage);
    legacy.mentions = vec![GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: None,
    }];
    normalize_current_bot_mentions(&mut legacy, &identity);
    assert!(mentions_current_bot(&legacy));

    let mut at_with_matching_target = group_message("hello", GroupEventType::GroupAtMessage);
    at_with_matching_target.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("bot-openid".to_owned()),
    }];
    normalize_current_bot_mentions(&mut at_with_matching_target, &identity);
    assert!(at_with_matching_target.mentions[0].is_current_bot);
    assert!(mentions_current_bot(&at_with_matching_target));

    let mut at_event = group_message("hello", GroupEventType::GroupAtMessage);
    at_event.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("another-member".to_owned()),
    }];
    normalize_current_bot_mentions(&mut at_event, &identity);
    assert!(mentions_current_bot(&at_event));
}

#[test]
fn active_keyword_survives_empty_normalized_body() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let active_keywords = vec!["小女仆".to_owned()];
    let message = group_message("小女仆", GroupEventType::GroupMessage);

    // 唤醒词会被正文归一化剥掉，但 raw content 仍非空，不能在空内容过滤阶段丢弃。
    assert!(!should_ignore_group_message(
        &message,
        "",
        "masked-group",
        &cache
    ));
    assert!(should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &message,
        "",
        &bot_identity(),
        &cache
    ));

    let mut other_bot = group_message("你好", GroupEventType::GroupMessage);
    other_bot.mentions = vec![GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("other-bot".to_owned()),
    }];
    normalize_current_bot_mentions(&mut other_bot, &bot_identity());
    assert!(!should_process_group_message(
        GroupMessageMode::Active,
        &active_keywords,
        &other_bot,
        &other_bot.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn markdown_mention_cleanup_preserves_plain_and_other_member_content() {
    let identity = Arc::new(BotIdentity::new("app", &[]));
    let mut plain = group_message("原始数据", GroupEventType::GroupMessage);
    normalize_current_bot_mentions(&mut plain, &identity);
    assert_eq!(plain.content, "原始数据");
    assert_eq!(plain.input_parts[0].text_content(), Some("原始数据"));

    let mut repeated = group_message(
        "[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=other) [@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=app) [@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=app) 原始数据",
        GroupEventType::GroupMessage,
    );
    repeated.mentions = vec![
        GroupMention {
            is_current_bot: false,
            member_role: None,
            target_id: Some("other".to_owned()),
        },
        GroupMention {
            is_current_bot: false,
            member_role: None,
            target_id: Some("app".to_owned()),
        },
        GroupMention {
            is_current_bot: false,
            member_role: None,
            target_id: Some("app".to_owned()),
        },
    ];
    normalize_current_bot_mentions(&mut repeated, &identity);
    assert_eq!(
        repeated.content,
        "[@张三](mqqapi://markdown/mention?at_type=1&at_tinyid=other) 原始数据"
    );
    assert_eq!(
        repeated.input_parts[0].text_content(),
        Some(repeated.content.as_str())
    );
}

#[test]
fn reply_to_cached_bot_message_triggers_mention_mode() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache.lock().unwrap().insert(Some("bot-msg-1".to_owned()));
    let mut message = group_message("继续", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "bot-msg-1".to_owned(),
        ref_msg_idx: None,
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn reply_to_cached_bot_refidx_triggers_mention_mode() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache
        .lock()
        .unwrap()
        .insert_ref_index_id(Some("REFIDX_bot_msg_1".to_owned()));
    let mut message = group_message("继续", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "msg-current-or-unknown".to_owned(),
        ref_msg_idx: Some("REFIDX_bot_msg_1".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
    assert!(!cache.lock().unwrap().contains("REFIDX_bot_msg_1"));
}

#[test]
fn reply_message_id_does_not_trigger_mention_mode_from_refidx_cache_without_ref_msg_idx() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    cache
        .lock()
        .unwrap()
        .insert_ref_index_id(Some("REFIDX_bot_msg_1".to_owned()));
    let mut message = group_message("继续", GroupEventType::GroupMessage);
    message.reply = Some(MessageReply {
        message_id: "REFIDX_bot_msg_1".to_owned(),
        ref_msg_idx: None,
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });

    assert!(!should_process_group_message(
        GroupMessageMode::Mention,
        &[],
        &message,
        &message.content,
        &bot_identity(),
        &cache
    ));
}

#[test]
fn group_cooldown_blocks_same_group_temporarily() {
    let mut cooldowns = GroupCooldowns::default();
    let message = group_message("hello", GroupEventType::GroupMessage);
    let now = Instant::now();

    assert!(cooldowns.check_and_mark(&message, now));
    assert!(!cooldowns.check_and_mark(&message, now + Duration::from_secs(1)));
    assert!(cooldowns.check_and_mark(&message, now + GROUP_USER_COOLDOWN + Duration::from_secs(1)));
}

#[test]
fn explicit_mention_or_reply_to_bot_addresses_bot() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));

    // 普通群消息既不 @ 机器人也不引用机器人，不属于明确指向机器人。
    let ordinary = group_message("随便聊聊", GroupEventType::GroupMessage);
    assert!(!group_message_addresses_bot(&ordinary, &cache));

    // 结构化 @ 机器人的普通群消息明确指向机器人。
    let mut mentioned = group_message("总结一下", GroupEventType::GroupMessage);
    mentioned.mentions = vec![official_bot_mention()];
    assert!(group_message_addresses_bot(&mentioned, &cache));

    // GROUP_AT_MESSAGE_CREATE 事件本身就是 @ 机器人，明确指向机器人。
    let at_event = group_message("总结一下", GroupEventType::GroupAtMessage);
    assert!(group_message_addresses_bot(&at_event, &cache));

    // 引用机器人刚发出的回复（命中 outbound ref_index id）明确指向机器人。
    let mut quoted = group_message("总结一下", GroupEventType::GroupMessage);
    quoted.reply = Some(MessageReply {
        message_id: "qq_reply_id".to_owned(),
        ref_msg_idx: Some("REFIDX_bot_reply".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    {
        let mut guard = cache.lock().unwrap();
        guard.insert_ref_index_id(Some("REFIDX_bot_reply".to_owned()));
    }
    assert!(group_message_addresses_bot(&quoted, &cache));

    // 引用普通用户消息（未命中 outbound 缓存）不属于明确指向机器人。
    let mut quoted_user = group_message("这句话什么意思", GroupEventType::GroupMessage);
    quoted_user.reply = Some(MessageReply {
        message_id: "user_msg_id".to_owned(),
        ref_msg_idx: None,
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    assert!(!group_message_addresses_bot(&quoted_user, &cache));
}
