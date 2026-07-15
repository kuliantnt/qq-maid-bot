//! Session 行映射与整行读取 helper。
//!
//! 集中维护 `sessions` / `session_messages` 表的 SELECT 行到 `SessionRecord` 的
//! 映射，以及按 session_id 整行重新读取。写操作（INSERT/UPDATE）集中在
//! `write.rs`，事务边界仍由 `SessionStore` 控制。
//! 不改变 schema 与已确认持久化格式。

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::runtime::pending::PreparedAction;

use super::jsonio::{decode_json, decode_optional_json};
use super::normalize::normalize_session;
use super::{SessionError, SessionMessage, SessionRecord, collect_sql_rows};

/// sessions 表读取出来的原始行，按列顺序保存；`into_record` 再解码 JSON 字段。
#[derive(Debug)]
struct StoredSessionRow {
    session_id: String,
    scope: String,
    scope_key: String,
    user_id: Option<String>,
    group_id: Option<String>,
    guild_id: Option<String>,
    channel_id: Option<String>,
    platform: String,
    created_at: String,
    updated_at: String,
    title: String,
    state_json: String,
    summary: String,
    pending_operation_json: Option<String>,
    last_todo_query_json: Option<String>,
    last_todo_action_json: Option<String>,
    last_memory_query_json: Option<String>,
    extra_json: String,
}

/// 按顺序从 `sessions` 行读出所有列，供 query_row 使用。
/// 仅供本模块的 `load_session_unlocked` 使用，保持私有。
fn stored_session_row(row: &Row<'_>) -> rusqlite::Result<StoredSessionRow> {
    Ok(StoredSessionRow {
        session_id: row.get(0)?,
        scope: row.get(1)?,
        scope_key: row.get(2)?,
        user_id: row.get(3)?,
        group_id: row.get(4)?,
        guild_id: row.get(5)?,
        channel_id: row.get(6)?,
        platform: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        title: row.get(10)?,
        state_json: row.get(11)?,
        summary: row.get(12)?,
        pending_operation_json: row.get(13)?,
        last_todo_query_json: row.get(14)?,
        last_todo_action_json: row.get(15)?,
        last_memory_query_json: row.get(16)?,
        extra_json: row.get(17)?,
    })
}

impl StoredSessionRow {
    /// 把原始行与消息历史解码为完整 `SessionRecord`，仅供本模块使用。
    fn into_record(self, history: Vec<SessionMessage>) -> Result<SessionRecord, SessionError> {
        Ok(SessionRecord {
            session_id: self.session_id,
            scope: self.scope,
            scope_key: self.scope_key,
            user_id: self.user_id,
            group_id: self.group_id,
            guild_id: self.guild_id,
            channel_id: self.channel_id,
            platform: self.platform,
            created_at: self.created_at,
            updated_at: self.updated_at,
            title: self.title,
            state: decode_json(&self.state_json, "session state")?,
            summary: self.summary,
            history,
            pending_operation: decode_pending_operation_json(
                self.pending_operation_json.as_deref(),
            )?,
            last_todo_query: decode_optional_json(
                self.last_todo_query_json.as_deref(),
                "last todo query",
            )?,
            last_todo_action: decode_optional_json(
                self.last_todo_action_json.as_deref(),
                "last todo action",
            )?,
            last_memory_query: decode_optional_json(
                self.last_memory_query_json.as_deref(),
                "last memory query",
            )?,
            extra: decode_json(&self.extra_json, "session extra")?,
        })
    }
}

fn decode_pending_operation_json(
    text: Option<&str>,
) -> Result<Option<PreparedAction>, SessionError> {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|err| {
        SessionError::decode(format!("failed to decode pending operation: {err}"))
    })?;
    if is_legacy_memory_pending(&value) {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|err| SessionError::decode(format!("failed to decode pending operation: {err}")))
}

fn is_legacy_memory_pending(value: &serde_json::Value) -> bool {
    matches!(
        value.get("kind").and_then(serde_json::Value::as_str),
        Some("memory_create" | "memory_update" | "memory_delete")
    )
}

/// 按 session_id 整行读取并规范化为 `SessionRecord`，缺失返回 None。
pub(super) fn load_session_unlocked(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionRecord>, SessionError> {
    let row = conn
        .query_row(
            "SELECT session_id, scope, scope_key, user_id, group_id, guild_id,
                    channel_id, platform, created_at, updated_at, title,
                    state_json, summary, pending_operation_json, last_todo_query_json,
                    last_todo_action_json, last_memory_query_json, extra_json
             FROM sessions
             WHERE session_id = ?1",
            params![session_id],
            stored_session_row,
        )
        .optional()
        .map_err(SessionError::from_sql)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let messages = load_messages_unlocked(conn, &row.session_id)?;
    let mut session = row.into_record(messages)?;
    normalize_session(&mut session);
    Ok(Some(session))
}

/// 按 message_index 顺序读取某会话的全部历史消息。
pub(super) fn load_messages_unlocked(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionMessage>, SessionError> {
    let mut stmt = conn
        .prepare(
            "SELECT role, content, ts
             FROM session_messages
             WHERE session_id = ?1
             ORDER BY message_index ASC, id ASC",
        )
        .map_err(SessionError::from_sql)?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(SessionMessage {
                role: row.get(0)?,
                content: row.get(1)?,
                ts: row.get(2)?,
            })
        })
        .map_err(SessionError::from_sql)?;
    collect_sql_rows(rows)
}
