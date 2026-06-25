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
/// - Active：全部处理。
pub(crate) fn should_process_group_message(
    mode: GroupMessageMode,
    message: &GroupMessage,
    bot_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
) -> bool {
    if message.event_type == GroupEventType::GroupAtMessage {
        return true;
    }

    match mode {
        GroupMessageMode::Off => false,
        GroupMessageMode::Command => is_group_command(&message.content),
        GroupMessageMode::Mention => {
            is_group_command(&message.content)
                || contains_bot_mention(&message.content)
                || is_reply_to_bot(message, bot_outbound_cache)
        }
        GroupMessageMode::Active => true,
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
