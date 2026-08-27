//! 群聊 @、Active Keyword 和命令前缀的寻址归一化。

use qq_maid_common::{command_prefix::CommandPrefix, input_part::MessageInputPart};

use crate::{event::GroupMessage, gateway::platform};

pub fn build_group_respond_content(message: &GroupMessage, active_keywords: &[String]) -> String {
    build_group_respond_content_with_prefix(message, active_keywords, CommandPrefix::default())
}

pub(crate) fn build_group_respond_content_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    let inbound = normalized_group_inbound_with_prefix(message, active_keywords, command_prefix);
    platform::render_text_for_core(&inbound)
}

/// 群聊本地命令只允许使用用户本轮显式正文。ARK、平行消息和聊天记录的安全摘要
/// 会进入 `input_parts` 供 Core 理解，但绝不能被 Gateway 当成用户输入的 slash 命令。
pub(crate) fn build_group_command_content_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    normalize_group_addressed_content(message, &message.content, active_keywords, command_prefix)
}

#[cfg(test)]
pub(crate) fn normalized_group_inbound(
    message: &GroupMessage,
    active_keywords: &[String],
) -> platform::InboundMessage {
    normalized_group_inbound_with_prefix(message, active_keywords, CommandPrefix::default())
}

pub(crate) fn normalized_group_inbound_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> platform::InboundMessage {
    let content = normalize_group_addressed_content(
        message,
        &message.content,
        active_keywords,
        command_prefix,
    );
    let mut inbound = platform::qq_official::inbound_from_group(message);
    inbound.text = content.clone();
    // Core 只消费平台无关的寻址事实。QQ 结构化 @ 已由 adapter 标记，Active 模式
    // 的配置唤醒词在归一化边界补充，不能让 Core 理解 GROUP_ACTIVE_KEYWORDS。
    inbound.mentioned_bot |=
        crate::gateway::contains_active_keyword(&message.content, active_keywords);

    // 有序内容块存在时 Core 会优先使用 input_parts。寻址 mention 只改写正文文本块，
    // 因此仅同步首个正文文本块，媒体块及其相对顺序、状态和元数据保持原样。
    if content != message.content
        && let Some(MessageInputPart::Text { text, .. }) = inbound.input_parts.first_mut()
    {
        *text = normalize_group_addressed_content(message, text, active_keywords, command_prefix);
        if text.is_empty() {
            inbound.input_parts.remove(0);
        }
    }

    inbound
}

fn normalize_group_addressed_content(
    message: &GroupMessage,
    content: &str,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    let mut candidate = content.trim_start();
    let mut stripped_address = false;
    let mut mention_index = 0usize;
    let mut stripped_mention = false;
    for _ in 0..4 {
        if let Some(command) = command_remainder(candidate, command_prefix) {
            return command;
        }
        if let Some((rest, prefix_kind)) = strip_group_command_prefix(
            candidate,
            message,
            active_keywords,
            mention_index,
            stripped_mention,
        ) {
            candidate = rest;
            stripped_address = true;
            if prefix_kind == GroupAddressPrefixKind::Mention {
                mention_index += 1;
                stripped_mention = true;
            }
            continue;
        }
        break;
    }
    if let Some(rest) = strip_group_command_suffix(candidate, message, active_keywords) {
        candidate = rest;
        stripped_address = true;
    }
    if stripped_address {
        if let Some(command) = command_remainder(candidate, command_prefix) {
            return command;
        }
        if command_prefix.as_char() != '/' && candidate.trim_start().starts_with('/') {
            // 自定义前缀启用后，`@机器人 /help` 只是普通正文；保留原始寻址文本，避免
            // Gateway 后续把剥离后的 `/help` 误判成已经规范化的配置命令。
            return content.to_owned();
        }
        trim_command_separator(candidate.trim_start())
            .trim()
            .to_owned()
    } else {
        content.to_owned()
    }
}

fn command_remainder(text: &str, command_prefix: CommandPrefix) -> Option<String> {
    let rest = trim_command_separator(text.trim_start());
    if command_prefix.is_candidate_with_dot_compat(rest) {
        // Core 负责把配置前缀规范化为内部 `/`；Gateway 这里只剥离 @/唤醒词，
        // 必须保留配置字符，避免跨层重复规范化后被当成普通文本。
        return Some(rest.trim().to_owned());
    }
    if command_prefix.as_char() == '/'
        && let Some(command) = rest.strip_prefix('／')
    {
        return Some(format!("/{command}").trim().to_owned());
    }
    None
}

fn trim_command_separator(text: &str) -> &str {
    text.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupAddressPrefixKind {
    Mention,
    ActiveKeyword,
}

fn strip_group_command_prefix<'a>(
    text: &'a str,
    message: &GroupMessage,
    active_keywords: &[String],
    mention_index: usize,
    stripped_mention: bool,
) -> Option<(&'a str, GroupAddressPrefixKind)> {
    let text = text.trim_start();
    if let Some((rest, target_id)) = strip_cq_at_prefix(text)
        && can_strip_encoded_mention(message, mention_index, stripped_mention, target_id)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    if let Some((rest, target_id)) = strip_angle_mention_prefix(text)
        && can_strip_encoded_mention(message, mention_index, stripped_mention, target_id)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    if let Some((rest, display_name)) = strip_display_mention_prefix(text)
        && can_strip_display_mention(message, active_keywords, mention_index, display_name)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    strip_active_keyword_prefix(text, active_keywords)
        .map(|rest| (rest, GroupAddressPrefixKind::ActiveKeyword))
}

fn strip_group_command_suffix<'a>(
    text: &'a str,
    message: &GroupMessage,
    active_keywords: &[String],
) -> Option<&'a str> {
    let text = text.trim_end();
    if let Some((rest, target_id)) = strip_cq_at_suffix(text)
        && can_strip_encoded_mention_suffix(message, target_id)
    {
        return Some(trim_group_address_suffix(rest));
    }
    if let Some((rest, target_id)) = strip_angle_mention_suffix(text)
        && can_strip_encoded_mention_suffix(message, target_id)
    {
        return Some(trim_group_address_suffix(rest));
    }
    if let Some((rest, display_name)) = strip_display_mention_suffix(text)
        && can_strip_display_mention_suffix(message, active_keywords, display_name)
    {
        return Some(trim_group_address_suffix(rest));
    }
    None
}

fn strip_cq_at_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("[CQ:at,")?;
    let end = rest.find(']')?;
    let attributes = &rest[..end];
    let target_id = attributes
        .split(',')
        .find_map(|attribute| attribute.strip_prefix("qq="))?;
    Some((&rest[end + 1..], target_id))
}

fn strip_angle_mention_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("<@")?;
    let end = rest.find('>')?;
    let target_id = rest[..end].strip_prefix('!').unwrap_or(&rest[..end]);
    if target_id.trim().is_empty() {
        return None;
    }
    Some((&rest[end + 1..], target_id))
}

fn strip_display_mention_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('@')?;
    let split_at = rest.find(is_group_address_separator)?;
    Some((&rest[split_at..], &rest[..split_at]))
}

fn strip_cq_at_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind("[CQ:at,")?;
    let (rest, target_id) = strip_cq_at_prefix(&text[start..])?;
    rest.is_empty().then_some((&text[..start], target_id))
}

fn strip_angle_mention_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind("<@")?;
    let (rest, target_id) = strip_angle_mention_prefix(&text[start..])?;
    rest.is_empty().then_some((&text[..start], target_id))
}

fn strip_display_mention_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind('@')?;
    let display_name = text[start + 1..].trim();
    (!display_name.is_empty() && !display_name.chars().any(char::is_whitespace))
        .then_some((&text[..start], display_name))
}

fn can_strip_encoded_mention(
    message: &GroupMessage,
    mention_index: usize,
    stripped_mention: bool,
    target_id: &str,
) -> bool {
    if let Some(mention) = message.mentions.get(mention_index) {
        return mention.is_current_bot
            && mention
                .target_id
                .as_deref()
                .is_none_or(|expected| expected == target_id);
    }
    !stripped_mention && message.event_type == crate::event::GroupEventType::GroupAtMessage
}

fn can_strip_display_mention(
    message: &GroupMessage,
    active_keywords: &[String],
    mention_index: usize,
    display_name: &str,
) -> bool {
    if let Some(mention) = message.mentions.get(mention_index) {
        return mention.is_current_bot;
    }
    // 缺少结构化身份时只兼容已配置的机器人展示名，不能把任意 @群成员当作寻址前缀。
    active_keywords.iter().any(|keyword| {
        let keyword = keyword.trim();
        !keyword.is_empty() && display_name.eq_ignore_ascii_case(keyword)
    })
}

fn can_strip_encoded_mention_suffix(message: &GroupMessage, target_id: &str) -> bool {
    if let Some(mention) = message.mentions.last() {
        return mention.is_current_bot
            && mention
                .target_id
                .as_deref()
                .is_none_or(|expected| expected == target_id);
    }
    message.event_type == crate::event::GroupEventType::GroupAtMessage
}

fn can_strip_display_mention_suffix(
    message: &GroupMessage,
    active_keywords: &[String],
    display_name: &str,
) -> bool {
    if let Some(mention) = message.mentions.last() {
        return mention.is_current_bot;
    }
    active_keywords.iter().any(|keyword| {
        let keyword = keyword.trim();
        !keyword.is_empty() && display_name.eq_ignore_ascii_case(keyword)
    })
}

fn trim_group_address_suffix(text: &str) -> &str {
    text.trim_end_matches(is_group_address_separator)
}

fn strip_active_keyword_prefix<'a>(text: &'a str, active_keywords: &[String]) -> Option<&'a str> {
    active_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .find_map(|keyword| {
            let rest = text
                .get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                .then(|| text.get(keyword.len()..))
                .flatten()?;
            (rest.is_empty()
                || rest.starts_with('/')
                || rest.starts_with('／')
                || rest.starts_with(is_group_address_separator))
            .then_some(rest)
        })
}

fn is_group_address_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，')
}
