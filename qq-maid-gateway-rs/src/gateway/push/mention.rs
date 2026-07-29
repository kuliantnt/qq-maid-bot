use qq_maid_core::runtime::push::{PushMention, PushTargetType, QQ_OFFICIAL_PLATFORM};
use tracing::warn;

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
    message_type: &str,
) -> (String, String) {
    if mentions.is_empty() {
        return (text.to_owned(), fallback_text.to_owned());
    }
    if target_type == PushTargetType::Private {
        warn!(
            platform = QQ_OFFICIAL_PLATFORM,
            mention_count = mentions.len(),
            "push mentions ignored because private messages do not support group member mention"
        );
        return (text.to_owned(), fallback_text.to_owned());
    }

    // QQ Bot 2.0 当前群消息请求体没有成员 mention 字段，官方列出的可发送类型中也没有
    // at 消息；因此不能拼接 openid 伪造原生提醒，只在有安全昵称时显式降级展示。
    warn!(
        platform = QQ_OFFICIAL_PLATFORM,
        mention_count = mentions.len(),
        "push mentions downgraded because QQ Bot 2.0 does not expose outbound member mention"
    );
    let names = mention_display_names(mentions);
    (
        prepend_mention_notice(text, &names, message_type == "markdown"),
        prepend_mention_notice(fallback_text, &names, false),
    )
}
