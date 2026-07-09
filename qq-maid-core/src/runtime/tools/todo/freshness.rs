//! Todo 最近可见查询快照 helper。
//!
//! 这些规则会读取并按 owner 过滤 `SessionRecord.last_todo_query`，属于 Todo
//! 领域的编号续指能力，不放在底层 session storage 中。

use crate::runtime::{
    freshness::query_is_fresh,
    session::{LAST_QUERY_TTL_SECONDS, LastTodoQuery, SessionRecord},
};

/// 当前快照是否属于用户可见 Todo 列表。
pub(crate) fn is_visible_todo_query_type(query_type: &str) -> bool {
    matches!(
        query_type,
        "list" | "search" | "due-date" | "all" | "completed-list" | "completed-time"
    )
}

/// 按 owner 和 query_type 条件读取最近 Todo 查询快照；过期时顺手清理旧值。
pub(crate) fn valid_last_todo_query(
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
pub(crate) fn valid_last_visible_todo_query(
    session: &mut SessionRecord,
    owner_key: &str,
) -> Option<LastTodoQuery> {
    valid_last_todo_query(session, owner_key, is_visible_todo_query_type)
}
