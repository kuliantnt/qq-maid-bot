//! 长期记忆查询拼装、ID 前缀解析与更新写入 helper。
//!
//! 这里集中维护列表 SELECT 拼装（含个人/群作用域合并）、ID 前缀解析（避免跨
//! 作用域前缀探测）、以及 update 时的字段写入。不改变 schema 与已确认持久化
//! 数据格式；权限校验复用 `permission::ensure_can_modify`，字段清洗复用 `clean::*`。

use qq_maid_common::redaction::redact_sensitive_text;
use rusqlite::{
    Connection, OptionalExtension, Row, params, params_from_iter, types::Value as SqlValue,
};

use super::clean::{
    clean_optional, clean_optional_option, clean_optional_str, clean_required, clean_scope_id,
};
use super::row::memory_from_row;
use super::{
    ListMemoryQuery, MemoryError, MemoryRecord, MemoryScopeType, ScopedMemoryQuery,
    UpdateMemoryRequest,
};

pub(super) fn list_unlocked(
    conn: &Connection,
    query: &ListMemoryQuery,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    let mut sql = String::from(
        "SELECT id, created_at, updated_at, memory_type, scope,
                scope_type, scope_id, created_by_user_id,
                user_id, group_id, content, source_text
         FROM memories
         WHERE 1 = 1",
    );
    let mut values = Vec::<SqlValue>::new();

    push_optional_filter(
        &mut sql,
        &mut values,
        "scope",
        clean_optional_option(query.scope.clone()),
    );
    push_optional_filter(
        &mut sql,
        &mut values,
        "memory_type",
        clean_optional_option(query.memory_type.clone()),
    );
    push_optional_filter(
        &mut sql,
        &mut values,
        "user_id",
        clean_optional_option(query.user_id.clone()),
    );
    push_optional_filter(
        &mut sql,
        &mut values,
        "group_id",
        clean_optional_option(query.group_id.clone()),
    );
    if let Some(q) = clean_optional_option(query.q.clone()) {
        sql.push_str(
            " AND (instr(lower(content), lower(?)) > 0 OR instr(lower(source_text), lower(?)) > 0)",
        );
        values.push(SqlValue::Text(q.clone()));
        values.push(SqlValue::Text(q));
    }
    sql.push_str(" ORDER BY row_id DESC LIMIT ?");
    values.push(SqlValue::Integer(query.limit() as i64));

    let mut stmt = conn.prepare(&sql).map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), memory_from_row)
        .map_err(MemoryError::from_sql)?;
    collect_rows(rows)
}

pub(super) fn list_scoped_unlocked(
    conn: &Connection,
    query: &ScopedMemoryQuery,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    let scope_id = clean_scope_id(&query.scope_id)?;
    let mut sql = String::from(
        "SELECT id, created_at, updated_at, memory_type, scope,
                scope_type, scope_id, created_by_user_id,
                user_id, group_id, content, source_text
         FROM memories
         WHERE scope_type = ? AND scope_id = ?",
    );
    let mut values = vec![
        SqlValue::Text(query.scope_type.as_str().to_owned()),
        SqlValue::Text(scope_id),
    ];

    push_optional_filter(
        &mut sql,
        &mut values,
        "scope",
        clean_optional_option(query.scope.clone()),
    );
    push_optional_filter(
        &mut sql,
        &mut values,
        "memory_type",
        clean_optional_option(query.memory_type.clone()),
    );
    if let Some(q) = clean_optional_option(query.q.clone()) {
        sql.push_str(
            " AND (instr(lower(content), lower(?)) > 0 OR instr(lower(source_text), lower(?)) > 0)",
        );
        values.push(SqlValue::Text(q.clone()));
        values.push(SqlValue::Text(q));
    }
    sql.push_str(" ORDER BY row_id DESC LIMIT ?");
    values.push(SqlValue::Integer(query.limit() as i64));

    let mut stmt = conn.prepare(&sql).map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), memory_from_row)
        .map_err(MemoryError::from_sql)?;
    collect_rows(rows)
}

pub(super) fn list_accessible_for_context_unlocked(
    conn: &Connection,
    personal_scope_id: Option<&str>,
    group_scope_id: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    let mut clauses = Vec::new();
    let mut values = Vec::<SqlValue>::new();
    if let Some(scope_id) = personal_scope_id.and_then(clean_optional_str) {
        clauses.push("(scope_type = ? AND scope_id = ?)".to_owned());
        values.push(SqlValue::Text(
            MemoryScopeType::Personal.as_str().to_owned(),
        ));
        values.push(SqlValue::Text(clean_scope_id(&scope_id)?));
    }
    if let Some(scope_id) = group_scope_id.and_then(clean_optional_str) {
        clauses.push("(scope_type = ? AND scope_id = ?)".to_owned());
        values.push(SqlValue::Text(MemoryScopeType::Group.as_str().to_owned()));
        values.push(SqlValue::Text(clean_scope_id(&scope_id)?));
    }
    if clauses.is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT id, created_at, updated_at, memory_type, scope,
                scope_type, scope_id, created_by_user_id,
                user_id, group_id, content, source_text
         FROM memories
         WHERE {}
         ORDER BY row_id DESC
         LIMIT ?",
        clauses.join(" OR ")
    );
    values.push(SqlValue::Integer(limit.clamp(1, 100) as i64));

    let mut stmt = conn.prepare(&sql).map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), memory_from_row)
        .map_err(MemoryError::from_sql)?;
    collect_rows(rows)
}

fn push_optional_filter(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    column: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        sql.push_str(" AND ");
        sql.push_str(column);
        sql.push_str(" = ?");
        values.push(SqlValue::Text(value));
    }
}

/// 根据完整 ID 或前缀解析真实 ID。
/// 前缀至少需要 4 个字符，且不能有多条匹配。
pub(super) fn resolve_memory_id_unlocked(
    conn: &Connection,
    id_or_prefix: &str,
) -> Result<String, MemoryError> {
    let target = id_or_prefix.trim();
    if target.is_empty() {
        return Err(MemoryError::bad_request("memory id is required"));
    }

    if let Some(id) = conn
        .query_row(
            "SELECT id FROM memories WHERE id = ?1",
            params![target],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(MemoryError::from_sql)?
    {
        return Ok(id);
    }

    if target.chars().count() < 4 {
        return Err(MemoryError::bad_request(
            "memory id prefix must contain at least 4 characters",
        ));
    }

    let mut stmt = conn
        .prepare(
            "SELECT id
             FROM memories
             WHERE substr(id, 1, length(?1)) = ?1
             ORDER BY row_id DESC
             LIMIT 2",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params![target], |row| row.get::<_, String>(0))
        .map_err(MemoryError::from_sql)?;
    let matches = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)?;

    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(MemoryError::not_found("memory not found")),
        _ => Err(MemoryError::bad_request("memory id prefix is ambiguous")),
    }
}

pub(super) fn resolve_memory_id_scoped_unlocked(
    conn: &Connection,
    scope_type: MemoryScopeType,
    scope_id: &str,
    id_or_prefix: &str,
) -> Result<String, MemoryError> {
    let scope_id = clean_scope_id(scope_id)?;
    let target = id_or_prefix.trim();
    if target.is_empty() {
        return Err(MemoryError::bad_request("memory id is required"));
    }

    if let Some(id) = conn
        .query_row(
            "SELECT id FROM memories
             WHERE id = ?1
               AND scope_type = ?2
               AND scope_id = ?3",
            params![target, scope_type.as_str(), scope_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(MemoryError::from_sql)?
    {
        return Ok(id);
    }

    if target.chars().count() < 4 {
        return Err(MemoryError::bad_request(
            "memory id prefix must contain at least 4 characters",
        ));
    }

    let mut stmt = conn
        .prepare(
            "SELECT id
             FROM memories
             WHERE scope_type = ?1
               AND scope_id = ?2
               AND substr(id, 1, length(?3)) = ?3
             ORDER BY row_id DESC
             LIMIT 2",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params![scope_type.as_str(), scope_id, target], |row| {
            row.get::<_, String>(0)
        })
        .map_err(MemoryError::from_sql)?;
    let matches = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)?;

    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(MemoryError::not_found("memory not found")),
        _ => Err(MemoryError::bad_request("memory id prefix is ambiguous")),
    }
}

pub(super) fn apply_update_to_record(
    record: &mut MemoryRecord,
    req: UpdateMemoryRequest,
) -> Result<(), MemoryError> {
    if let Some(content) = req.content {
        record.content = redact_sensitive_text(&clean_required(content, "content")?);
    }
    if let Some(source_text) = req.source_text {
        record.source_text = redact_sensitive_text(&source_text);
    }
    if let Some(memory_type) = req.memory_type.and_then(clean_optional) {
        record.memory_type = memory_type;
    }
    if let Some(scope) = req.scope.and_then(clean_optional) {
        record.scope = scope;
    }
    record.updated_at = Some(super::now_iso_cn());
    Ok(())
}

pub(super) fn update_record_unlocked(
    conn: &Connection,
    record: &MemoryRecord,
) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories
         SET content = ?1, source_text = ?2, memory_type = ?3, scope = ?4, updated_at = ?5
         WHERE id = ?6",
        params![
            record.content.as_str(),
            record.source_text.as_str(),
            record.memory_type.as_str(),
            record.scope.as_str(),
            record.updated_at.as_deref(),
            record.id.as_str(),
        ],
    )
    .map_err(MemoryError::from_sql)?;
    Ok(())
}

fn collect_rows<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>, MemoryError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}
