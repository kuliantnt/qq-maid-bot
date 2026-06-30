//! 待办 ID 解析与 private scope 解析 helper。
//!
//! 用户侧使用 `[id]` / `#id` 等标记，数据库内部使用自增整数；这里负责把用户输入
//! 清理并解析为数据库 ID。`private_target_from_scope_key` 用于 reminder 从
//! `private:` 前缀的 scope_key 中提取可推送的私聊目标，保持 reminder 层不直接
//! 依赖 scope 字符串细节。

use super::{TodoError, clean_optional};

/// 清理待办 ID：去除首尾空格和括号标记。
pub(super) fn clean_todo_id(value: &str) -> String {
    value
        .trim()
        .trim_matches(&['[', ']', '#', ' ', '\t', '\n', '\r'][..])
        .to_owned()
}

/// 把用户输入解析为数据库内部整数 ID，仅接受正数。
pub(super) fn parse_todo_db_id(value: &str) -> Option<i64> {
    clean_todo_id(value)
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
}

/// 解析必填 ID，无法解析时按 not_found 报错，避免与“查到了但状态不匹配”混淆。
pub(super) fn parse_required_todo_db_id(value: &str) -> Result<i64, TodoError> {
    parse_todo_db_id(value).ok_or_else(|| TodoError::not_found("todo not found"))
}

/// 从 `private:<target>` 形式的 scope_key 中提取私聊推送目标。
pub(super) fn private_target_from_scope_key(value: &str) -> Option<String> {
    value.strip_prefix("private:").and_then(clean_optional)
}
