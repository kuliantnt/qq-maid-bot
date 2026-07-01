//! 长期记忆字段清洗与作用域身份推断 helper。
//!
//! 集中维护 `trim`/空值归一、必填校验与 legacy 作用域身份推断，供 `MemoryStore`
//! 与其它 helper 复用。这里不改变权限/兼容旧数据语义：legacy 记忆只有在能
//! 证明归属（user_id / group_id）时才归入 personal/group，否则放入
//! `legacy_unassigned`，避免把无法证明归属的数据暴露给任意用户。

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
