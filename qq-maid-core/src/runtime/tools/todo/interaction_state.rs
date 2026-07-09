//! Todo 交互状态探测门面。
//!
//! 最近可见列表、最近操作和 owner 过滤属于 Todo 编号续指能力。respond 层只消费
//! 这里返回的布尔快照，不直接读取 Todo session 字段或 owner 细节。

use crate::runtime::{
    freshness::query_is_fresh,
    respond::RespondRequest,
    session::{LAST_QUERY_TTL_SECONDS, SessionRecord},
};

use super::{TodoStore, valid_last_visible_todo_query, visible_snapshot_has_todo_items};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TodoInteractionSnapshot {
    pub(crate) has_visible_snapshot: bool,
    pub(crate) has_recent_operation: bool,
}

pub(crate) fn snapshot_for_request(
    req: &RespondRequest,
    active_session: Option<&SessionRecord>,
) -> TodoInteractionSnapshot {
    let request_visible_snapshot =
        visible_snapshot_has_todo_items(req.visible_entity_snapshot.as_ref());
    let Some(session) = active_session else {
        return TodoInteractionSnapshot {
            has_visible_snapshot: request_visible_snapshot,
            has_recent_operation: false,
        };
    };

    let owner = TodoStore::owner(req.user_id.as_deref(), &req.scope_key);
    let mut snapshot = session.clone();
    let session_visible_snapshot = valid_last_visible_todo_query(&mut snapshot, &owner.key)
        .is_some_and(|query| !query.result_ids.is_empty());
    let has_recent_operation = session.last_todo_action.as_ref().is_some_and(|action| {
        action.owner_key == owner.key && query_is_fresh(&action.created_at, LAST_QUERY_TTL_SECONDS)
    });

    TodoInteractionSnapshot {
        has_visible_snapshot: request_visible_snapshot || session_visible_snapshot,
        has_recent_operation,
    }
}
