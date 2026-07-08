//! 最近查询快照新鲜度与可见性 helper。
//!
//! 这里只读 `SessionRecord` 上的最近 Todo 查询快照，判断是否仍在 TTL 内、是否
//! 属于用户可见列表；过期值顺手清理。函数语义保持与拆分前一致，不改变持久化
//! 数据格式或最近快照的存储字段。

use chrono::{DateTime, Duration};

use super::{LAST_QUERY_TTL_SECONDS, LastTodoQuery, SessionRecord};
use qq_maid_common::time_context;

/// 判断一条“最近查询”记录是否仍在有效期内（created_at 为 RFC3339，TTL 单位为秒）。
pub fn query_is_fresh(created_at: &str, ttl_seconds: i64) -> bool {
    let Ok(created_at) = DateTime::parse_from_rfc3339(created_at.trim()) else {
        return false;
    };
    let Ok(now) = DateTime::parse_from_rfc3339(&super::now_iso_cn()) else {
        return false;
    };
    let age = now.signed_duration_since(created_at.with_timezone(&time_context::shanghai_offset()));
    age >= Duration::zero() && age.num_seconds() <= ttl_seconds
}

/// 当前快照是否属于用户可见 Todo 列表。
pub fn is_visible_todo_query_type(query_type: &str) -> bool {
    matches!(
        query_type,
        "list"
            | "search"
            | "due-date"
            | "all"
            | "completed-list"
            | "completed-time"
            | "cancelled-list"
    )
}

/// 按 owner 和 query_type 条件读取最近 Todo 查询快照；过期时顺手清理旧值。
pub fn valid_last_todo_query(
    session: &mut SessionRecord,
    owner_key: &str,
    query_type_matches: impl Fn(&str) -> bool,
) -> Option<LastTodoQuery> {
    let query = session.last_todo_query.clone()?;
    if query.owner_key != owner_key || !query_type_matches(&query.query_type) {
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_todo_query = None;
        return None;
    }
    Some(query)
}

/// 读取最近一次仍可供用户按编号续指的 Todo 列表快照。
pub fn valid_last_visible_todo_query(
    session: &mut SessionRecord,
    owner_key: &str,
) -> Option<LastTodoQuery> {
    valid_last_todo_query(session, owner_key, is_visible_todo_query_type)
}
