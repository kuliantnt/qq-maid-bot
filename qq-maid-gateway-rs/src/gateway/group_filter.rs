//! 群消息过滤与冷却判定。
//!
//! 从 `gateway/mod.rs` 提取的纯判定逻辑，负责：
//! - 自身 / bot 消息和普通空内容过滤（`should_ignore_group_message`）；
//! - 按群消息模式（Off / Command / Mention / Active）决定是否处理（`should_process_group_message`）；
//! - 群级和用户级冷却（`GroupCooldowns`）。
//!
//! 这些逻辑不涉及 LLM 调用或 QQ 发送，只依赖群消息结构、模式配置和机器人 outbound 缓存，
//! 独立成模块便于维护和单测。

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use qq_maid_common::command_prefix::CommandPrefix;
use tracing::debug;

use super::{
    BotOutboundCache,
    bot_identity::SharedBotIdentity,
    event::{GroupEventType, GroupMessage},
};
use crate::config::GroupMessageMode;

/// 群级冷却：同一群短时间内只处理一条消息，避免刷屏。
pub(crate) const GROUP_COOLDOWN: Duration = Duration::from_secs(3);
/// 群内单用户冷却：限制同一用户在群内的高频发言触发。
pub(crate) const GROUP_USER_COOLDOWN: Duration = Duration::from_secs(10);

/// 群消息冷却记录，按群 openid 和"群:用户"键分别记录最近触发时间。
#[derive(Debug, Default)]
pub(crate) struct GroupCooldowns {
    groups: HashMap<String, Instant>,
    users: HashMap<String, Instant>,
}

impl GroupCooldowns {
    /// 检查并标记：若群或用户仍在冷却期内返回 `false`，否则记录当前时间并返回 `true`。
    pub(crate) fn check_and_mark(&mut self, message: &GroupMessage, now: Instant) -> bool {
        self.retain(now);
        let user_key = group_user_key(message);
        if self
            .groups
            .get(&message.group_openid)
            .is_some_and(|last| now.duration_since(*last) < GROUP_COOLDOWN)
            || self
                .users
                .get(&user_key)
                .is_some_and(|last| now.duration_since(*last) < GROUP_USER_COOLDOWN)
        {
            return false;
        }
        self.groups.insert(message.group_openid.clone(), now);
        self.users.insert(user_key, now);
        true
    }

    /// 清理已过期的冷却记录，避免 HashMap 无限增长。
    fn retain(&mut self, now: Instant) {
        self.groups
            .retain(|_, last| now.duration_since(*last) <= GROUP_COOLDOWN);
        self.users
            .retain(|_, last| now.duration_since(*last) <= GROUP_USER_COOLDOWN);
    }
}

/// 判断群消息是否应被忽略（自身消息、bot 消息、普通空内容）。
///
/// `masked_group` 仅用于日志脱敏展示，不影响判定结果。
pub(crate) fn should_ignore_group_message(
    message: &GroupMessage,
    respond_content: &str,
    masked_group: &str,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    if message.author_is_self {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring self group message"
        );
        return true;
    }
    if message.author_is_bot {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring bot group message"
        );
        return true;
    }
    if !mentions_current_bot(message)
        && respond_content.trim().is_empty()
        && !is_reply_to_bot(message, bot_outbound_cache)
    {
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            "ignoring empty group message"
        );
        return true;
    }
    false
}

/// 按群消息模式策略判断是否应处理该消息。
///
/// QQ 官方 at 事件直接视为提到当前机器人；普通群消息优先由 READY 阶段学习的稳定身份
/// 字段匹配，`is_you` 仅用于没有稳定身份字段的旧事件兼容。
/// 后续只按群消息模式决定是否进入 Core：
/// - Off：不处理；
/// - 其他模式：先放行斜杠命令候选，再应用各自的唤醒规则；
/// - Command：除直接斜杠候选外，仅接受归一化后的 @ 命令；
/// - Mention：提到机器人或回复机器人；
/// - Active：提到机器人或命中配置提示词。
///
/// 这些本地策略只对 QQ 官方已经推送到 Gateway 的群事件生效，关键词不能让平台额外推送
/// 原本不可见的普通非 @ 消息。
#[cfg(test)]
pub(crate) fn should_process_group_message(
    mode: GroupMessageMode,
    active_keywords: &[String],
    message: &GroupMessage,
    respond_content: &str,
    bot_identity: &SharedBotIdentity,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    should_process_group_message_with_prefix(
        mode,
        active_keywords,
        CommandPrefix::default(),
        message,
        respond_content,
        bot_identity,
        bot_outbound_cache,
    )
}

pub(crate) fn should_process_group_message_with_prefix(
    mode: GroupMessageMode,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
    message: &GroupMessage,
    respond_content: &str,
    _bot_identity: &SharedBotIdentity,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    let mentions_current_bot = mentions_current_bot(message);

    // QQ 有时把 `@机器人 /help` 作为普通群消息下发；
    // 此时原始 content 不是斜杠开头，需要使用 gateway 已归一化的 Core 文本判断命令。
    let is_direct_command_candidate = command_prefix.is_candidate(&message.content);
    let is_normalized_command = command_prefix.is_candidate(respond_content);
    let is_structured_mention_command = mentions_current_bot && is_normalized_command;

    match mode {
        GroupMessageMode::Off => false,
        // 斜杠候选必须先于唤醒判断进入 Core；是否合法、是否有权限均由 Core 决定。
        _ if is_direct_command_candidate => true,
        GroupMessageMode::Command => is_structured_mention_command,
        GroupMessageMode::Mention => {
            is_structured_mention_command
                || mentions_current_bot
                || is_reply_to_bot(message, bot_outbound_cache)
        }
        GroupMessageMode::Active => {
            is_structured_mention_command
                || mentions_current_bot
                || contains_active_keyword(&message.content, active_keywords)
        }
    }
}

/// 在普通群消息进入过滤、命令和 Core adapter 前统一解析“是否 @ 当前机器人”。
/// 稳定 ID 的判断优先级高于旧 `is_you`：即便旧字段为 true，只要事件同时给出
/// 不匹配的稳定 ID，也不能把任意 mention 误判为当前机器人。
pub(crate) fn normalize_current_bot_mentions(
    message: &mut GroupMessage,
    bot_identity: &SharedBotIdentity,
) {
    if message.event_type == GroupEventType::GroupAtMessage {
        return;
    }
    for mention in &mut message.mentions {
        if let Some(target_id) = mention.target_id.as_deref() {
            mention.is_you = bot_identity.contains(target_id);
        } else if mention.is_you {
            // 旧事件没有稳定 mention ID 时才接受 is_you；不记录用户或群完整 ID。
            debug!(
                event_type = "group_message",
                mention_identity_source = "legacy_is_you",
                "QQ group mention used legacy is_you fallback"
            );
        }
    }
}

pub(crate) fn mentions_current_bot(message: &GroupMessage) -> bool {
    message.event_type == GroupEventType::GroupAtMessage
        || message.mentions.iter().any(|mention| mention.is_you)
}

/// `active` 模式只按显式提示词触发，避免普通群聊闲谈被机器人自动插话。
fn contains_active_keyword(content: &str, keywords: &[String]) -> bool {
    let content = content.to_ascii_lowercase();
    keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .any(|keyword| content.contains(&keyword.to_ascii_lowercase()))
}

/// 判断消息是否为回复机器人发出的消息（通过 outbound 缓存匹配 reply.message_id）。
fn is_reply_to_bot(
    message: &GroupMessage,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    message.reply.as_ref().is_some_and(|reply| {
        let mut cache = bot_outbound_cache.lock().unwrap();
        cache.contains(&reply.message_id)
            || reply
                .ref_msg_idx
                .as_deref()
                .is_some_and(|ref_msg_idx| cache.contains_ref_index_id(ref_msg_idx))
    })
}

/// 群普通消息是否明确指向当前机器人。
///
/// 普通群消息（`GROUP_MESSAGE_CREATE`）的 `NormalChat` 默认受群级/用户级冷却限制以避免
/// 刷屏；Core 分类为 `Immediate` 的命令或 Pending 后续操作已由调用方提前绕过该冷却。
/// 对仍受冷却限制的普通聊天，这里只判定“是否明确指向机器人”：命中时发送轻量提示，
/// 未命中时静默忽略，避免高频 @ 普通聊天短期堆积模型成本。
pub(crate) fn group_message_addresses_bot(
    message: &GroupMessage,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    mentions_current_bot(message) || is_reply_to_bot(message, bot_outbound_cache)
}

/// 构造群内用户冷却键：`group_openid:member_openid`。
pub(crate) fn group_user_key(message: &GroupMessage) -> String {
    let member = message.member_openid.as_deref().unwrap_or("unknown");
    format!("{}:{member}", message.group_openid)
}

#[cfg(test)]
mod tests {
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
            is_you: true,
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
            is_you: false,
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
    fn configured_bot_mention_id_no_longer_triggers_without_is_you() {
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
                "{mode:?} should ignore configured mention ids without official is_you"
            );
        }
    }

    #[test]
    fn content_mentions_do_not_trigger_without_official_is_you() {
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
            is_you: false,
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
    fn mention_mode_accepts_structured_bot_mention_only_for_official_is_you() {
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
            is_you: false,
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
    fn normalizes_group_mentions_by_stable_bot_identity_before_legacy_is_you() {
        let identity = Arc::new(BotIdentity::new("appid", &["bot-openid".to_owned()]));
        let mut stable_match = group_message("hello", GroupEventType::GroupMessage);
        stable_match.mentions = vec![GroupMention {
            is_you: false,
            member_role: None,
            target_id: Some("bot-openid".to_owned()),
        }];
        normalize_current_bot_mentions(&mut stable_match, &identity);
        assert!(mentions_current_bot(&stable_match));

        let mut stable_mismatch = group_message("hello", GroupEventType::GroupMessage);
        stable_mismatch.mentions = vec![GroupMention {
            is_you: true,
            member_role: None,
            target_id: Some("another-member".to_owned()),
        }];
        normalize_current_bot_mentions(&mut stable_mismatch, &identity);
        assert!(!mentions_current_bot(&stable_mismatch));

        let mut legacy = group_message("hello", GroupEventType::GroupMessage);
        legacy.mentions = vec![GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];
        normalize_current_bot_mentions(&mut legacy, &identity);
        assert!(mentions_current_bot(&legacy));

        let mut at_event = group_message("hello", GroupEventType::GroupAtMessage);
        at_event.mentions = vec![GroupMention {
            is_you: false,
            member_role: None,
            target_id: Some("another-member".to_owned()),
        }];
        normalize_current_bot_mentions(&mut at_event, &identity);
        assert!(mentions_current_bot(&at_event));
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
        assert!(
            cooldowns.check_and_mark(&message, now + GROUP_USER_COOLDOWN + Duration::from_secs(1))
        );
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
}
