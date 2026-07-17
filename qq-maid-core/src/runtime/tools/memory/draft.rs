//! Memory 草稿 JSON 提取、清洗、分类与敏感内容判断。

use qq_maid_common::{markdown::to_chat_text, redaction::redact_sensitive_text};
use serde_json::Value;

use super::{
    PreparedMemoryDraft,
    storage::{MemoryCategory, MemoryKind, MemoryTarget, MemoryVisibility},
};

const MAX_MEMORY_DRAFT_LENGTH: usize = 600;
const MEMORY_PREFIXES: &[&str] = &["记忆草稿", "记忆", "内容", "可写入记忆", "写入内容"];

pub(crate) fn parse_valid_memory_draft_content(raw: &str) -> Option<String> {
    let value = extract_json_object(raw)?;
    let content = value.as_object()?.get("content")?;
    let draft = match content {
        Value::String(value) => sanitize_memory_content(value)?,
        Value::Null => return None,
        _ => return None,
    };
    if is_invalid_memory_draft(&draft) || contains_sensitive_text(&draft) {
        None
    } else {
        Some(draft)
    }
}

pub(crate) fn normalize_explicit_memory_content(raw: &str) -> Option<String> {
    let content = sanitize_memory_content(raw)?;
    (!is_invalid_memory_draft(&content) && !contains_sensitive_text(&content)).then_some(content)
}

/// 草稿阶段检测疑似密钥、token 等敏感内容；普通聊天不会自动进入此写入路径。
pub(crate) fn contains_sensitive_text(text: &str) -> bool {
    if redact_sensitive_text(text) != text {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    if [
        "身份证",
        "护照号",
        "银行卡",
        "账号密码",
        "登录密码",
        "支付密码",
        "api key",
        "apikey",
        "credential",
        "private key",
        "access token",
        "refresh token",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    let chars = text.chars().collect::<Vec<_>>();
    chars.windows(18).any(|window| {
        window[..17].iter().all(|ch| ch.is_ascii_digit())
            && (window[17].is_ascii_digit() || matches!(window[17], 'x' | 'X'))
    })
}

pub(crate) fn prepare_memory_draft(
    target: MemoryTarget,
    content: String,
    source_text: String,
    source_ref: Option<String>,
    change_type: &str,
) -> PreparedMemoryDraft {
    let (category, attribute_key) = classify_draft(&content, target.memory_kind());
    let visibility = match target.memory_kind() {
        MemoryKind::Personal => MemoryVisibility::Private,
        MemoryKind::GroupProfile | MemoryKind::Group => MemoryVisibility::GroupMembers,
        MemoryKind::LegacyUnassigned => MemoryVisibility::Private,
    };
    PreparedMemoryDraft {
        target,
        visibility,
        category,
        content,
        source_text,
        source_summary: "用户通过明确记忆指令提交".to_owned(),
        change_type: change_type.to_owned(),
        attribute_key,
        source_ref,
    }
}

fn classify_draft(content: &str, kind: MemoryKind) -> (MemoryCategory, Option<String>) {
    if ["叫我", "称呼", "昵称"]
        .iter()
        .any(|value| content.contains(value))
    {
        return (MemoryCategory::Identity, Some("nickname".to_owned()));
    }
    if ["身份", "角色", "人设"]
        .iter()
        .any(|value| content.contains(value))
    {
        return (MemoryCategory::Identity, Some("identity".to_owned()));
    }
    if ["喜欢", "不喜欢", "偏好", "希望你回复"]
        .iter()
        .any(|value| content.contains(value))
    {
        return (MemoryCategory::Preference, None);
    }
    if kind == MemoryKind::Group
        && ["群规", "约定", "每周", "公告"]
            .iter()
            .any(|value| content.contains(value))
    {
        return (MemoryCategory::Instruction, None);
    }
    (MemoryCategory::Note, None)
}

fn sanitize_memory_content(value: &str) -> Option<String> {
    if value.trim_start().starts_with("```") {
        return None;
    }
    let mut content = to_chat_text(value);
    content = content.trim().trim_matches('。').trim().to_owned();
    for prefix in MEMORY_PREFIXES {
        if let Some(rest) = content.strip_prefix(prefix) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix(['：', ':']) {
                content = rest.trim().to_owned();
                break;
            }
        }
    }
    if content.trim_start().starts_with('{') && content.contains("\"content\"") {
        return None;
    }
    if content.chars().count() > MAX_MEMORY_DRAFT_LENGTH {
        content = content
            .chars()
            .take(MAX_MEMORY_DRAFT_LENGTH)
            .collect::<String>()
            .trim_end()
            .to_owned();
    }
    (!content.is_empty()).then_some(content)
}

fn extract_json_object(raw: &str) -> Option<Value> {
    let text = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    if let Some(fenced) = strip_outer_json_fence(text)
        && let Ok(value) = serde_json::from_str::<Value>(fenced)
    {
        return Some(value);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str::<Value>(&text[start..=end]).ok())
        .flatten()
}

fn strip_outer_json_fence(text: &str) -> Option<&str> {
    let body = text.strip_prefix("```")?;
    let body = body.strip_prefix("json").unwrap_or(body).trim_start();
    body.strip_suffix("```").map(str::trim)
}

fn is_invalid_memory_draft(text: &str) -> bool {
    matches!(text.trim(), "" | "无" | "不适合写入长期记忆" | "无法整理")
}
