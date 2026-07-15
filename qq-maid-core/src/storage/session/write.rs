//! Session SQLite 写入与通用行收集 helper。

use rusqlite::{Connection, Row, Transaction, params};

use super::jsonio::{encode_json, encode_optional_json};
use super::{SessionError, SessionRecord};

pub(super) fn upsert_session_tx(
    tx: &Transaction<'_>,
    session: &SessionRecord,
) -> Result<(), SessionError> {
    let state_json = encode_json(&session.state, "session state")?;
    let pending_operation_json = encode_optional_json(&session.pending_operation, "pending")?;
    let last_todo_query_json = encode_optional_json(&session.last_todo_query, "last todo query")?;
    let last_todo_action_json =
        encode_optional_json(&session.last_todo_action, "last todo action")?;
    let last_memory_query_json =
        encode_optional_json(&session.last_memory_query, "last memory query")?;
    let extra_json = encode_json(&session.extra, "session extra")?;
    tx.execute(
        "INSERT INTO sessions (
            session_id, scope, scope_key, user_id, group_id, guild_id, channel_id, platform,
            created_at, updated_at, title, state_json, summary, pending_operation_json,
            last_todo_query_json, last_todo_action_json, last_memory_query_json, extra_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(session_id) DO UPDATE SET
            scope = excluded.scope,
            scope_key = excluded.scope_key,
            user_id = excluded.user_id,
            group_id = excluded.group_id,
            guild_id = excluded.guild_id,
            channel_id = excluded.channel_id,
            platform = excluded.platform,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            title = excluded.title,
            state_json = excluded.state_json,
            summary = excluded.summary,
            pending_operation_json = excluded.pending_operation_json,
            last_todo_query_json = excluded.last_todo_query_json,
            last_todo_action_json = excluded.last_todo_action_json,
            last_memory_query_json = excluded.last_memory_query_json,
            extra_json = excluded.extra_json",
        params![
            session.session_id.as_str(),
            session.scope.as_str(),
            session.scope_key.as_str(),
            session.user_id.as_deref(),
            session.group_id.as_deref(),
            session.guild_id.as_deref(),
            session.channel_id.as_deref(),
            session.platform.as_str(),
            session.created_at.as_str(),
            session.updated_at.as_str(),
            session.title.as_str(),
            state_json,
            session.summary.as_str(),
            pending_operation_json,
            last_todo_query_json,
            last_todo_action_json,
            last_memory_query_json,
            extra_json,
        ],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}

pub(super) fn replace_messages_tx(
    tx: &Transaction<'_>,
    session: &SessionRecord,
) -> Result<(), SessionError> {
    tx.execute(
        "DELETE FROM session_messages WHERE session_id = ?1",
        params![session.session_id.as_str()],
    )
    .map_err(SessionError::from_sql)?;
    for (index, message) in session.history.iter().enumerate() {
        tx.execute(
            "INSERT INTO session_messages (session_id, message_index, role, content, ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.session_id.as_str(),
                index as i64,
                message.role.as_str(),
                message.content.as_str(),
                message.ts.as_str(),
            ],
        )
        .map_err(SessionError::from_sql)?;
    }
    Ok(())
}

pub(super) fn set_active_session_id_conn(
    conn: &Connection,
    scope_key: &str,
    session_id: &str,
    now: &str,
) -> Result<(), SessionError> {
    conn.execute(
        "INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(scope_key) DO UPDATE SET
            session_id = excluded.session_id,
            updated_at = excluded.updated_at",
        params![scope_key, session_id, now],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}

pub(super) fn set_active_session_id_tx(
    tx: &Transaction<'_>,
    scope_key: &str,
    session_id: &str,
    now: &str,
) -> Result<(), SessionError> {
    tx.execute(
        "INSERT INTO session_active (scope_key, session_id, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(scope_key) DO UPDATE SET
            session_id = excluded.session_id,
            updated_at = excluded.updated_at",
        params![scope_key, session_id, now],
    )
    .map_err(SessionError::from_sql)?;
    Ok(())
}

pub(super) fn collect_sql_rows<T, F>(
    rows: rusqlite::MappedRows<'_, F>,
) -> Result<Vec<T>, SessionError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SessionError::from_sql)
}
