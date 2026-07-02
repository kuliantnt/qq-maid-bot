//! 记忆草稿的 JSON 提取、清洗、自动分类与敏感内容判断。
//!
//! LLM 返回的草稿必须是包含 `content` 字段的 JSON；这里负责：
//! - 提取 `content` 字符串并清洗（剥掉 Markdown 围栏、内嵌 JSON、首尾空白）；
//! - 判定草稿是否为无效/不适宜写入长期记忆的占位内容；
//! - 根据草稿内容自动分类记忆类型与范围（默认 note/general）；
//! - 检测疑似密钥、token 等敏感内容，草稿阶段直接拒绝，不入库。
//!
//! 安全边界：含敏感内容的草稿一律返回 `None`/拒绝，长期记忆只能由明确记忆指令生成
//! 草稿并经用户确认后写入；普通聊天不会经过本模块自动写记忆。

use serde_json::Value;

use crate::runtime::{
    respond::{
        common::{clean_string, extract_json_object},
        llm_service::clean_memory_draft_output,
    },
    session::redact_sensitive_text,
};

/// 从 LLM 返回的 JSON 中提取记忆草稿的 content 字段。
fn parse_memory_draft_json_content(raw: &str) -> Option<String> {
    let value = extract_json_object(raw)?;
    let object = value.as_object()?;
    let content = object.get("content")?;
    match content {
        Value::String(value) => sanitize_memory_content(value),
        Value::Null => None,
        _ => None,
    }
}

pub(super) fn parse_valid_memory_draft_content(raw: &str) -> Option<String> {
    let draft = parse_memory_draft_json_content(raw)?;
    if is_invalid_memory_draft(&draft) || contains_sensitive_text(&draft) {
        None
    } else {
        Some(draft)
    }
}

fn sanitize_memory_content(value: &str) -> Option<String> {
    if looks_like_markdown_fence(value) {
        return None;
    }
    let content = clean_memory_draft_output(value);
    if looks_like_embedded_memory_json(&content) {
        return None;
    }
    clean_string(content)
}

fn looks_like_markdown_fence(text: &str) -> bool {
    text.trim_start().starts_with("```")
}

fn looks_like_embedded_memory_json(text: &str) -> bool {
    let text = text.trim();
    text.starts_with('{') && text.contains("\"content\"")
}

/// 根据记忆草稿内容自动分类记忆类型和范围。
/// 返回 (memory_type, scope)，默认 type=note, scope=general。
pub(super) fn classify_memory(_text: &str) -> (String, String) {
    ("note".to_owned(), "general".to_owned())
}

fn is_invalid_memory_draft(text: &str) -> bool {
    matches!(text.trim(), "" | "无" | "不适合写入长期记忆" | "无法整理")
}

/// 草稿阶段检测疑似密钥、token 等敏感内容：脱敏后与原文不一致即视为敏感。
pub(super) fn contains_sensitive_text(text: &str) -> bool {
    redact_sensitive_text(text) != text
}

/// 修订草稿时把用户补充说明追加到原始来源文本，保留可追溯的输入链路。
pub(super) fn append_memory_source_text(existing: &str, user_text: &str) -> String {
    let existing = existing.trim();
    let user_text = user_text.trim();
    if existing.is_empty() {
        user_text.to_owned()
    } else if user_text.is_empty() {
        existing.to_owned()
    } else {
        format!("{existing}\n{user_text}")
    }
}
