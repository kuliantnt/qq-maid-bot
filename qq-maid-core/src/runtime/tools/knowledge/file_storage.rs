//! 知识库托管文件的状态与列表持久化。
//!
//! 原始字节仍由 `ConsoleUserDataService` 保存；本模块只维护知识用途关联、处理状态和
//! 索引统计，并把托管文档与历史目录文档明确分开。

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{
    management::UserFileModule,
    runtime::tools::knowledge::{index::managed_document_key, storage::KnowledgeStore},
    storage::{
        database::{DatabaseError, SqliteDatabase},
        session::now_iso_cn,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnowledgeFileStatus {
    Pending,
    Processing,
    Ready,
    Failed,
}

impl KnowledgeFileStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "processing" => Some(Self::Processing),
            "ready" => Some(Self::Ready),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnowledgeFileSort {
    UploadedAt,
    UpdatedAt,
}

#[derive(Debug, Clone)]
pub(crate) struct KnowledgeFileListQuery {
    pub search: String,
    pub status: Option<KnowledgeFileStatus>,
    pub sort: KnowledgeFileSort,
    pub descending: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct KnowledgeFileEntry {
    pub file_id: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub size: Option<u64>,
    pub status: KnowledgeFileStatus,
    pub uploaded_at: Option<String>,
    pub processing_started_at: Option<String>,
    pub processed_at: Option<String>,
    pub updated_at: String,
    pub error_code: Option<String>,
    pub error_summary: Option<String>,
    pub chunk_count: Option<u64>,
    pub embedding_count: Option<u64>,
    pub source_kind: &'static str,
    pub source_label: String,
    pub document_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ClaimedKnowledgeFile {
    pub admin_id: i64,
    pub file_id: String,
    pub filename: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub(crate) enum RetryOutcome {
    NotFound,
    NotFailed(KnowledgeFileStatus),
    Reset,
}

#[derive(Debug, Clone)]
pub(crate) struct KnowledgeFilePage {
    pub items: Vec<KnowledgeFileEntry>,
    pub total_count: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct KnowledgeFileStore {
    database: SqliteDatabase,
}

impl KnowledgeFileStore {
    pub(crate) fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    pub(crate) fn database(&self) -> &SqliteDatabase {
        &self.database
    }

    pub(crate) fn insert_pending(
        &self,
        admin_id: i64,
        file_id: &str,
        uploaded_at: &str,
    ) -> Result<(), DatabaseError> {
        let document_key = managed_document_key(file_id);
        let changed = self
            .database
            .connection()?
            .execute(
                "INSERT INTO knowledge_managed_files
                 (file_id, document_key, status, uploaded_at, updated_at)
                 SELECT ?1, ?2, 'pending', ?3, ?3
                 WHERE EXISTS(
                   SELECT 1 FROM console_user_files
                   WHERE admin_id = ?4 AND file_id = ?1 AND module = ?5
                 )",
                params![
                    file_id,
                    document_key,
                    uploaded_at,
                    admin_id,
                    UserFileModule::Knowledge.as_str()
                ],
            )
            .map_err(DatabaseError::from_sql)?;
        if changed != 1 {
            return Err(DatabaseError::from_sql(
                rusqlite::Error::QueryReturnedNoRows,
            ));
        }
        Ok(())
    }

    pub(crate) fn find_owned(
        &self,
        admin_id: i64,
        file_id: &str,
    ) -> Result<Option<KnowledgeFileEntry>, DatabaseError> {
        let connection = self.database.connection()?;
        Self::find_owned_in_transaction(&connection, admin_id, file_id)
    }

    pub(crate) fn find_owned_in_transaction(
        connection: &rusqlite::Connection,
        admin_id: i64,
        file_id: &str,
    ) -> Result<Option<KnowledgeFileEntry>, DatabaseError> {
        connection
            .query_row(
                &managed_file_select(
                    "WHERE f.admin_id = ?1
                       AND f.module = 'knowledge'
                       AND m.file_id = ?2",
                ),
                params![admin_id, file_id],
                managed_entry_from_row,
            )
            .optional()
            .map_err(DatabaseError::from_sql)
    }

    pub(crate) fn list(
        &self,
        admin_id: i64,
        query: &KnowledgeFileListQuery,
    ) -> Result<KnowledgeFilePage, DatabaseError> {
        let connection = self.database.connection()?;
        let status = query
            .status
            .map(KnowledgeFileStatus::as_str)
            .unwrap_or_default();
        let limit = i64::try_from(query.limit).map_err(|_| {
            DatabaseError::from_sql(rusqlite::Error::InvalidParameterName(
                "knowledge file list limit is too large".to_owned(),
            ))
        })?;
        let offset = i64::try_from(query.offset).map_err(|_| {
            DatabaseError::from_sql(rusqlite::Error::InvalidParameterName(
                "knowledge file list offset is too large".to_owned(),
            ))
        })?;
        let order = match (query.sort, query.descending) {
            (KnowledgeFileSort::UploadedAt, false) => "uploaded_at ASC",
            (KnowledgeFileSort::UploadedAt, true) => "uploaded_at DESC",
            (KnowledgeFileSort::UpdatedAt, false) => "updated_at ASC",
            (KnowledgeFileSort::UpdatedAt, true) => "updated_at DESC",
        };
        let cte = knowledge_file_cte();
        let total_count = connection
            .query_row(
                &format!(
                    "WITH files AS ({cte})
                     SELECT COUNT(*) FROM files
                     WHERE (?2 = '' OR lower(filename) LIKE '%' || lower(?2) || '%')
                       AND (?3 = '' OR status = ?3)"
                ),
                params![admin_id, query.search, status],
                |row| row.get::<_, i64>(0),
            )
            .map_err(DatabaseError::from_sql)?;
        let mut statement = connection
            .prepare(&format!(
                "WITH files AS ({cte})
                 SELECT file_id, filename, content_type, size, status,
                        uploaded_at, processing_started_at, processed_at, updated_at,
                        content_hash, error_code, error_summary, chunk_count,
                        embedding_count, source_kind, source_label, document_key
                 FROM files
                 WHERE (?2 = '' OR lower(filename) LIKE '%' || lower(?2) || '%')
                   AND (?3 = '' OR status = ?3)
                 ORDER BY {order}, source_kind ASC,
                          coalesce(file_id, document_key) ASC
                 LIMIT ?4 OFFSET ?5"
            ))
            .map_err(DatabaseError::from_sql)?;
        let items = statement
            .query_map(
                params![admin_id, query.search, status, limit, offset],
                entry_from_row,
            )
            .map_err(DatabaseError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)?;
        Ok(KnowledgeFilePage {
            items,
            total_count: u64::try_from(total_count.max(0))
                .map_err(|_| DatabaseError::from_sql(rusqlite::Error::InvalidQuery))?,
        })
    }

    pub(crate) fn recover_processing(&self) -> Result<usize, DatabaseError> {
        let mut connection = self.database.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(DatabaseError::from_sql)?;
        let mut statement = transaction
            .prepare(
                "SELECT m.document_key
                 FROM knowledge_managed_files m
                 JOIN console_user_files f ON f.file_id = m.file_id
                 WHERE m.status IN ('pending', 'processing', 'failed')
                   AND f.module = 'knowledge'",
            )
            .map_err(DatabaseError::from_sql)?;
        let document_keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(DatabaseError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DatabaseError::from_sql)?;
        drop(statement);
        // 非 ready 状态都不允许保留派生索引：这同时覆盖 worker panic、join failure、
        // mark_ready/mark_failed 写回失败后的下一周期清理，避免用检索过滤掩盖孤立数据。
        for document_key in &document_keys {
            KnowledgeStore::delete_document_in_transaction(&transaction, document_key)?;
        }
        let now = now_iso_cn();
        let changed = transaction
            .execute(
                "UPDATE knowledge_managed_files
                 SET status = 'pending', processing_started_at = NULL,
                     error_code = NULL, error_summary = NULL, updated_at = ?1
                 WHERE status = 'processing'
                   AND EXISTS(
                     SELECT 1 FROM console_user_files
                     WHERE file_id = knowledge_managed_files.file_id
                       AND module = 'knowledge'
                   )",
                [now],
            )
            .map_err(DatabaseError::from_sql)?;
        transaction.commit().map_err(DatabaseError::from_sql)?;
        Ok(changed)
    }

    pub(crate) fn claim_next(&self) -> Result<Option<ClaimedKnowledgeFile>, DatabaseError> {
        let mut connection = self.database.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(DatabaseError::from_sql)?;
        let file_id = transaction
            .query_row(
                "SELECT m.file_id FROM knowledge_managed_files m
                 JOIN console_user_files f ON f.file_id = m.file_id
                 WHERE m.status = 'pending' AND f.module = 'knowledge'
                 ORDER BY m.uploaded_at ASC, m.file_id ASC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(DatabaseError::from_sql)?;
        let Some(file_id) = file_id else {
            transaction.commit().map_err(DatabaseError::from_sql)?;
            return Ok(None);
        };
        let now = now_iso_cn();
        let changed = transaction
            .execute(
                "UPDATE knowledge_managed_files
                 SET status = 'processing', processing_started_at = ?1,
                     error_code = NULL, error_summary = NULL, updated_at = ?1
                 WHERE file_id = ?2 AND status = 'pending'
                   AND EXISTS(
                     SELECT 1 FROM console_user_files
                     WHERE file_id = ?2 AND module = 'knowledge'
                   )",
                params![now, file_id],
            )
            .map_err(DatabaseError::from_sql)?;
        if changed != 1 {
            transaction.commit().map_err(DatabaseError::from_sql)?;
            return Ok(None);
        }
        let claimed = transaction
            .query_row(
                "SELECT f.admin_id, m.file_id, f.original_filename, f.created_at
                 FROM knowledge_managed_files m
                 JOIN console_user_files f ON f.file_id = m.file_id
                 WHERE m.file_id = ?1
                   AND m.status = 'processing'
                   AND f.module = 'knowledge'",
                [file_id],
                |row| {
                    Ok(ClaimedKnowledgeFile {
                        admin_id: row.get(0)?,
                        file_id: row.get(1)?,
                        filename: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .map_err(DatabaseError::from_sql)?;
        transaction.commit().map_err(DatabaseError::from_sql)?;
        Ok(Some(claimed))
    }

    pub(crate) fn mark_ready(
        &self,
        file_id: &str,
        content_hash: &str,
        chunk_count: usize,
        embedding_count: usize,
    ) -> Result<bool, DatabaseError> {
        let now = now_iso_cn();
        let changed = self
            .database
            .connection()?
            .execute(
                "UPDATE knowledge_managed_files
             SET status = 'ready', processed_at = ?1, updated_at = ?1,
                 content_hash = ?2, error_code = NULL, error_summary = NULL,
                 chunk_count = ?3, embedding_count = ?4
             WHERE file_id = ?5 AND status = 'processing'
               AND EXISTS(
                 SELECT 1 FROM console_user_files
                 WHERE file_id = ?5 AND module = 'knowledge'
               )",
                params![
                    now,
                    content_hash,
                    i64::try_from(chunk_count).unwrap_or(i64::MAX),
                    i64::try_from(embedding_count).unwrap_or(i64::MAX),
                    file_id
                ],
            )
            .map_err(DatabaseError::from_sql)?;
        Ok(changed == 1)
    }

    /// mark_ready 未应用时只清理仍属于未 ready 状态的托管文档，避免并发/恢复路径已经
    /// 成功推进到 ready 时误删新索引。事务与状态读取保持同一 SQLite 写锁。
    pub(crate) fn cleanup_lost_claim(&self, file_id: &str) -> Result<bool, DatabaseError> {
        let mut connection = self.database.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(DatabaseError::from_sql)?;
        let status = transaction
            .query_row(
                "SELECT m.status
                 FROM knowledge_managed_files m
                 JOIN console_user_files f ON f.file_id = m.file_id
                 WHERE m.file_id = ?1 AND f.module = 'knowledge'",
                [file_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(DatabaseError::from_sql)?;
        if status.as_deref() == Some("ready") {
            transaction.commit().map_err(DatabaseError::from_sql)?;
            return Ok(false);
        }
        KnowledgeStore::delete_document_in_transaction(
            &transaction,
            &managed_document_key(file_id),
        )?;
        transaction.commit().map_err(DatabaseError::from_sql)?;
        Ok(true)
    }

    pub(crate) fn mark_failed(
        &self,
        file_id: &str,
        error_code: &str,
        error_summary: &str,
    ) -> Result<bool, DatabaseError> {
        let now = now_iso_cn();
        let changed = self
            .database
            .connection()?
            .execute(
                "UPDATE knowledge_managed_files
             SET status = 'failed', processed_at = NULL, updated_at = ?1,
                 error_code = ?2, error_summary = ?3
             WHERE file_id = ?4 AND status = 'processing'
               AND EXISTS(
                 SELECT 1 FROM console_user_files
                 WHERE file_id = ?4 AND module = 'knowledge'
               )",
                params![now, error_code, error_summary, file_id],
            )
            .map_err(DatabaseError::from_sql)?;
        Ok(changed == 1)
    }

    pub(crate) fn reset_failed_for_retry(
        &self,
        admin_id: i64,
        file_id: &str,
    ) -> Result<RetryOutcome, DatabaseError> {
        let mut connection = self.database.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(DatabaseError::from_sql)?;
        let entry = Self::find_owned_in_transaction(&transaction, admin_id, file_id)?;
        let Some(entry) = entry else {
            transaction.commit().map_err(DatabaseError::from_sql)?;
            return Ok(RetryOutcome::NotFound);
        };
        if entry.status != KnowledgeFileStatus::Failed {
            transaction.commit().map_err(DatabaseError::from_sql)?;
            return Ok(RetryOutcome::NotFailed(entry.status));
        }
        KnowledgeStore::delete_document_in_transaction(
            &transaction,
            entry.document_key.as_deref().unwrap_or_default(),
        )?;
        let now = now_iso_cn();
        transaction
            .execute(
                "UPDATE knowledge_managed_files
                 SET status = 'pending', processing_started_at = NULL,
                     processed_at = NULL, updated_at = ?1, content_hash = NULL,
                     error_code = NULL, error_summary = NULL,
                     chunk_count = 0, embedding_count = 0
                 WHERE file_id = ?2 AND status = 'failed'
                   AND EXISTS(
                     SELECT 1 FROM console_user_files
                     WHERE file_id = ?2 AND module = 'knowledge'
                   )",
                params![now, file_id],
            )
            .map_err(DatabaseError::from_sql)?;
        transaction.commit().map_err(DatabaseError::from_sql)?;
        Ok(RetryOutcome::Reset)
    }

    pub(crate) fn delete_managed_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        admin_id: i64,
        file_id: &str,
        delete_source: bool,
    ) -> Result<Option<String>, DatabaseError> {
        let entry = Self::find_owned_in_transaction(transaction, admin_id, file_id)?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let document_key = entry.document_key.clone().unwrap_or_default();
        KnowledgeStore::delete_document_in_transaction(transaction, &document_key)?;
        let deleted = transaction
            .execute(
                "DELETE FROM knowledge_managed_files
                 WHERE file_id = ?1
                   AND EXISTS(
                     SELECT 1 FROM console_user_files
                     WHERE file_id = ?1 AND module = 'knowledge'
                   )",
                [file_id],
            )
            .map_err(DatabaseError::from_sql)?;
        if deleted != 1 {
            return Err(DatabaseError::from_sql(
                rusqlite::Error::QueryReturnedNoRows,
            ));
        }
        if delete_source {
            let deleted = transaction
                .execute(
                    "DELETE FROM console_user_files
                     WHERE admin_id = ?1 AND file_id = ?2 AND module = 'knowledge'",
                    params![admin_id, file_id],
                )
                .map_err(DatabaseError::from_sql)?;
            if deleted != 1 {
                return Err(DatabaseError::from_sql(
                    rusqlite::Error::QueryReturnedNoRows,
                ));
            }
        }
        Ok(Some(document_key))
    }
}

fn managed_file_select(filter: &str) -> String {
    format!(
        "SELECT m.file_id, f.original_filename, f.content_type, f.size,
                m.status, m.uploaded_at, m.processing_started_at, m.processed_at,
                m.updated_at, m.content_hash, m.error_code, m.error_summary,
                m.chunk_count, m.embedding_count, m.document_key
         FROM knowledge_managed_files m
         JOIN console_user_files f ON f.file_id = m.file_id
         {filter}"
    )
}

fn managed_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeFileEntry> {
    Ok(KnowledgeFileEntry {
        file_id: Some(row.get(0)?),
        filename: row.get(1)?,
        content_type: row.get(2)?,
        size: Some(nonnegative_u64(row.get(3)?, 3)?),
        status: status_from_row(row, 4)?,
        uploaded_at: row.get(5)?,
        processing_started_at: row.get(6)?,
        processed_at: row.get(7)?,
        updated_at: row.get(8)?,
        error_code: row.get(10)?,
        error_summary: row.get(11)?,
        chunk_count: nonnegative_optional_u64(row.get(12)?, 12)?,
        embedding_count: nonnegative_optional_u64(row.get(13)?, 13)?,
        source_kind: "managed",
        source_label: row.get(1)?,
        document_key: Some(row.get(14)?),
    })
}

fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeFileEntry> {
    let file_id: Option<String> = row.get(0)?;
    let source_kind: String = row.get(14)?;
    let source_kind = if source_kind == "managed" {
        "managed"
    } else {
        "directory"
    };
    Ok(KnowledgeFileEntry {
        file_id,
        filename: row.get(1)?,
        content_type: row.get(2)?,
        size: nonnegative_optional_u64(row.get(3)?, 3)?,
        status: status_from_row(row, 4)?,
        uploaded_at: row.get(5)?,
        processing_started_at: row.get(6)?,
        processed_at: row.get(7)?,
        updated_at: row.get(8)?,
        error_code: row.get(10)?,
        error_summary: row.get(11)?,
        chunk_count: nonnegative_optional_u64(row.get(12)?, 12)?,
        embedding_count: nonnegative_optional_u64(row.get(13)?, 13)?,
        source_kind,
        source_label: row.get(15)?,
        document_key: row.get(16)?,
    })
}

fn status_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<KnowledgeFileStatus> {
    let raw: String = row.get(index)?;
    KnowledgeFileStatus::parse(&raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            format!("unknown knowledge file status: {raw}").into(),
        )
    })
}

fn nonnegative_u64(value: i64, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn nonnegative_optional_u64(value: Option<i64>, index: usize) -> rusqlite::Result<Option<u64>> {
    value.map(|value| nonnegative_u64(value, index)).transpose()
}

fn knowledge_file_cte() -> String {
    "SELECT m.file_id, f.original_filename AS filename, f.content_type, f.size,
            m.status, m.uploaded_at, m.processing_started_at, m.processed_at,
            m.updated_at, m.content_hash, m.error_code, m.error_summary,
            m.chunk_count, m.embedding_count, 'managed' AS source_kind,
            f.original_filename AS source_label, m.document_key
     FROM knowledge_managed_files m
     JOIN console_user_files f ON f.file_id = m.file_id
     WHERE f.admin_id = ?1 AND f.module = 'knowledge'
     UNION ALL
     SELECT NULL AS file_id, d.relative_path AS filename, 'text/markdown' AS content_type,
            NULL AS size, 'ready' AS status, NULL AS uploaded_at,
            NULL AS processing_started_at, d.indexed_at AS processed_at,
            d.indexed_at AS updated_at, d.file_hash AS content_hash,
            NULL AS error_code, NULL AS error_summary,
            (SELECT COUNT(*) FROM knowledge_chunks c WHERE c.document_id = d.id) AS chunk_count,
            (SELECT COUNT(*) FROM knowledge_chunk_embeddings e
             JOIN knowledge_chunks c ON c.chunk_id = e.chunk_id
             WHERE c.document_id = d.id) AS embedding_count,
            'directory' AS source_kind, d.relative_path AS source_label,
            d.relative_path AS document_key
     FROM knowledge_documents d
     LEFT JOIN knowledge_managed_files m ON m.document_key = d.relative_path
     WHERE d.source_kind = 'directory' AND m.file_id IS NULL"
        .to_owned()
}
