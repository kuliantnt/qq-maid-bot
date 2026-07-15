//! 长期记忆（Memory）领域内持久化门面。
//!
//! `MemoryStore` 只接收应用启动阶段已经执行 migration 的数据库句柄。v3 schema、
//! 强类型模型和原子事务分别位于相邻模块；权限与冲突语义由 runtime/tools/memory
//! 领域层负责，storage 仅提供精确查询、持久化和事务能力。

use qq_maid_common::{redaction::redact_sensitive_text, time_context::now_iso_cn};
use rusqlite::{Connection, params};
use serde::Serialize;
use uuid::Uuid;

use crate::storage::database::{DatabaseError, SqliteDatabase};

use super::types::MemoryRecall;

const PRIVATE_PERSONAL_RECORD_LIMIT: usize = 12;
const GROUP_LAYER_RECORD_LIMIT: usize = 4;
const PRIVATE_PERSONAL_VISIBILITIES: &[MemoryVisibility] = &[
    MemoryVisibility::Private,
    MemoryVisibility::ContextOnly,
    MemoryVisibility::Public,
];
const GROUP_PERSONAL_VISIBILITIES: &[MemoryVisibility] =
    &[MemoryVisibility::ContextOnly, MemoryVisibility::Public];
const GROUP_LAYER_VISIBILITIES: &[MemoryVisibility] = &[
    MemoryVisibility::GroupMembers,
    MemoryVisibility::ContextOnly,
    MemoryVisibility::Public,
];

mod clean;
mod query;
mod row;
mod schema;
mod types;
mod v3;

pub use schema::{
    MEMORY_DOMAIN_SCHEMA_V3, MEMORY_MIGRATIONS, MEMORY_SCHEMA_V1, MEMORY_SCOPE_SCHEMA_V2,
};
pub use types::{
    CreateMemoryRequest, CreateScopedMemoryRequest, ListMemoryQuery, MemoryCategory, MemoryKind,
    MemoryQuery, MemoryRecord, MemoryScopeType, MemorySourceType, MemoryStatus, MemoryTarget,
    MemoryVisibility, ScopedMemoryQuery, UpdateMemoryRequest,
};
pub(crate) use types::{PersistMemoryRequest, PersistMemoryResult, ReplaceScopedStorageRequest};

#[cfg(test)]
use clean::infer_legacy_scope_identity;
use clean::{
    clean_optional, clean_optional_option, clean_optional_str, clean_required, clean_scope_id,
    default_memory_type, default_scope,
};
use query::{
    apply_update_to_record, list_recall_layer_unlocked, list_scoped_unlocked,
    resolve_memory_id_scoped_unlocked, update_record_unlocked,
};
#[cfg(test)]
use query::{list_unlocked, resolve_memory_id_unlocked};
use row::{get_by_id_scoped_unlocked, get_by_id_unlocked};

#[derive(Debug, Clone, Serialize)]
pub struct MemoryErrorInfo {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryItemResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryListResponse {
    pub ok: bool,
    pub memories: Vec<MemoryRecord>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryDeleteResponse {
    pub ok: bool,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<MemoryErrorInfo>,
}

#[derive(Debug, Clone)]
pub struct MemoryError {
    code: &'static str,
    message: String,
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    database: SqliteDatabase,
}

impl MemoryStore {
    pub(crate) fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 兼容旧入口：仅根据已有 user/group 字段保守推导访问边界。
    #[cfg(test)]
    pub(crate) fn create(&self, req: CreateMemoryRequest) -> Result<MemoryRecord, MemoryError> {
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

    /// 兼容现有 respond flow 的明确边界创建；新业务使用 Memory 领域操作。
    pub(crate) fn create_scoped(
        &self,
        req: CreateScopedMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        let record = build_scoped_record(req)?;
        let conn = self.connection()?;
        insert_record_unlocked(&conn, &record)?;
        get_by_id_unlocked(&conn, &record.id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after insert"))
    }

    /// 兼容旧“替换=删除旧记录并创建新 ID”语义；领域层先完成权限校验。
    pub(crate) fn replace_scoped(
        &self,
        req: ReplaceScopedStorageRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        let scope_id = clean_scope_id(&req.scope_id)?;
        let new_record = build_scoped_record(CreateScopedMemoryRequest {
            scope_type: req.scope_type,
            scope_id: scope_id.clone(),
            created_by_user_id: req.created_by_user_id,
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
        insert_record_unlocked(&tx, &new_record)?;
        let changed = tx
            .execute(
                "DELETE FROM memories
                 WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3",
                params![old_id, req.scope_type.as_str(), scope_id],
            )
            .map_err(MemoryError::from_sql)?;
        if changed == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(new_record)
    }

    #[cfg(test)]
    pub(crate) fn list(&self, query: ListMemoryQuery) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_unlocked(&conn, &query)
    }

    /// 旧 scoped 读取只返回同名 personal/group 范围，不会把 group_profile 混入。
    pub(crate) fn list_scoped(
        &self,
        query: ScopedMemoryQuery,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        list_scoped_unlocked(&conn, &query)
    }

    pub(crate) fn list_v3(&self, query: MemoryQuery) -> Result<Vec<MemoryRecord>, MemoryError> {
        let conn = self.connection()?;
        query::list_v3_unlocked(&conn, &query)
    }

    /// 按场景分别执行 personal、group_profile 和 group 查询。
    ///
    /// 可见性集合在领域层固定为 SQL 条件，未授权记录不会先进入 Rust 合并或模型上下文。
    pub(crate) fn recall_for_context(
        &self,
        personal_scope_id: Option<&str>,
        group_scope_id: Option<&str>,
        shared_conversation: bool,
    ) -> Result<MemoryRecall, MemoryError> {
        let conn = self.connection()?;
        let (personal_visibilities, personal_record_limit) = if shared_conversation {
            (GROUP_PERSONAL_VISIBILITIES, GROUP_LAYER_RECORD_LIMIT)
        } else {
            (PRIVATE_PERSONAL_VISIBILITIES, PRIVATE_PERSONAL_RECORD_LIMIT)
        };
        let personal = personal_scope_id.and_then(clean_optional_str).map_or_else(
            || Ok(Vec::new()),
            |scope_id| {
                list_recall_layer_unlocked(
                    &conn,
                    MemoryScopeType::Personal,
                    &scope_id,
                    MemoryKind::Personal,
                    None,
                    personal_visibilities,
                    personal_record_limit,
                )
            },
        )?;
        let Some(group_scope_id) = group_scope_id.and_then(clean_optional_str) else {
            return Ok(MemoryRecall {
                personal,
                ..MemoryRecall::default()
            });
        };
        let group = list_recall_layer_unlocked(
            &conn,
            MemoryScopeType::Group,
            &group_scope_id,
            MemoryKind::Group,
            None,
            GROUP_LAYER_VISIBILITIES,
            GROUP_LAYER_RECORD_LIMIT,
        )?;
        let group_profile = personal_scope_id.and_then(clean_optional_str).map_or_else(
            || Ok(Vec::new()),
            |subject_id| {
                list_recall_layer_unlocked(
                    &conn,
                    MemoryScopeType::Group,
                    &group_scope_id,
                    MemoryKind::GroupProfile,
                    Some(&subject_id),
                    GROUP_LAYER_VISIBILITIES,
                    GROUP_LAYER_RECORD_LIMIT,
                )
            },
        )?;
        Ok(MemoryRecall {
            group,
            group_profile,
            personal,
        })
    }

    #[cfg(test)]
    pub(crate) fn list_accessible_for_context(
        &self,
        personal_scope_id: Option<&str>,
        group_scope_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, MemoryError> {
        let recall =
            self.recall_for_context(personal_scope_id, group_scope_id, group_scope_id.is_some())?;
        let mut records = Vec::new();
        records.extend(recall.group);
        records.extend(recall.group_profile);
        records.extend(recall.personal);
        records.truncate(limit.clamp(1, 100));
        Ok(records)
    }

    #[cfg(test)]
    pub(crate) fn get(&self, id_or_prefix: &str) -> Result<MemoryRecord, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        get_by_id_unlocked(&conn, &id)?.ok_or_else(|| MemoryError::not_found("memory not found"))
    }

    pub(crate) fn get_scoped(
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

    #[cfg(test)]
    pub(crate) fn update(
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

    pub(crate) fn update_scoped(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id_or_prefix: &str,
        req: UpdateMemoryRequest,
    ) -> Result<MemoryRecord, MemoryError> {
        if !req.has_update() {
            return Err(MemoryError::bad_request("no memory update fields provided"));
        }
        let conn = self.connection()?;
        let id = resolve_memory_id_scoped_unlocked(&conn, scope_type, scope_id, id_or_prefix)?;
        let mut record = get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
            .ok_or_else(|| MemoryError::not_found("memory not found"))?;
        apply_update_to_record(&mut record, req)?;
        update_record_unlocked(&conn, &record)?;
        get_by_id_scoped_unlocked(&conn, scope_type, scope_id, &id)?
            .ok_or_else(|| MemoryError::io("memory disappeared after update"))
    }

    #[cfg(test)]
    pub(crate) fn delete(&self, id_or_prefix: &str) -> Result<String, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_unlocked(&conn, id_or_prefix)?;
        if conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .map_err(MemoryError::from_sql)?
            == 0
        {
            return Err(MemoryError::not_found("memory not found"));
        }
        Ok(id)
    }

    pub(crate) fn delete_scoped(
        &self,
        scope_type: MemoryScopeType,
        scope_id: &str,
        id_or_prefix: &str,
    ) -> Result<String, MemoryError> {
        let conn = self.connection()?;
        let id = resolve_memory_id_scoped_unlocked(&conn, scope_type, scope_id, id_or_prefix)?;
        if conn
            .execute(
                "DELETE FROM memories WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3",
                params![id, scope_type.as_str(), clean_scope_id(scope_id)?],
            )
            .map_err(MemoryError::from_sql)?
            == 0
        {
            return Err(MemoryError::not_found("memory not found"));
        }
        Ok(id)
    }

    pub(super) fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, MemoryError> {
        self.database
            .connection()
            .map_err(MemoryError::from_database)
    }

    #[cfg(test)]
    pub fn drop_schema_for_test(&self) -> Result<(), MemoryError> {
        self.connection()?
            .execute("DROP TABLE memories", [])
            .map_err(MemoryError::from_sql)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn abort_memory_insert_for_test(&self) -> Result<(), MemoryError> {
        self.connection()?
            .execute_batch(
                "CREATE TRIGGER abort_memory_insert_for_test
                 BEFORE INSERT ON memories
                 BEGIN SELECT RAISE(ABORT, 'memory insert aborted for test'); END;",
            )
            .map_err(MemoryError::from_sql)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn abort_memory_archive_for_test(&self) -> Result<(), MemoryError> {
        self.connection()?
            .execute_batch(
                "CREATE TRIGGER abort_memory_archive_for_test
                 BEFORE UPDATE OF status ON memories
                 WHEN NEW.status = 'archived'
                 BEGIN SELECT RAISE(ABORT, 'memory archive aborted for test'); END;",
            )
            .map_err(MemoryError::from_sql)?;
        Ok(())
    }
}

impl MemoryItemResponse {
    pub fn ok(memory: MemoryRecord) -> Self {
        Self {
            ok: true,
            memory: Some(memory),
            error: None,
        }
    }

    pub fn error(err: MemoryError) -> Self {
        Self {
            ok: false,
            memory: None,
            error: Some(err.into_info()),
        }
    }
}

impl MemoryListResponse {
    pub fn ok(memories: Vec<MemoryRecord>) -> Self {
        Self {
            count: memories.len(),
            ok: true,
            memories,
            error: None,
        }
    }

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
    pub fn ok(id: String) -> Self {
        Self {
            ok: true,
            deleted: true,
            id: Some(id),
            error: None,
        }
    }

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
    pub fn code(&self) -> &str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn is_not_found_or_forbidden(&self) -> bool {
        matches!(self.code, "not_found" | "forbidden")
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
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

    fn profile_opted_out() -> Self {
        Self {
            code: "profile_opted_out",
            message: "group profile storage is disabled for this subject".to_owned(),
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

fn build_scoped_record(req: CreateScopedMemoryRequest) -> Result<MemoryRecord, MemoryError> {
    let now = now_iso_cn();
    let content = clean_required(req.content, "content")?;
    let scope_id = clean_required(req.scope_id, "scope_id")?;
    let created_by_user_id = clean_required(req.created_by_user_id, "created_by_user_id")?;
    let (memory_kind, visibility, status) = match req.scope_type {
        MemoryScopeType::Personal => (
            MemoryKind::Personal,
            MemoryVisibility::Private,
            MemoryStatus::Active,
        ),
        MemoryScopeType::Group => (
            MemoryKind::Group,
            MemoryVisibility::GroupMembers,
            MemoryStatus::Active,
        ),
        MemoryScopeType::LegacyUnassigned => (
            MemoryKind::LegacyUnassigned,
            MemoryVisibility::Private,
            MemoryStatus::Archived,
        ),
    };
    Ok(MemoryRecord {
        id: Uuid::new_v4().to_string(),
        ts: now.clone(),
        created_at: now.clone(),
        updated_at: None,
        memory_type: clean_optional(req.memory_type).unwrap_or_else(default_memory_type),
        scope: clean_optional(req.scope).unwrap_or_else(default_scope),
        scope_type: req.scope_type.as_str().to_owned(),
        scope_id: Some(scope_id),
        created_by_user_id: Some(created_by_user_id),
        memory_kind,
        subject_id: None,
        relation_subject_id: None,
        relation_object_id: None,
        visibility,
        source_type: MemorySourceType::UserConfirmed,
        source_ref: None,
        last_confirmed_at: (status == MemoryStatus::Active).then_some(now),
        status,
        pinned: false,
        attribute_key: None,
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
            user_id, group_id, content, source_text,
            memory_kind, subject_id, relation_subject_id, relation_object_id,
            visibility, source_type, source_ref, last_confirmed_at,
            status, pinned, attribute_key
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        params![
            record.id,
            record.created_at,
            record.updated_at,
            record.memory_type,
            record.scope,
            record.scope_type,
            record.scope_id,
            record.created_by_user_id,
            record.user_id,
            record.group_id,
            record.content,
            record.source_text,
            record.memory_kind.as_str(),
            record.subject_id,
            record.relation_subject_id,
            record.relation_object_id,
            record.visibility.as_str(),
            record.source_type.as_str(),
            record.source_ref,
            record.last_confirmed_at,
            record.status.as_str(),
            record.pinned,
            record.attribute_key,
        ],
    )
    .map_err(MemoryError::from_sql)?;
    Ok(())
}

#[cfg(test)]
mod tests;
