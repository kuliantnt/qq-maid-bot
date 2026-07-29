use qq_maid_core::runtime::push::{PushMention, PushTargetType, QQ_OFFICIAL_PLATFORM};
use tracing::warn;

use crate::gateway::platform::qq_official::member_mention;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PreparedQqBot2Content {
    /// 实际提交给 QQ API 的 Markdown 或文本内容。
    pub content: String,
    /// Markdown 失败时实际提交给 QQ API 的纯文本内容。
    pub fallback_content: String,
    /// QQ 客户端实际可见内容的安全索引表示，不包含成员敏感 ID。
    pub ref_index_content: String,
    /// 文本 fallback 对应的安全索引表示，不包含成员敏感 ID。
    pub fallback_ref_index_content: String,
}

impl PreparedQqBot2Content {
    fn unchanged(text: &str, fallback_text: &str) -> Self {
        Self {
            content: text.to_owned(),
            fallback_content: fallback_text.to_owned(),
            ref_index_content: text.to_owned(),
            fallback_ref_index_content: fallback_text.to_owned(),
        }
    }
}

pub(super) fn partition_onebot_mentions(
    mentions: &[PushMention],
) -> (Vec<String>, Vec<PushMention>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for mention in mentions {
        if mention.user_id.bytes().all(|byte| byte.is_ascii_digit())
            && mention.user_id.parse::<u64>().is_ok()
        {
            valid.push(mention.user_id.clone());
        } else {
            invalid.push(mention.clone());
        }
    }
    (valid, invalid)
}

pub(super) fn mention_display_names(mentions: &[PushMention]) -> Vec<String> {
    mentions
        .iter()
        .filter_map(|mention| mention.display_name.as_deref())
        .filter_map(safe_display_name)
        .collect()
}

fn safe_display_name(value: &str) -> Option<String> {
    let value = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = value.chars().take(64).collect::<String>();
    (!value.is_empty()).then_some(value)
}

pub(super) fn prepend_mention_notice(
    text: &str,
    display_names: &[String],
    markdown: bool,
) -> String {
    if display_names.is_empty() {
        return text.to_owned();
    }
    let names = display_names
        .iter()
        .map(|name| {
            if markdown {
                qq_maid_common::markdown::escape_inline(name)
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("、");
    format!("提醒成员：{names}\n\n{text}")
}

pub(super) fn prepare_qq_bot2_content(
    target_type: PushTargetType,
    mentions: &[PushMention],
    text: &str,
    fallback_text: &str,
    _message_type: &str,
) -> PreparedQqBot2Content {
    if mentions.is_empty() {
        return PreparedQqBot2Content::unchanged(text, fallback_text);
    }
    if target_type == PushTargetType::Private {
        warn!(
            platform = QQ_OFFICIAL_PLATFORM,
            mention_count = mentions.len(),
            "push mentions ignored because private messages do not support group member mention"
        );
        return PreparedQqBot2Content::unchanged(text, fallback_text);
    }

    // QQ 官方群消息通过 content/markdown.content 中的 `<@user_id>` 表达原生成员 @；
    // 请求体没有独立 mentions 字段并不代表平台不支持。这里与被动群回复共用格式。
    let prefix = mentions
        .iter()
        .filter_map(|mention| member_mention(&mention.user_id))
        .collect::<Vec<_>>()
        .join(" ");
    if prefix.is_empty() {
        return PreparedQqBot2Content::unchanged(text, fallback_text);
    }

    PreparedQqBot2Content {
        content: format!("{prefix}\n{text}"),
        fallback_content: format!("{prefix}\n{fallback_text}"),
        // QQ 客户端会把协议标记渲染为昵称；Gateway 没有安全昵称时只索引正文，
        // 避免引用索引持久化或回显原始成员 ID。
        ref_index_content: text.to_owned(),
        fallback_ref_index_content: fallback_text.to_owned(),
    }
}
