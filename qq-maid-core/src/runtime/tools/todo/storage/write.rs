//! Todo 表写入 helper。
//!
//! `TodoStore` 公开方法仍保留在 `mod.rs`，这里仅放无锁 INSERT/UPDATE helper，
//! 让主入口文件更聚焦在事务边界和业务方法。

use rusqlite::{Connection, params};

use super::{TodoError, TodoItem, TodoItemDraft, TodoOwner, TodoStatus, query::get_by_id_unlocked};

pub(super) fn insert_todo_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    draft: TodoItemDraft,
    now: &str,
) -> Result<TodoItem, TodoError> {
    conn.execute(
        "INSERT INTO todos (
            owner_key, user_id, scope_key, title, detail, raw_text,
            due_date, due_at, reminder_at, time_precision, recurrence_kind,
            recurrence_interval_days, recurrence_interval, recurrence_unit,
            status, completed, created_at, updated_at,
            completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16, ?17, NULL)",
        params![
            owner.key.as_str(),
            owner.user_id.as_deref(),
            owner.scope_key.as_str(),
            draft.title,
            draft.detail,
            draft.raw_text,
            draft.due_date,
            draft.due_at,
            draft.reminder_at,
            draft.time_precision.as_str(),
            draft.recurrence_kind.as_str(),
            i64::from(draft.recurrence_interval_days),
            i64::from(draft.recurrence_interval),
            draft.recurrence_unit.as_str(),
            TodoStatus::Pending.as_str(),
            now,
            now,
        ],
    )
    .map_err(TodoError::from_sql)?;
    let id = conn.last_insert_rowid();
    get_by_id_unlocked(conn, owner, id)?
        .ok_or_else(|| TodoError::io("todo disappeared after insert"))
}

pub(super) fn complete_pending_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    now: &str,
) -> Result<Option<TodoItem>, TodoError> {
    let affected = conn
        .execute(
            "UPDATE todos
             SET status = ?4,
                 completed = 1,
                 updated_at = ?5,
                 completed_at = ?5
             WHERE id = ?1
               AND owner_key = ?2
               AND scope_key = ?3
               AND status = ?6",
            params![
                id,
                owner.key.as_str(),
                owner.scope_key.as_str(),
                TodoStatus::Completed.as_str(),
                now,
                TodoStatus::Pending.as_str(),
            ],
        )
        .map_err(TodoError::from_sql)?;
    if affected == 0 {
        return Ok(None);
    }
    get_by_id_unlocked(conn, owner, id)?
        .map(Some)
        .ok_or_else(|| TodoError::io("todo disappeared after complete"))
}

pub(super) fn update_pending_todo_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    draft: TodoItemDraft,
    now: &str,
) -> Result<Option<TodoItem>, TodoError> {
    let affected = conn
        .execute(
            "UPDATE todos
             SET title = ?4,
                 detail = ?5,
                 raw_text = ?6,
                 due_date = ?7,
                 due_at = ?8,
                 reminder_at = ?9,
                 time_precision = ?10,
                 recurrence_kind = ?11,
                 recurrence_interval_days = ?12,
                 recurrence_interval = ?13,
                 recurrence_unit = ?14,
                 updated_at = ?15
             WHERE id = ?1
               AND owner_key = ?2
               AND scope_key = ?3
               AND status = ?16",
            params![
                id,
                owner.key.as_str(),
                owner.scope_key.as_str(),
                draft.title,
                draft.detail,
                draft.raw_text,
                draft.due_date,
                draft.due_at,
                draft.reminder_at,
                draft.time_precision.as_str(),
                draft.recurrence_kind.as_str(),
                i64::from(draft.recurrence_interval_days),
                i64::from(draft.recurrence_interval),
                draft.recurrence_unit.as_str(),
                now,
                TodoStatus::Pending.as_str(),
            ],
        )
        .map_err(TodoError::from_sql)?;
    if affected == 0 {
        return Ok(None);
    }
    get_by_id_unlocked(conn, owner, id)?
        .map(Some)
        .ok_or_else(|| TodoError::io("todo disappeared after edit"))
}
