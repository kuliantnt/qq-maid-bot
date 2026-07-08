//! 长期记忆（Memory）存储模块。
//!
//! 长期记忆使用项目级 SQLite 数据库持久化。`MemoryStore` 只接收应用启动阶段
//! 已经初始化并执行 migration 的 [`SqliteDatabase`] 句柄，不自行读取数据库路径，
//! 也不保留 JSONL 回退，避免写入失败时出现“表面成功、实际未保存”的状态。

use qq_maid_common::redaction::redact_sensitive_text;
use qq_maid_common::time_context::now_iso_cn;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::database::{DatabaseError, SqliteDatabase, SqliteMigration};

// 拆分出的纯 helper 子模块：均不改变 schema、权限语义与已确认持久化格式。
mod clean;
mod permission;
mod query;
mod row;

use clean::{
    clean_optional, clean_optional_option, clean_required, clean_scope_id, default_memory_type,
    default_scope, infer_legacy_scope_identity, legacy_unassigned_scope_type,
};
use permission::ensure_can_modify;
use query::{
    apply_update_to_record, list_accessible_for_context_unlocked, list_scoped_unlocked,
    list_unlocked, resolve_memory_id_scoped_unlocked, resolve_memory_id_unlocked,
    update_record_unlocked,
};
use row::{get_by_id_scoped_unlocked, get_by_id_unlocked};

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

/// Memory schema v2：补充真正的访问边界字段。
///
/// 旧字段 `scope` 只表示记忆业务分类，不能作为权限边界；`scope_type/scope_id`
/// 才表示个人或群记忆归属。迁移只信任已存在的 `user_id/group_id`，缺少稳定标识的旧记录
/// 统一放入 `legacy_unassigned`，避免把无法证明归属的数据暴露给任意用户。
pub const MEMORY_SCOPE_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "memory_scope_schema_v2",
    sql: "ALTER TABLE memories ADD COLUMN scope_type TEXT NOT NULL DEFAULT 'legacy_unassigned';
        ALTER TABLE memories ADD COLUMN scope_id TEXT;
        ALTER TABLE memories ADD COLUMN created_by_user_id TEXT;
        UPDATE memories
           SET scope_type = 'personal',
               scope_id = user_id,
               created_by_user_id = user_id
         WHERE user_id IS NOT NULL AND trim(user_id) <> '';
        UPDATE memories
           SET scope_type = 'group',
               scope_id = group_id
         WHERE (scope_type IS NULL OR scope_type = 'legacy_unassigned')
           AND group_id IS NOT NULL AND trim(group_id) <> '';
        UPDATE memories
           SET scope_type = 'legacy_unassigned',
               scope_id = NULL,
               created_by_user_id = NULL
         WHERE scope_type NOT IN ('personal', 'group');
        CREATE INDEX IF NOT EXISTS idx_memories_scope_boundary_order
            ON memories(scope_type, scope_id, row_id);
        CREATE INDEX IF NOT EXISTS idx_memories_group_creator_order
            ON memories(scope_type, scope_id, created_by_user_id, row_id);",
};

pub const MEMORY_MIGRATIONS: &[SqliteMigration] = &[MEMORY_SCHEMA_V1, MEMORY_SCOPE_SCHEMA_V2];

/// 记忆记录，表示一条持久化存储的长期记忆。
///
/// 包含记忆内容、类型（如 note / preference）、作用域（如 general / writing_style）、
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
    /// 真正的访问边界类型：personal / group / legacy_unassigned。
    #[serde(default = "legacy_unassigned_scope_type")]
    pub scope_type: String,
    /// 真正的访问边界 ID：个人为稳定用户 ID，群记忆为稳定群 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    /// 创建者用户 ID；用于记录来源，新版群记忆管理权限由群角色决定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_user_id: Option<String>,
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

/// scoped 创建请求。运行时业务入口应优先使用它，避免依赖旧 `user_id/group_id`
/// 推断权限边界；旧 `CreateMemoryRequest` 只作为兼容入口保留。
#[derive(Debug, Clone)]
pub struct CreateScopedMemoryRequest {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub created_by_user_id: String,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub content: String,
    pub source_text: String,
    pub memory_type: String,
    pub scope: String,
}

/// scoped 原子替换请求。产品语义仍是删除旧记忆并创建新记忆，数据库层必须
/// 保证新旧记录切换在同一个事务内完成，避免创建失败时丢失旧记录。
#[derive(Debug, Clone)]
pub struct ReplaceScopedMemoryRequest {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub id_or_prefix: String,
    pub actor: MemoryActor,
    pub user_id: Option<String>,
    pub group_id: Option<String>,
    pub content: String,
    pub source_text: String,
    pub memory_type: String,
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

/// 长期记忆访问边界。不要和 `MemoryRecord::scope` 混用，后者只是业务分类。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScopeType {
    Personal,
    Group,
    LegacyUnassigned,
}

/// 带权限边界的记忆查询条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedMemoryQuery {
    pub scope_type: MemoryScopeType,
    pub scope_id: String,
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub scope: Option<String>,
    pub memory_type: Option<String>,
}

/// 更新、删除或替换 scoped 记忆时的操作者上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryActor {
    pub user_id: String,
    pub can_manage_group_memory: bool,
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
        let user_id = clean_optional_option(req.user_id);
        let group_id = clean_optional_option(req.group_id);
        let (scope_type, scope_id, created_by_user_id) =
            infer_legacy_scope_identity(user_id.as_deref(), group_id.as_deref());
        self.create_scoped(CreateScopedMemoryRequest {
            scope_type,
            scope_id,
            created_by_user_id,
            user_id,
            group_id,
            content: req.content,
            source_text: req.source_text,
            memory_type: req.memory_type,
            scope: req.scope,
        })
    }

    /// 在明确作用域内创建记忆。新业务路径必须显式传入访问边界。
    pub fn create_scoped(
        &self,
        req: CreateScopedMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        let record = build_scoped_record(req)?;
        let conn = self.connection()?;
        insert_record_unlocked(&conn, &record)?;
        get_by_id_unlocked(&conn, &record.id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after insert"))
    }

    /// 在当前作用域内原子替换记忆。产品语义是旧记录被删除、新记录使用新 ID。
    pub fn replace_scoped(
        &self,
        req: ReplaceScopedMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        let scope_id = clean_scope_id(&req.scope_id)?;
        let new_record = build_scoped_record(CreateScopedMemoryRequest {
            scope_type: req.scope_type,
            scope_id: scope_id.clone(),
            created_by_user_id: req.actor.user_id.clone(),
            user_id: req.user_id,
            group_id: req.group_id,
            content: req.content,
            source_text: req.source_text,
            memory_type: req.memory_type,
            scope: req.scope,
        })?;

        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(MemoryError::from_sql)?;
        let old_id =
            resolve_memory_id_scoped_unlocked(&tx, req.scope_type, &scope_id, &req.id_or_prefix)?;
        let old_record = get_by_id_scoped_unlocked(&tx, req.scope_type, &scope_id, &old_id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))?;
        ensure_can_modify(&old_record, &req.actor)?;

        insert_record_unlocked(&tx, &new_record)?;
        let changed = tx
            .execute(
                "DELETE FROM memories WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3",
                params![old_id.as_str(), req.scope_type.as_str(), scope_id.as_str()],
            )
            .map_err(MemoryError::from_sql)?;
        if changed == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        tx.commit().map_err(MemoryError::from_sql)?;

        Ok(new_record)
    }

    /// 按查询条件列出记忆记录，返回匹配结果（后写入先展示、限制数量）。
    pub fn list(&self, query: ListMemoryQuery) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_unlocked(&conn, &query)
    }

    /// 按明确访问边界列出记忆；ID 前缀解析、管理和注入路径都应先限定这里。
    pub fn list_scoped(&self, query: ScopedMemoryQuery) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_scoped_unlocked(&conn, &query)
    }

    /// 分别限定个人/群作用域后，在 SQL 内按原有 row_id DESC 合并并截断。
    pub fn list_accessible_for_context(
        &self,
        personal_scope_id: Option<&str>,
        group_scope_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_accessible_for_context_unlocked(&conn, personal_scope_id, group_scope_id, limit)
    }

    /// 根据完整 ID 或前缀查找单条记忆记录。
    pub fn get(&self, id_or_prefix: &str) -> Result<MemoryRecord, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        get_by_id_unlocked(&conn, &id)?.ok_or_else(|| MemoryError::not_found("memory not found"))
    }

    /// 在当前作用域内根据完整 ID 或前缀查找，避免跨作用域前缀探测。
    pub fn get_scoped(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id_or_prefix: &str,
    ) -> Result<MemoryRecord, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_scoped_unlocked(&conn, scope_type, scope_id, id_or_prefix)?;
        get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))
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

        apply_update_to_record(&mut record, req)?;
        update_record_unlocked(&conn, &record)?;

        get_by_id_unlocked(&conn, &id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after update"))
    }

    /// 在当前作用域内更新记忆。群记忆只允许群主或管理员修改。
    pub fn update_scoped(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id_or_prefix: &str,
        actor: &MemoryActor,
        req: UpdateMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        if !req.has_update() {
            return Err(MemoryError::bad_request("no memory update fields provided"));
        }

        let conn = self.connection()?;
        let id = resolve_memory_id_scoped_unlocked(&conn, scope_type, scope_id, id_or_prefix)?;
        let mut record = get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))?;
        ensure_can_modify(&record, actor)?;

        apply_update_to_record(&mut record, req)?;
        update_record_unlocked(&conn, &record)?;

        get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
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

    /// 在当前作用域内删除记忆。群记忆只允许群主或管理员删除；权限失败不暴露记录归属。
    pub fn delete_scoped(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id_or_prefix: &str,
        actor: &MemoryActor,
    ) -> Result<String, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_scoped_unlocked(&conn, scope_type, scope_id, id_or_prefix)?;
        let record = get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))?;
        ensure_can_modify(&record, actor)?;
        let changed = conn
            .execute(
                "DELETE FROM memories WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3",
                params![id.as_str(), scope_type.as_str(), clean_scope_id(scope_id)?],
            )
            .map_err(MemoryError::from_sql)?;
        if changed == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        Ok(id)
    }

    fn connection(&self) -> Result<crate::storage::database::PooledSqliteConnection, MemoryError> {
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

    #[cfg(test)]
    pub fn abort_memory_insert_for_test(&self) -> Result<(), MemoryError> {
        let conn = self.connection()?;
        conn.execute_batch(
            "CREATE TRIGGER abort_memory_insert_for_test
             BEFORE INSERT ON memories
             BEGIN
                 SELECT RAISE(ABORT, 'memory insert aborted for test');
             END;",
        )
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

    /// 判断错误是否可对用户统一展示为当前范围内不可管理。
    pub fn is_not_found_or_forbidden(&self) -> bool {
        matches!(self.code, "not_found" | "forbidden")
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

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            code: "forbidden",
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

impl ScopedMemoryQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

impl MemoryScopeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Group => "group",
            Self::LegacyUnassigned => "legacy_unassigned",
        }
    }
}

impl std::str::FromStr for MemoryScopeType {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "personal" => Ok(Self::Personal),
            "group" => Ok(Self::Group),
            "legacy_unassigned" => Ok(Self::LegacyUnassigned),
            _ => Err(()),
        }
    }
}

fn build_scoped_record(req: CreateScopedMemoryRequest) -> Result<MemoryRecord, MemoryError> {
    let now = now_iso_cn();
    let content = clean_required(req.content, "content")?;
    let scope_id = clean_required(req.scope_id, "scope_id")?;
    let created_by_user_id = clean_required(req.created_by_user_id, "created_by_user_id")?;
    Ok(MemoryRecord {
        id: Uuid::new_v4().to_string(),
        ts: now.clone(),
        created_at: now,
        updated_at: None,
        memory_type: clean_optional(req.memory_type).unwrap_or_else(default_memory_type),
        scope: clean_optional(req.scope).unwrap_or_else(default_scope),
        scope_type: req.scope_type.as_str().to_owned(),
        scope_id: Some(scope_id),
        created_by_user_id: Some(created_by_user_id),
        user_id: clean_optional_option(req.user_id),
        group_id: clean_optional_option(req.group_id),
        content: redact_sensitive_text(&content),
        source_text: redact_sensitive_text(&req.source_text),
    })
}

fn insert_record_unlocked(conn: &Connection, record: &MemoryRecord) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memories (
            id, created_at, updated_at, memory_type, scope,
            scope_type, scope_id, created_by_user_id,
            user_id, group_id, content, source_text
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            record.id.as_str(),
            record.created_at.as_str(),
            record.memory_type.as_str(),
            record.scope.as_str(),
            record.scope_type.as_str(),
            record.scope_id.as_deref(),
            record.created_by_user_id.as_deref(),
            record.user_id.as_deref(),
            record.group_id.as_deref(),
            record.content.as_str(),
            record.source_text.as_str(),
        ],
    )
    .map_err(MemoryError::from_sql)?;
    Ok(())
}

impl UpdateMemoryRequest {
    fn has_update(&self) -> bool {
        self.content.is_some()
            || self.source_text.is_some()
            || self.memory_type.is_some()
            || self.scope.is_some()
    }
}

#[cfg(test)]
mod tests;
