//! QQ 全量群消息中的当前机器人 mention 归一化。
//!
//! 该模块只处理结构化身份确认和当前正文同步，不参与群消息模式、冷却或 Core 业务判断。

use qq_maid_common::input_part::MessageInputPart;

use crate::gateway::{
    bot_identity::SharedBotIdentity,
    event::{GroupEventType, GroupMessage},
};

/// 在群消息进入过滤、命令和 Core adapter 前统一解析“是否 @ 当前机器人”。
/// `is_current_bot` 是 Gateway 内部语义。QQ 提供的当前机器人标记是正证据；普通 mention
/// 的 target_id 命中 READY 或配置身份时也可补充确认，避免只依赖昵称或任意文本。
pub(crate) fn normalize_current_bot_mentions(
    message: &mut GroupMessage,
    bot_identity: &SharedBotIdentity,
) {
    for mention in &mut message.mentions {
        let marked_by_event = mention.is_current_bot;
        if let Some(target_id) = mention.target_id.as_deref() {
            // QQ 某些全量群事件的 member_openid 不在 READY 身份候选中；已有的官方
            // 当前机器人标记不能被一次本地身份集合 miss 覆盖，否则 @ 会静默丢失。
            mention.is_current_bot = marked_by_event || bot_identity.contains(target_id);
        }
    }
    if message.event_type != GroupEventType::GroupAtMessage {
        strip_current_bot_markdown_mentions(message);
    }
}

/// 从全量群消息的当前正文中移除已由结构化 mentions 确认的当前机器人 Markdown mention。
///
/// QQ 的 `GROUP_AT_MESSAGE_CREATE` 已经移除了机器人 mention，不能在这里再次处理。普通
/// `GROUP_MESSAGE_CREATE` 只按 Markdown mention token 与结构化 mentions 的顺序对应；两者
/// 数量不一致时放弃清理，避免把普通成员正文或任意 Markdown 链接误删。
fn strip_current_bot_markdown_mentions(message: &mut GroupMessage) {
    let tokens = markdown_mention_tokens(&message.content);
    if tokens.is_empty() || tokens.len() != message.mentions.len() {
        return;
    }

    let mut spans = tokens
        .into_iter()
        .zip(&message.mentions)
        .filter_map(|(token, mention)| mention.is_current_bot.then_some(token))
        .collect::<Vec<_>>();
    if spans.is_empty() {
        return;
    }

    let mut cleaned = message.content.clone();
    for (start, end) in spans.drain(..).rev() {
        let end = consume_following_whitespace(&cleaned, end);
        cleaned.replace_range(start..end, "");
    }
    let cleaned = cleaned.trim().to_owned();
    message.content = cleaned.clone();
    sync_top_level_text_part(&mut message.input_parts, cleaned);
}

fn sync_top_level_text_part(input_parts: &mut Vec<MessageInputPart>, content: String) {
    let text_index = input_parts
        .iter()
        .position(|part| matches!(part, MessageInputPart::Text { .. }));
    match (text_index, content.is_empty()) {
        (Some(index), true) => {
            input_parts.remove(index);
        }
        (Some(index), false) => {
            if let MessageInputPart::Text { text, .. } = &mut input_parts[index] {
                *text = content;
            }
        }
        (None, false) => input_parts.insert(0, MessageInputPart::text(content)),
        (None, true) => {}
    }
}

fn markdown_mention_tokens(content: &str) -> Vec<(usize, usize)> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = content[cursor..].find("[@") {
        let start = cursor + relative_start;
        let Some(label_end) = content[start..].find("](").map(|offset| start + offset) else {
            break;
        };
        let url_start = label_end + 2;
        let Some(relative_end) = content[url_start..].find(')') else {
            break;
        };
        let end = url_start + relative_end + 1;
        if is_qq_markdown_mention_uri(&content[url_start..end - 1]) {
            tokens.push((start, end));
            cursor = end;
        } else {
            cursor = start + 2;
        }
    }
    tokens
}

fn is_qq_markdown_mention_uri(uri: &str) -> bool {
    let Some(query) = uri.strip_prefix("mqqapi://markdown/mention?") else {
        return false;
    };
    let has_at_type = query.split('&').any(|item| item == "at_type=1");
    let has_target = query.split('&').any(|item| {
        item.strip_prefix("at_tinyid=")
            .is_some_and(|value| !value.trim().is_empty())
    });
    has_at_type && has_target
}

fn consume_following_whitespace(text: &str, mut end: usize) -> usize {
    while end < text.len() {
        let Some(ch) = text[end..].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        end += ch.len_utf8();
    }
    end
}
