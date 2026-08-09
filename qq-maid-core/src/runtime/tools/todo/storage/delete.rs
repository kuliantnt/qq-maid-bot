//! Todo 删除存储入口。
//!
//! 普通 owner 删除与群管理员删除都在这里维护各自的 SQL 授权条件，避免大体量
//! `storage/mod.rs` 继续增长，也让跨创建者群管理路径不会误放宽个人删除接口。

use rusqlite::{OptionalExtension, params};

use crate::{
    identity::group_raw_target_from_scope_key,
    runtime::tools::todo::reminder::TODO_REMINDER_SOURCE,
    storage::{notification::NotificationStatus, session::now_iso_cn},
};

use super::{
    TodoBulkDeleteOutcome, TodoError, TodoItem, TodoOwner, TodoStatus, TodoStore,
    id::{clean_todo_id, parse_todo_db_id},
    query::{query_pending_items_by_group_scope, todo_item_from_row},
    sort::sort_todos,
};

impl TodoStore {
    /// 物理删除已完成待办事项（按 ID 列表匹配）。
    pub fn delete_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        self.delete_by_ids_with_status(owner, ids, TodoStatus::Completed)
    }

    /// 物理删除进行中的待办事项（按 ID 列表匹配）。
    ///
    /// 删除确认只能删除发起确认时仍处于 Pending 的记录；如果确认期间记录状态变化，
    /// 这里会按 skipped 处理，避免过期确认越过用户当前状态授权。
    pub fn delete_pending_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        self.delete_by_ids_with_status(owner, ids, TodoStatus::Pending)
    }

    /// 按 ID 物理删除任意状态的待办事项。
    pub fn delete_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut deleted_count = 0usize;
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "DELETE FROM todos
                     WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3",
                    params![id, owner.key.as_str(), owner.scope_key.as_str()],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else {
                deleted_count += affected;
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkDeleteOutcome {
            deleted_count,
            skipped_ids,
        })
    }

    /// 列出会投递到指定群 conversation scope 的所有未完成 Todo。
    pub(crate) fn list_pending_for_group_scope(
        &self,
        scope_key: &str,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let scope_key = validate_group_scope(scope_key)?;
        let conn = self.connection()?;
        let mut items = query_pending_items_by_group_scope(&conn, scope_key)?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 原子取消提醒并按内部 ID 删除指定群 scope 中仍为未完成状态的 Todo。
    ///
    /// 列表编号只负责解析出候选 ID；真正删除时在同一 SQL 事务内再次校验完整群
    /// scope 与 pending 状态。Notification Outbox 取消失败会回滚 Todo 删除，避免记录
    /// 已消失但旧提醒仍可投递。
    pub(crate) fn delete_pending_for_group_scope_and_cancel_notification(
        &self,
        scope_key: &str,
        item_id: &str,
    ) -> Result<Option<TodoItem>, TodoError> {
        self.delete_pending_for_group_scope_inner(scope_key, item_id, false)
    }

    #[cfg(test)]
    pub(crate) fn delete_group_todo_with_cancel_failure_for_test(
        &self,
        scope_key: &str,
        item_id: &str,
    ) -> Result<Option<TodoItem>, TodoError> {
        self.delete_pending_for_group_scope_inner(scope_key, item_id, true)
    }

    fn delete_pending_for_group_scope_inner(
        &self,
        scope_key: &str,
        item_id: &str,
        fail_after_cancel_for_test: bool,
    ) -> Result<Option<TodoItem>, TodoError> {
        let scope_key = validate_group_scope(scope_key)?;
        let Some(item_id) = parse_todo_db_id(&clean_todo_id(item_id)) else {
            return Ok(None);
        };
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let item = tx
            .query_row(
                "SELECT id, user_id, scope_key, title, detail, raw_text,
                        due_date, due_at, reminder_at, time_precision, recurrence_kind,
                        recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                        created_at, updated_at, completed_at
                 FROM todos
                 WHERE id = ?1 AND scope_key = ?2 AND status = ?3",
                params![item_id, scope_key, TodoStatus::Pending.as_str()],
                todo_item_from_row,
            )
            .optional()
            .map_err(TodoError::from_sql)?;
        let Some(item) = item else {
            tx.commit().map_err(TodoError::from_sql)?;
            return Ok(None);
        };
        let now = now_iso_cn();
        tx.execute(
            "UPDATE notification_outbox
             SET status = ?3,
                 updated_at = ?4,
                 cancelled_at = ?4,
                 locked_by = NULL,
                 locked_at = NULL
             WHERE source_type = ?1
               AND source_id = ?2
               AND status IN ('pending', 'retry', 'sending', 'failed')",
            params![
                TODO_REMINDER_SOURCE,
                item.id.as_str(),
                NotificationStatus::Cancelled.as_str(),
                now,
            ],
        )
        .map_err(TodoError::from_sql)?;
        #[cfg(test)]
        if fail_after_cancel_for_test {
            return Err(TodoError::io("injected notification cancellation failure"));
        }
        #[cfg(not(test))]
        let _ = fail_after_cancel_for_test;
        let affected = tx
            .execute(
                "DELETE FROM todos
                 WHERE id = ?1 AND scope_key = ?2 AND status = ?3",
                params![item_id, scope_key, TodoStatus::Pending.as_str()],
            )
            .map_err(TodoError::from_sql)?;
        tx.commit().map_err(TodoError::from_sql)?;
        Ok((affected == 1).then_some(item))
    }

    /// 按指定终态物理删除记录，并在事务内校验 owner、scope 和 status。
    fn delete_by_ids_with_status(
        &self,
        owner: &TodoOwner,
        ids: &[String],
        status: TodoStatus,
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut deleted_count = 0usize;
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "DELETE FROM todos
                     WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3 AND status = ?4",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        status.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else {
                deleted_count += affected;
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkDeleteOutcome {
            deleted_count,
            skipped_ids,
        })
    }
}

fn validate_group_scope(scope_key: &str) -> Result<&str, TodoError> {
    let scope_key = scope_key.trim();
    if group_raw_target_from_scope_key(scope_key).is_none() {
        return Err(TodoError::bad_request("群 Todo 管理需要有效的群作用域"));
    }
    Ok(scope_key)
}
