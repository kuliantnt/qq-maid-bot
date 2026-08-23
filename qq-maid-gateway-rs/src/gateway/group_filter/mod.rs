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
use tracing::trace;

mod mention_normalizer;

pub(crate) use mention_normalizer::normalize_current_bot_mentions;

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
        trace!(
            message_id = %message.message_id,
            group = %masked_group,
            "已忽略机器人自身发送的群聊消息"
        );
        return true;
    }
    if message.author_is_bot {
        trace!(
            message_id = %message.message_id,
            group = %masked_group,
            "已忽略其他机器人发送的群聊消息"
        );
        return true;
    }
    if !mentions_current_bot(message)
        && respond_content.trim().is_empty()
        && message.content.trim().is_empty()
        && !is_reply_to_bot(message, bot_outbound_cache)
    {
        trace!(
            message_id = %message.message_id,
            group = %masked_group,
            "已忽略空群聊消息"
        );
        return true;
    }
    false
}

/// 按群消息模式策略判断是否应处理该消息。
///
/// QQ 官方 at 事件直接视为提到当前机器人；普通群消息保留 QQ 结构化的当前机器人标记，
/// 并用 mention 的稳定 target_id 与 READY 阶段学习的机器人身份集合补充确认。
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
    let is_direct_command_candidate =
        command_prefix.is_candidate_with_sealdice_compat(&message.content);
    let is_normalized_command = command_prefix.is_candidate_with_sealdice_compat(respond_content);
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

pub(crate) fn mentions_current_bot(message: &GroupMessage) -> bool {
    message.event_type == GroupEventType::GroupAtMessage
        || message
            .mentions
            .iter()
            .any(|mention| mention.is_current_bot)
}

/// `active` 模式只按显式提示词触发，避免普通群聊闲谈被机器人自动插话。
pub(crate) fn contains_active_keyword(content: &str, keywords: &[String]) -> bool {
    let content = content.to_ascii_lowercase();
    keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .any(|keyword| content.contains(&keyword.to_ascii_lowercase()))
}

/// 判断消息是否为回复机器人发出的消息（通过 outbound 缓存匹配 reply.message_id）。
pub(crate) fn is_reply_to_bot(
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
mod tests;
