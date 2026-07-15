//! Memory 草稿 JSON 提取、清洗、分类与敏感内容判断。

use qq_maid_common::{markdown_strip::strip_markdown_for_chat, redaction::redact_sensitive_text};
use serde_json::Value;

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

pub(crate) fn classify_memory(_text: &str) -> (String, String) {
    ("note".to_owned(), "general".to_owned())
}

/// 草稿阶段检测疑似密钥、token 等敏感内容；普通聊天不会自动进入此写入路径。
pub(crate) fn contains_sensitive_text(text: &str) -> bool {
    redact_sensitive_text(text) != text
}

fn sanitize_memory_content(value: &str) -> Option<String> {
    if value.trim_start().starts_with("```") {
        return None;
    }
    let mut content = strip_markdown_for_chat(value);
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
