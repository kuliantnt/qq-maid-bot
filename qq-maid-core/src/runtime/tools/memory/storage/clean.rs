//! 长期记忆 storage 字段清洗与作用域身份推断 helper。
//!
//! 集中维护 `trim`/空值归一、必填校验与 legacy 作用域身份推断，供 `MemoryStore`
//! 与其它 helper 复用。这里不改变权限/兼容旧数据语义：legacy 记忆只有在能
//! 证明归属（user_id / group_id）时才归入 personal/group，否则放入
//! `legacy_unassigned`，避免把无法证明归属的数据暴露给任意用户。

use qq_maid_common::redaction::redact_sensitive_text;

use super::{MemoryError, MemoryScopeType};

/// 清理并验证必填字段：去除首尾空格，空值则返回错误。
pub(super) fn clean_required(value: String, field: &str) -> Result<String, MemoryError> {
    clean_optional(value).ok_or_else(|| MemoryError::bad_request(format!("{field} is required")))
}

/// 清理可选字段：去除首尾空格，空值返回 None。
pub(super) fn clean_optional(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// 把 `&str` 版本的可选字段清理为 `Option<String>`。
pub(super) fn clean_optional_str(value: &str) -> Option<String> {
    clean_optional(value.to_owned())
}

/// 清理可选 Option 字段：内层值空则返回 None。
pub(super) fn clean_optional_option(value: Option<String>) -> Option<String> {
    value.and_then(clean_optional)
}

/// 校验作用域 ID 非空，避免越权查询时把空串当作通用作用域。
pub(super) fn clean_scope_id(value: &str) -> Result<String, MemoryError> {
    clean_optional(value.to_owned()).ok_or_else(|| MemoryError::bad_request("scope_id is required"))
}

/// 清理结构化属性键。只允许稳定的 ASCII 标识符，避免把自然语言内容塞进冲突键。
pub(super) fn clean_attribute_key(value: Option<String>) -> Result<Option<String>, MemoryError> {
    let Some(value) = clean_optional_option(value) else {
        return Ok(None);
    };
    if value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(MemoryError::bad_request("invalid memory attribute_key"));
    }
    Ok(Some(value.to_ascii_lowercase()))
}

/// 安全来源引用只允许单行短标识；原始消息正文继续放在已脱敏的 `source_text`。
pub(super) fn clean_source_ref(value: Option<String>) -> Result<Option<String>, MemoryError> {
    let Some(value) = clean_optional_option(value) else {
        return Ok(None);
    };
    if value.len() > 256
        || redact_sensitive_text(&value) != value
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b':' | b'/' | b'?' | b'&' | b'=' | b'.' | b'_' | b'-' | b'#' | b'@' | b'%'
                )
        })
    {
        return Err(MemoryError::bad_request("invalid memory source_ref"));
    }
    Ok(Some(value))
}

/// 清理可选稳定身份作用域；空串不能成为画像或关系主体。
pub(super) fn clean_stable_identity(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, MemoryError> {
    let value = clean_optional_option(value);
    if value.as_deref().is_some_and(|value| value.len() > 512) {
        return Err(MemoryError::bad_request(format!("{field} is too long")));
    }
    Ok(value)
}

/// 默认记忆类型。
pub(super) fn default_memory_type() -> String {
    "note".to_owned()
}

/// 默认记忆作用域（业务分类，非权限边界）。
pub(super) fn default_scope() -> String {
    "general".to_owned()
}

/// `MemoryRecord::scope_type` 的 legacy 兼容默认值。
pub(super) fn legacy_unassigned_scope_type() -> String {
    MemoryScopeType::LegacyUnassigned.as_str().to_owned()
}

/// 根据旧 `user_id` / `group_id` 推导 legacy 作用域身份。
///
/// 优先个人维度。仅当能证明归属时才归入 personal/group，否则归入
/// `legacy_unassigned` 和占位用户，保证旧记录不会被任意用户读取。
#[cfg(test)]
pub(super) fn infer_legacy_scope_identity(
    user_id: Option<&str>,
    group_id: Option<&str>,
) -> (MemoryScopeType, String, String) {
    if let Some(user_id) = user_id.and_then(clean_optional_str) {
        return (MemoryScopeType::Personal, user_id.clone(), user_id);
    }
    if let Some(group_id) = group_id.and_then(clean_optional_str) {
        return (
            MemoryScopeType::Group,
            group_id,
            "legacy_unknown_user".to_owned(),
        );
    }
    (
        MemoryScopeType::LegacyUnassigned,
        "legacy_unassigned".to_owned(),
        "legacy_unknown_user".to_owned(),
    )
}
