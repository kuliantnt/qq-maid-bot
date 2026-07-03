//! 群消息过滤与冷却判定。
//!
//! 从 `gateway/mod.rs` 提取的纯判定逻辑，负责：
//! - 自身 / bot 消息和空内容过滤（`should_ignore_group_message`）；
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

use tracing::debug;

use super::{
    BotOutboundCache,
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

/// 判断群消息是否应被忽略（自身消息、bot 消息、空内容）。
///
/// `masked_group` 仅用于日志脱敏展示，不影响判定结果。
pub(crate) fn should_ignore_group_message(
    message: &GroupMessage,
    respond_content: &str,
    masked_group: &str,
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
    if respond_content.trim().is_empty() {
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
/// `GroupAtMessage` 事件始终处理；其余按模式：
/// - Off：不处理；
/// - Command：仅斜杠命令；
/// - Mention：命令、@机器人、回复机器人；
/// - Active：仅处理命中配置提示词的普通群消息。
pub(crate) fn should_process_group_message(
    mode: GroupMessageMode,
    active_keywords: &[String],
    message: &GroupMessage,
    respond_content: &str,
    bot_app_id: &str,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    if message.event_type == GroupEventType::GroupAtMessage {
        return true;
    }

    // QQ 有时把 `@机器人 /help` 作为普通群消息下发；
    // 此时原始 content 不是斜杠开头，需要使用 gateway 已归一化的 Core 文本判断命令。
    let is_normalized_command = is_group_command(respond_content);
    let is_structured_mention_command = mentions_bot(message, bot_app_id) && is_normalized_command;

    match mode {
        GroupMessageMode::Off => false,
        GroupMessageMode::Command => {
            is_group_command(&message.content) || is_structured_mention_command
        }
        GroupMessageMode::Mention => {
            is_group_command(&message.content)
                || is_structured_mention_command
                || mentions_bot(message, bot_app_id)
                || contains_bot_mention(&message.content)
                || is_reply_to_bot(message, bot_outbound_cache)
        }
        GroupMessageMode::Active => {
            is_structured_mention_command
                || mentions_bot(message, bot_app_id)
                || contains_bot_mention(&message.content)
                || contains_active_keyword(&message.content, active_keywords)
        }
    }
}

/// 判断内容是否以 `/` 或全角 `／` 开头（群命令）。
fn is_group_command(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with('/') || trimmed.starts_with('／')
}

/// 判断内容是否包含 @机器人 标记（CQ:at / <@ / @机器人）。
fn contains_bot_mention(content: &str) -> bool {
    content.contains("CQ:at") || content.contains("<@") || content.contains("@机器人")
}

fn mentions_bot(message: &GroupMessage, bot_app_id: &str) -> bool {
    let bot_app_id = bot_app_id.trim();
    !bot_app_id.is_empty()
        && message
            .mention_ids
            .iter()
            .any(|mention_id| mention_id.trim() == bot_app_id)
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
        bot_outbound_cache
            .lock()
            .unwrap()
            .contains(&reply.message_id)
    })
}

/// 构造群内用户冷却键：`group_openid:member_openid`。
pub(crate) fn group_user_key(message: &GroupMessage) -> String {
    let member = message.member_openid.as_deref().unwrap_or("unknown");
    format!("{}:{member}", message.group_openid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::event::MessageReply;

    fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("member-1".to_owned()),
            content: content.to_owned(),
            mention_ids: Vec::new(),
            reply: None,
            timestamp: None,
            attachments: Vec::new(),
            event_type,
            author_is_bot: false,
            author_is_self: false,
        }
    }

    #[test]
    fn group_message_mode_policy_matches_triggers() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let active_keywords = vec!["小女仆".to_owned()];
        let ordinary = group_message("hello", GroupEventType::GroupMessage);
        let command = group_message("/rss", GroupEventType::GroupMessage);
        let mention = group_message("[CQ:at,qq=123] hello", GroupEventType::GroupMessage);
        let active_keyword = group_message("小女仆在吗", GroupEventType::GroupMessage);
        let at_event = group_message("hello", GroupEventType::GroupAtMessage);

        assert!(!should_process_group_message(
            GroupMessageMode::Off,
            &active_keywords,
            &ordinary,
            &ordinary.content,
            "appid",
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Off,
            &active_keywords,
            &at_event,
            &at_event.content,
            "appid",
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Command,
            &active_keywords,
            &command,
            &command.content,
            "appid",
            &cache
        ));
        assert!(!should_process_group_message(
            GroupMessageMode::Command,
            &active_keywords,
            &mention,
            &mention.content,
            "appid",
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &active_keywords,
            &mention,
            &mention.content,
            "appid",
            &cache
        ));
        assert!(!should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &ordinary,
            &ordinary.content,
            "appid",
            &cache
        ));
        assert!(should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &active_keyword,
            &active_keyword.content,
            "appid",
            &cache
        ));
    }

    #[test]
    fn structured_mention_slash_command_uses_normalized_content() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let active_keywords = vec!["小女仆".to_owned()];
        let mut message = group_message("@脸脸家的小女仆 /help", GroupEventType::GroupMessage);
        message.mention_ids = vec!["appid".to_owned()];
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
                    "appid",
                    &cache
                ),
                "{mode:?} should accept structured mention slash command"
            );
        }
    }

    #[test]
    fn structured_mention_slash_command_requires_current_bot_mention() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let active_keywords = vec!["小女仆".to_owned()];
        let mut message = group_message("@其他成员 /help", GroupEventType::GroupMessage);
        message.mention_ids = vec!["other-user".to_owned()];
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
                    "appid",
                    &cache
                ),
                "{mode:?} should ignore slash command aimed at another structured mention"
            );
        }
    }

    #[test]
    fn active_mode_accepts_direct_bot_mention_text() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let active_keywords = vec!["小女仆".to_owned()];
        let mut structured = group_message("@脸脸家的小女仆 实在是睡不着", GroupEventType::GroupMessage);
        structured.mention_ids = vec!["appid".to_owned()];

        assert!(should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &structured,
            &structured.content,
            "appid",
            &cache
        ));

        let display = group_message("@机器人 实在是睡不着", GroupEventType::GroupMessage);
        assert!(should_process_group_message(
            GroupMessageMode::Active,
            &active_keywords,
            &display,
            &display.content,
            "appid",
            &cache
        ));
    }

    #[test]
    fn mention_mode_accepts_structured_bot_mention_only_for_configured_app_id() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let mut message = group_message("hello", GroupEventType::GroupMessage);
        message.mention_ids = vec!["appid".to_owned()];

        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &[],
            &message,
            &message.content,
            "appid",
            &cache
        ));

        message.mention_ids = vec!["other-user".to_owned()];
        assert!(!should_process_group_message(
            GroupMessageMode::Mention,
            &[],
            &message,
            &message.content,
            "appid",
            &cache
        ));
    }

    #[test]
    fn reply_to_cached_bot_message_triggers_mention_mode() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        cache.lock().unwrap().insert(Some("bot-msg-1".to_owned()));
        let mut message = group_message("继续", GroupEventType::GroupMessage);
        message.reply = Some(MessageReply {
            message_id: "bot-msg-1".to_owned(),
            content: None,
        });

        assert!(should_process_group_message(
            GroupMessageMode::Mention,
            &[],
            &message,
            &message.content,
            "appid",
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
}
