//! 长期记忆（Memory）存储模块。
//!
//! 长期记忆使用项目级 SQLite 数据库持久化。`MemoryStore` 只接收应用启动阶段
//! 已经初始化并执行 migration 的 [`SqliteDatabase`] 句柄，不自行读取数据库路径，
//! 也不保留 JSONL 回退，避免写入失败时出现“表面成功、实际未保存”的状态。

use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{
    Connection, OptionalExtension, Row, params, params_from_iter, types::Value as SqlValue,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    storage::database::{DatabaseError, SqliteDatabase, SqliteMigration},
    util::time_context::now_iso_cn,
};

/// Memory schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// `row_id` 只作为 SQLite 内部插入顺序使用，用户可见和 pending 快照仍使用 UUID `id`。
/// 列表沿用旧 JSONL 的“后写入先展示”语义，因此按 `row_id DESC` 排序。
pub const MEMORY_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "memory_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS memories (
            row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            memory_type TEXT NOT NULL DEFAULT 'note',
            scope TEXT NOT NULL DEFAULT 'general',
            user_id TEXT,
            group_id TEXT,
            content TEXT NOT NULL,
            source_text TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memories_scope_type_order
            ON memories(scope, memory_type, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_user_group_order
            ON memories(user_id, group_id, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_created_order
            ON memories(row_id);",
};

pub const MEMORY_MIGRATIONS: &[SqliteMigration] = &[MEMORY_SCHEMA_V1];

/// 敏感信息匹配模式列表，用于在存储时自动脱敏 API Key、Token 等凭证。
static SENSITIVE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)(OPENAI_API_KEY\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(DEEPSEEK_API_KEY\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(QQ_SECRET\s*=\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(API[_ -]?KEY\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(SECRET\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"(?i)(TOKEN\s*[:=]\s*)\S+").unwrap(),
            "$1<redacted>",
        ),
        (
            Regex::new(r"sk-[A-Za-z0-9_-]{20,}").unwrap(),
            "<redacted:openai_api_key>",
        ),
        (
            Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._-]{20,}").unwrap(),
            "Bearer <redacted>",
        ),
    ]
});

/// 记忆记录，表示一条持久化存储的长期记忆。
///
/// 包含记忆内容、类型（如 note / preference）、作用域（如 general / front_detection）、
/// 关联的用户和群组信息，以及创建/更新时间。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub ts: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: String,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(
        rename = "type",
        alias = "memory_type",
        default = "default_memory_type"
    )]
    pub memory_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub source_text: String,
}

/// 创建记忆的请求参数。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateMemoryRequest {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub source_text: String,
    #[serde(
        rename = "type",
        alias = "memory_type",
        default = "default_memory_type"
    )]
    pub memory_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

/// 更新记忆的请求参数，所有字段均为可选。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpdateMemoryRequest {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub source_text: Option<String>,
    #[serde(rename = "type", alias = "memory_type", default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// 列表查询参数，支持按内容、作用域、类型、用户和群组过滤。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ListMemoryQuery {
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub scope: Option<String>,
    #[serde(rename = "type", alias = "memory_type")]
    pub memory_type: Option<String>,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
}

/// API 响应中表示错误信息的结构。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryErrorInfo {
    pub code: String,
    pub message: String,
}

/// 单条记忆的响应体。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryItemResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

/// 记忆列表的响应体。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryListResponse {
    pub ok: bool,
    pub memories: Vec<MemoryRecord>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

/// 删除记忆的响应体。
#[derive(Debug, Clone, Serialize)]
pub struct MemoryDeleteResponse {
    pub ok: bool,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

/// 内部使用的记忆操作错误类型。
#[derive(Debug, Clone)]
pub struct MemoryError {
    code: &'static str,
    message: String,
}

/// 记忆存储器，基于项目通用 SQLite 连接实现。
///
/// 数据库连接由应用启动时统一打开并执行 migration；MemoryStore 只接收已初始化句柄，
/// 不自行读取路径，也不在业务方法中创建表或回退到旧 JSONL 文件。
#[derive(Debug, Clone)]
pub struct MemoryStore {
    database: SqliteDatabase,
}

impl MemoryStore {
    /// 创建一个新的 MemoryStore，复用应用级 SQLite 句柄。
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 创建一条记忆记录，写入数据库并返回新记录。内容会自动脱敏处理。
    pub fn create(&self, req: CreateMemoryRequest) -> Result<MemoryRecord, MemoryError> {
        let now = now_iso_cn();
        let content = clean_required(req.content, "content")?;
        let record = MemoryRecord {
            id: Uuid::new_v4().to_string(),
            ts: now.clone(),
            created_at: now.clone(),
            updated_at: None,
            memory_type: clean_optional(req.memory_type).unwrap_or_else(default_memory_type),
            scope: clean_optional(req.scope).unwrap_or_else(default_scope),
            user_id: clean_optional_option(req.user_id),
            group_id: clean_optional_option(req.group_id),
            content: redact_sensitive_text(&content),
            source_text: redact_sensitive_text(&req.source_text),
        };
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO memories (
                id, created_at, updated_at, memory_type, scope, user_id, group_id, content, source_text
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.id.as_str(),
                record.created_at.as_str(),
                record.memory_type.as_str(),
                record.scope.as_str(),
                record.user_id.as_deref(),
                record.group_id.as_deref(),
                record.content.as_str(),
                record.source_text.as_str(),
            ],
        )
        .map_err(MemoryError::from_sql)?;
        get_by_id_unlocked(&conn, &record.id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after insert"))
    }

    /// 按查询条件列出记忆记录，返回匹配结果（后写入先展示、限制数量）。
    pub fn list(&self, query: ListMemoryQuery) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_unlocked(&conn, &query)
    }

    /// 根据完整 ID 或前缀查找单条记忆记录。
    pub fn get(&self, id_or_prefix: &str) -> Result<MemoryRecord, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        get_by_id_unlocked(&conn, &id)?.ok_or_else(|| MemoryError::not_found("memory not found"))
    }

    /// 更新一条记忆记录的指定字段，返回更新后的记录。
    pub fn update(
        &self,
        id_or_prefix: &str,
        req: UpdateMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        if !req.has_update() {
            return Err(MemoryError::bad_request("no memory update fields provided"));
        }

        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        let mut record = get_by_id_unlocked(&conn, &id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))?;

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
        record.updated_at = Some(now_iso_cn());

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
                id.as_str(),
            ],
        )
        .map_err(MemoryError::from_sql)?;

        get_by_id_unlocked(&conn, &id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after update"))
    }

    /// 根据完整 ID 或前缀删除一条记忆记录，返回被删除记录的 ID。
    pub fn delete(&self, id_or_prefix: &str) -> Result<String, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        let changed = conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .map_err(MemoryError::from_sql)?;
        if changed == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        Ok(id)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, MemoryError> {
        self.database
            .connection()
            .map_err(MemoryError::from_database)
    }

    #[cfg(test)]
    pub fn drop_schema_for_test(&self) -> Result<(), MemoryError> {
        let conn = self.connection()?;
        conn.execute("DROP TABLE memories", [])
            .map_err(MemoryError::from_sql)?;
        Ok(())
    }
}

impl MemoryItemResponse {
    /// 构造成功响应。
    pub fn ok(memory: MemoryRecord) -> Self {
        Self {
            ok: true,
            memory: Some(memory),
            error: None,
        }
    }

    /// 构造错误响应。
    pub fn error(err: MemoryError) -> Self {
        Self {
            ok: false,
            memory: None,
            error: Some(err.into_info()),
        }
    }
}

impl MemoryListResponse {
    /// 构造成功响应。
    pub fn ok(memories: Vec<MemoryRecord>) -> Self {
        Self {
            count: memories.len(),
            ok: true,
            memories,
            error: None,
        }
    }

    /// 构造错误响应。
    pub fn error(err: MemoryError) -> Self {
        Self {
            ok: false,
            memories: Vec::new(),
            count: 0,
            error: Some(err.into_info()),
        }
    }
}

impl MemoryDeleteResponse {
    /// 构造成功响应。
    pub fn ok(id: String) -> Self {
        Self {
            ok: true,
            deleted: true,
            id: Some(id),
            error: None,
        }
    }

    /// 构造错误响应。
    pub fn error(err: MemoryError) -> Self {
        Self {
            ok: false,
            deleted: false,
            id: None,
            error: Some(err.into_info()),
        }
    }
}

impl MemoryError {
    /// 获取错误码。
    pub fn code(&self) -> &str {
        self.code
    }

    /// 获取错误消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        Self::io(format!("sqlite failed: {err}"))
    }

    fn into_info(self) -> MemoryErrorInfo {
        MemoryErrorInfo {
            code: self.code.to_owned(),
            message: self.message,
        }
    }
}

impl ListMemoryQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

impl UpdateMemoryRequest {
    fn has_update(&self) -> bool {
        self.content.is_some()
            || self.source_text.is_some()
            || self.memory_type.is_some()
            || self.scope.is_some()
    }
}

fn list_unlocked(
    conn: &Connection,
    query: &ListMemoryQuery,
) -> Result<Vec<MemoryRecord>, MemoryError> {
    let mut sql = String::from(
        "SELECT id, created_at, updated_at, memory_type, scope, user_id, group_id, content, source_text
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
fn resolve_memory_id_unlocked(
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

fn get_by_id_unlocked(conn: &Connection, id: &str) -> Result<Option<MemoryRecord>, MemoryError> {
    conn.query_row(
        "SELECT id, created_at, updated_at, memory_type, scope, user_id, group_id, content, source_text
         FROM memories
         WHERE id = ?1",
        params![id],
        memory_from_row,
    )
    .optional()
    .map_err(MemoryError::from_sql)
}

fn memory_from_row(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let created_at: String = row.get(1)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        ts: created_at.clone(),
        created_at,
        updated_at: row.get(2)?,
        memory_type: row.get(3)?,
        scope: row.get(4)?,
        user_id: row.get(5)?,
        group_id: row.get(6)?,
        content: row.get(7)?,
        source_text: row.get(8)?,
    })
}

fn collect_rows<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>, MemoryError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}

/// 清理并验证必填字段：去除首尾空格，空值则返回错误。
fn clean_required(value: String, field: &str) -> Result<String, MemoryError> {
    clean_optional(value).ok_or_else(|| MemoryError::bad_request(format!("{field} is required")))
}

/// 清理可选字段：去除首尾空格，空值返回 None。
fn clean_optional(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// 清理可选 Option 字段：内层值空则返回 None。
fn clean_optional_option(value: Option<String>) -> Option<String> {
    value.and_then(clean_optional)
}

/// 脱敏文本中的敏感信息（API Key、Secret、Token 等）。
fn redact_sensitive_text(text: &str) -> String {
    let mut redacted = text.to_owned();
    for (pattern, replacement) in SENSITIVE_PATTERNS.iter() {
        redacted = pattern.replace_all(&redacted, *replacement).to_string();
    }
    redacted
}

/// 默认记忆类型。
fn default_memory_type() -> String {
    "note".to_owned()
}

/// 默认记忆作用域。
fn default_scope() -> String {
    "general".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> MemoryStore {
        MemoryStore::new(
            SqliteDatabase::open_temp("qq-maid-memory-test", MEMORY_MIGRATIONS).unwrap(),
        )
    }

    fn create_memory(store: &MemoryStore, content: &str) -> MemoryRecord {
        store
            .create(CreateMemoryRequest {
                user_id: Some("u1".to_owned()),
                group_id: Some("g1".to_owned()),
                content: content.to_owned(),
                source_text: format!("/memory {content}"),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
            })
            .unwrap()
    }

    #[test]
    fn create_get_list_update_and_delete_memory() {
        let store = test_store();
        let created = store
            .create(CreateMemoryRequest {
                user_id: Some("u1".to_owned()),
                group_id: Some("g1".to_owned()),
                content: "如果不确定前台，请礼貌询问".to_owned(),
                source_text: "/memory 如果不确定前台，请礼貌询问".to_owned(),
                memory_type: "preference".to_owned(),
                scope: "front_detection".to_owned(),
            })
            .unwrap();

        let listed = store
            .list(ListMemoryQuery {
                q: Some("礼貌".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);

        let prefix = &created.id[..8];
        assert_eq!(store.get(prefix).unwrap().id, created.id);

        let updated = store
            .update(
                prefix,
                UpdateMemoryRequest {
                    content: Some("前台不确定时先询问".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.content, "前台不确定时先询问");
        assert!(updated.updated_at.is_some());

        let deleted_id = store.delete(prefix).unwrap();
        assert_eq!(deleted_id, created.id);
        assert!(store.get(prefix).is_err());
    }

    #[test]
    fn list_uses_stable_newest_first_order() {
        let store = test_store();
        let first = create_memory(&store, "第一条记忆");
        let second = create_memory(&store, "第二条记忆");

        let records = store.list(ListMemoryQuery::default()).unwrap();

        assert_eq!(records[0].id, second.id);
        assert_eq!(records[1].id, first.id);
    }

    #[test]
    fn filters_by_scope_type_user_group_and_query_text() {
        let store = test_store();
        store
            .create(CreateMemoryRequest {
                user_id: Some("u1".to_owned()),
                group_id: Some("g1".to_owned()),
                content: "前台不确定时先询问本人".to_owned(),
                source_text: "seed".to_owned(),
                memory_type: "preference".to_owned(),
                scope: "front_detection".to_owned(),
            })
            .unwrap();
        create_memory(&store, "普通记忆");

        let records = store
            .list(ListMemoryQuery {
                q: Some("本人".to_owned()),
                scope: Some("front_detection".to_owned()),
                memory_type: Some("preference".to_owned()),
                user_id: Some("u1".to_owned()),
                group_id: Some("g1".to_owned()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].content, "前台不确定时先询问本人");
    }

    #[test]
    fn reports_not_found_and_invalid_update() {
        let store = test_store();

        assert_eq!(store.get("missing-id").unwrap_err().code(), "not_found");
        assert_eq!(store.get("abc").unwrap_err().code(), "bad_request");
        assert_eq!(
            store
                .update("missing-id", UpdateMemoryRequest::default())
                .unwrap_err()
                .code(),
            "bad_request"
        );
        assert_eq!(store.delete("missing-id").unwrap_err().code(), "not_found");
    }

    #[test]
    fn sqlite_reopen_keeps_memory_records() {
        let path =
            std::env::temp_dir().join(format!("qq-maid-memory-reopen-{}.db", Uuid::new_v4()));
        let first_store = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
        let created = create_memory(&first_store, "重启后仍要保留");
        drop(first_store);

        let reopened = MemoryStore::new(SqliteDatabase::open(&path, MEMORY_MIGRATIONS).unwrap());
        let restored = reopened.get(&created.id).unwrap();

        assert_eq!(restored.content, "重启后仍要保留");
        assert_eq!(restored.ts, restored.created_at);
    }

    #[test]
    fn stores_multiline_chinese_special_and_long_content() {
        let store = test_store();
        let content = format!(
            "第一行：中文、emoji-like 文本 :-) 和 SQL 符号 ' \" % _\n第二行：{}",
            "长文本".repeat(80)
        );

        let created = create_memory(&store, &content);
        let restored = store.get(&created.id).unwrap();

        assert_eq!(restored.content, content);
        assert!(restored.source_text.contains('\n'));
        assert_eq!(
            store
                .list(ListMemoryQuery {
                    q: Some("% _".to_owned()),
                    ..Default::default()
                })
                .unwrap()
                .len(),
            1
        );
    }
}
