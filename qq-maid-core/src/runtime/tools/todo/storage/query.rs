//! Todo 表查询与行映射。
//!
//! 这里集中维护 SELECT 语句拼装和 rusqlite 行到 `TodoItem` 的映射；写操作
//! （INSERT/UPDATE/DELETE）仍保留在 `TodoStore` 实现内，便于直接看到事务边界。
//!
//! 行映射中 `time_precision` / `status` 列使用项目自定义枚举，反向解析失败时
//! 需要携带列索引封装成 rusqlite 错误，方便上层按数据异常归类。

use rusqlite::{Connection, OptionalExtension, params, types::Type};

use super::{
    TodoError, TodoItem, TodoOwner, TodoRecurrenceKind, TodoRecurrenceUnit, TodoStatus,
    TodoTimePrecision,
};

/// 列出 owner + scope 下的全部待办（不限状态）。
pub(super) fn query_items(
    conn: &Connection,
    owner: &TodoOwner,
) -> Result<Vec<TodoItem>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, reminder_at, time_precision, recurrence_kind,
                    recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                    created_at, updated_at, completed_at
             FROM todos
             WHERE owner_key = ?1 AND scope_key = ?2",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(
            params![owner.key.as_str(), owner.scope_key.as_str()],
            todo_item_from_row,
        )
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

/// 列出指定状态的待办。
pub(super) fn query_items_by_status(
    conn: &Connection,
    owner: &TodoOwner,
    status: TodoStatus,
) -> Result<Vec<TodoItem>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, reminder_at, time_precision, recurrence_kind,
                    recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                    created_at, updated_at, completed_at
             FROM todos
             WHERE owner_key = ?1 AND scope_key = ?2 AND status = ?3",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(
            params![
                owner.key.as_str(),
                owner.scope_key.as_str(),
                status.as_str()
            ],
            todo_item_from_row,
        )
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

/// 按 owner_key + 一组私聊 scope 读取指定状态的待办。
///
/// reminder 按某个 owner 查 pending 时可能命中该 owner 名下的多个历史 private scope，
/// 这里一次性传入 scope 列表并通过 `IN (...)` 占位符拼装。
pub(super) fn query_items_by_owner_scopes_and_status(
    conn: &Connection,
    owner_key: &str,
    scope_keys: &[String],
    status: TodoStatus,
) -> Result<Vec<TodoItem>, TodoError> {
    let placeholders = std::iter::repeat_n("?", scope_keys.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, reminder_at, time_precision, recurrence_kind,
                recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                created_at, updated_at, completed_at
         FROM todos
         WHERE owner_key = ? AND status = ? AND scope_key IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql).map_err(TodoError::from_sql)?;
    let status = status.as_str();
    let mut params = Vec::with_capacity(scope_keys.len() + 2);
    params.push(owner_key);
    params.push(status);
    params.extend(scope_keys.iter().map(String::as_str));
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), todo_item_from_row)
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

/// 查询 pending 且 scope_key 是私聊归属的 (owner_key, scope_key) 配对，
/// 供 reminder 聚合 owner 与私聊目标的对应关系。
#[cfg(test)]
pub(super) fn query_private_pending_owner_scopes(
    conn: &Connection,
) -> Result<Vec<(String, String)>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT owner_key, scope_key
             FROM todos
             WHERE status = ?1
               AND (
                   scope_key LIKE 'private:%'
                   OR scope_key LIKE 'platform:%:private:%'
               )
             ORDER BY owner_key ASC, scope_key ASC",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(params![TodoStatus::Pending.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

/// 查询已开启每日摘要且存在 pending Todo 的私聊 (owner_key, scope_key) 配对。
pub(super) fn query_private_daily_reminder_owner_scopes(
    conn: &Connection,
) -> Result<Vec<(String, String)>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT t.owner_key, t.scope_key
             FROM todos t
             JOIN todo_daily_reminder_prefs p
               ON p.owner_key = t.owner_key AND p.scope_key = t.scope_key
             WHERE t.status = ?1
               AND p.enabled = 1
               AND (
                   t.scope_key LIKE 'private:%'
                   OR t.scope_key LIKE 'platform:%:private:%'
               )
             ORDER BY t.owner_key ASC, t.scope_key ASC",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(params![TodoStatus::Pending.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

/// 按内部 ID 读取任意 owner/scope 下的待办，仅供内部后台任务根据可信 source_id 回查。
pub(super) fn get_by_id_any_unlocked(
    conn: &Connection,
    id: i64,
) -> Result<Option<TodoItem>, TodoError> {
    conn.query_row(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, reminder_at, time_precision, recurrence_kind,
                recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                created_at, updated_at, completed_at
         FROM todos
         WHERE id = ?1",
        params![id],
        todo_item_from_row,
    )
    .optional()
    .map_err(TodoError::from_sql)
}

/// 按内部 ID 读取任意状态待办（owner/scope 限定），用于写操作后回读最新行。
pub(super) fn get_by_id_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
) -> Result<Option<TodoItem>, TodoError> {
    conn.query_row(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, reminder_at, time_precision, recurrence_kind,
                recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                created_at, updated_at, completed_at
         FROM todos
         WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3",
        params![id, owner.key.as_str(), owner.scope_key.as_str()],
        todo_item_from_row,
    )
    .optional()
    .map_err(TodoError::from_sql)
}

/// 按内部 ID 读取指定状态待办（owner/scope 限定）。
pub(super) fn get_by_id_status_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    status: TodoStatus,
) -> Result<Option<TodoItem>, TodoError> {
    conn.query_row(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, reminder_at, time_precision, recurrence_kind,
                recurrence_interval_days, recurrence_interval, recurrence_unit, status,
                created_at, updated_at, completed_at
         FROM todos
         WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3 AND status = ?4",
        params![
            id,
            owner.key.as_str(),
            owner.scope_key.as_str(),
            status.as_str()
        ],
        todo_item_from_row,
    )
    .optional()
    .map_err(TodoError::from_sql)
}

/// rusqlite 行到 `TodoItem` 的映射；`time_precision` / `status` 反向解析失败
/// 时封装成列级错误，便于上层按数据异常归类。
fn todo_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TodoItem> {
    let time_precision = row.get::<_, String>(9)?;
    let time_precision = TodoTimePrecision::from_db(&time_precision)
        .map_err(|message| from_sql_text_error(9, message))?;
    let recurrence_kind = row.get::<_, String>(10)?;
    let recurrence_kind = TodoRecurrenceKind::from_db(&recurrence_kind)
        .map_err(|message| from_sql_text_error(10, message))?;
    let recurrence_interval_days = row
        .get::<_, i64>(11)?
        .try_into()
        .map_err(|_| from_sql_text_error(11, "invalid recurrence interval".to_owned()))?;
    let recurrence_interval = row
        .get::<_, i64>(12)?
        .try_into()
        .map_err(|_| from_sql_text_error(12, "invalid recurrence interval".to_owned()))?;
    let recurrence_unit = row.get::<_, String>(13)?;
    let recurrence_unit = TodoRecurrenceUnit::from_db(&recurrence_unit)
        .map_err(|message| from_sql_text_error(13, message))?;
    let status = row.get::<_, String>(14)?;
    let status =
        TodoStatus::from_db(&status).map_err(|message| from_sql_text_error(14, message))?;
    Ok(TodoItem {
        id: row.get::<_, i64>(0)?.to_string(),
        user_id: row.get(1)?,
        scope_key: row.get(2)?,
        title: row.get(3)?,
        detail: row.get(4)?,
        raw_text: row.get(5)?,
        due_date: row.get(6)?,
        due_at: row.get(7)?,
        reminder_at: row.get(8)?,
        time_precision,
        recurrence_kind,
        recurrence_interval_days,
        recurrence_interval,
        recurrence_unit,
        status,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        completed_at: row.get(17)?,
    })
}

/// 把枚举反向解析失败的文字错误包装成 rusqlite 的列级转换错误。
fn from_sql_text_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, TodoError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(TodoError::from_sql)
}
