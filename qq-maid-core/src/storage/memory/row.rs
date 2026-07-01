//! 长期记忆行映射与按 ID 读取 helper。
//!
//! 集中维护 `memories` 表行到 `MemoryRecord` 的映射，以及按完整 ID（可选附带
//! 作用域边界）读取单条记录。这里只读不写，不改变权限判定与 schema。

use rusqlite::{Connection, OptionalExtension, params};

use super::clean::clean_scope_id;
use super::{MemoryError, MemoryRecord, MemoryScopeType};

/// 从 `memories` 行映射为 `MemoryRecord`。`ts` 复用 `created_at`，保持与旧
/// JSONL 行为一致（列表语义不依赖独立时间戳字段）。
pub(super) fn memory_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let created_at: String = row.get(1)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        ts: created_at.clone(),
        created_at,
        updated_at: row.get(2)?,
        memory_type: row.get(3)?,
        scope: row.get(4)?,
        scope_type: row.get(5)?,
        scope_id: row.get(6)?,
        created_by_user_id: row.get(7)?,
        user_id: row.get(8)?,
        group_id: row.get(9)?,
        content: row.get(10)?,
        source_text: row.get(11)?,
    })
}

/// 按完整 ID 读取单条记忆（不限作用域），用于内部写后回读与管理路径。
pub(super) fn get_by_id_unlocked(
    conn: &Connection,
    id: &str,
) -> Result<Option<MemoryRecord>, MemoryError> {
    conn.query_row(
        "SELECT id, created_at, updated_at, memory_type, scope,
                scope_type, scope_id, created_by_user_id,
                user_id, group_id, content, source_text
         FROM memories
         WHERE id = ?1",
        params![id],
        memory_from_row,
    )
    .optional()
    .map_err(MemoryError::from_sql)
}

/// 在指定作用域内按完整 ID 读取单条记忆，便于越权前先做作用域过滤。
pub(super) fn get_by_id_scoped_unlocked(
    conn: &Connection,
    scope_type: MemoryScopeType,
    scope_id: &str,
    id: &str,
) -> Result<Option<MemoryRecord>, MemoryError> {
    let scope_id = clean_scope_id(scope_id)?;
    conn.query_row(
        "SELECT id, created_at, updated_at, memory_type, scope,
                scope_type, scope_id, created_by_user_id,
                user_id, group_id, content, source_text
         FROM memories
         WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3",
        params![id, scope_type.as_str(), scope_id],
        memory_from_row,
    )
    .optional()
    .map_err(MemoryError::from_sql)
}
