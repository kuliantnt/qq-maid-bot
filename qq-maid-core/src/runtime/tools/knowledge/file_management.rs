//! 知识库托管文件的上传、处理、重试和删除领域流程。
//!
//! Handler 只负责协议与认证；这里保证“原始文件已保存”和“知识索引已 ready”严格分离，
//! 并由单独 worker 执行阻塞的 Markdown/FTS/embedding 流程。

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rusqlite::TransactionBehavior;
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::{
    management::{
        ConsoleUserDataError, ConsoleUserDataService, StagedFileDeletion, UserFileContent,
        UserFileModule,
    },
    runtime::tools::knowledge::{
        file_storage::{
            ClaimedKnowledgeFile, KnowledgeFileEntry, KnowledgeFileListQuery, KnowledgeFilePage,
            KnowledgeFileStatus, KnowledgeFileStore, RetryOutcome,
        },
        index::KnowledgeIndex,
    },
    storage::database::DatabaseError,
};

const WORKER_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct KnowledgeFileError {
    code: &'static str,
    message: String,
}

impl KnowledgeFileError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    fn not_found() -> Self {
        Self {
            code: "not_found",
            message: "knowledge file not found".to_owned(),
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            code: "conflict",
            message: message.into(),
        }
    }

    fn too_large(max_bytes: usize) -> Self {
        Self {
            code: "payload_too_large",
            message: format!("knowledge file must not exceed {max_bytes} bytes"),
        }
    }

    fn storage() -> Self {
        Self {
            code: "storage_error",
            message: "knowledge file storage operation failed".to_owned(),
        }
    }
}

#[derive(Clone)]
pub struct KnowledgeFileService {
    files: ConsoleUserDataService,
    index: KnowledgeIndex,
    store: KnowledgeFileStore,
    max_file_bytes: usize,
}

impl KnowledgeFileService {
    pub(crate) fn new(
        files: ConsoleUserDataService,
        index: KnowledgeIndex,
        max_file_bytes: u64,
    ) -> Result<Self, KnowledgeFileError> {
        let max_file_bytes = usize::try_from(max_file_bytes)
            .map_err(|_| KnowledgeFileError::invalid("knowledge file size limit is invalid"))?;
        let store = KnowledgeFileStore::new(index.database().clone());
        Ok(Self {
            files,
            index,
            store,
            max_file_bytes,
        })
    }

    pub(crate) fn max_file_bytes(&self) -> usize {
        self.max_file_bytes
    }

    pub(crate) fn upload(
        &self,
        admin_id: i64,
        filename: String,
        content_type: String,
        bytes: Vec<u8>,
    ) -> Result<KnowledgeFileEntry, KnowledgeFileError> {
        if bytes.len() > self.max_file_bytes {
            return Err(KnowledgeFileError::too_large(self.max_file_bytes));
        }
        let file = self
            .files
            .create_file_with_limit(
                admin_id,
                filename,
                content_type,
                bytes,
                self.max_file_bytes,
                UserFileModule::Knowledge,
            )
            .map_err(map_user_data_error)?;
        if let Err(error) = self
            .store
            .insert_pending(admin_id, &file.file_id, &file.created_at)
        {
            if let Err(cleanup_error) = self.files.delete_file_for_module(
                admin_id,
                &file.file_id,
                UserFileModule::Knowledge,
            ) {
                warn!(
                    file_id = %short_id(&file.file_id),
                    cleanup_code = cleanup_error.code(),
                    "failed to clean up original file after knowledge link insert failed"
                );
            }
            return Err(map_database_error(error));
        }
        self.store
            .find_owned(admin_id, &file.file_id)
            .map_err(map_database_error)?
            .ok_or_else(KnowledgeFileError::storage)
    }

    pub(crate) fn list(
        &self,
        admin_id: i64,
        query: &KnowledgeFileListQuery,
    ) -> Result<KnowledgeFilePage, KnowledgeFileError> {
        self.store.list(admin_id, query).map_err(map_database_error)
    }

    pub(crate) fn read(
        &self,
        admin_id: i64,
        file_id: &str,
    ) -> Result<UserFileContent, KnowledgeFileError> {
        validate_file_id(file_id)?;
        let exists = self
            .store
            .find_owned(admin_id, file_id)
            .map_err(map_database_error)?
            .is_some();
        if !exists {
            return Err(KnowledgeFileError::not_found());
        }
        self.files
            .read_file_for_module(admin_id, file_id, UserFileModule::Knowledge)
            .map_err(map_user_data_error)
    }

    pub(crate) fn retry(
        &self,
        admin_id: i64,
        file_id: &str,
    ) -> Result<KnowledgeFileEntry, KnowledgeFileError> {
        validate_file_id(file_id)?;
        match self
            .store
            .reset_failed_for_retry(admin_id, file_id)
            .map_err(map_database_error)?
        {
            RetryOutcome::NotFound => Err(KnowledgeFileError::not_found()),
            RetryOutcome::NotFailed(status) => Err(KnowledgeFileError::conflict(format!(
                "only failed knowledge files can be retried; current status is {}",
                status.as_str()
            ))),
            RetryOutcome::Reset => self
                .store
                .find_owned(admin_id, file_id)
                .map_err(map_database_error)?
                .ok_or_else(KnowledgeFileError::storage),
        }
    }

    pub(crate) fn delete(&self, admin_id: i64, file_id: &str) -> Result<(), KnowledgeFileError> {
        validate_file_id(file_id)?;
        let mut connection = self
            .store
            .database()
            .connection()
            .map_err(map_database_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| map_database_error(DatabaseError::from_sql(error)))?;
        let Some(entry) =
            KnowledgeFileStore::find_owned_in_transaction(&transaction, admin_id, file_id)
                .map_err(map_database_error)?
        else {
            transaction
                .commit()
                .map_err(|error| map_database_error(DatabaseError::from_sql(error)))?;
            return Err(KnowledgeFileError::not_found());
        };
        if entry.status == KnowledgeFileStatus::Processing {
            return Err(KnowledgeFileError::conflict(
                "knowledge file is processing and cannot be deleted",
            ));
        }
        let staged = match self.files.stage_owned_file_deletion(
            &transaction,
            admin_id,
            file_id,
            UserFileModule::Knowledge,
        ) {
            Ok(staged) => staged,
            Err(error) => return Err(map_user_data_error(error)),
        };
        let transaction_result = (|| {
            self.files
                .clean_file_references_in_transaction(&transaction, admin_id, file_id)
                .map_err(map_user_data_error)?;
            self.store
                .delete_managed_in_transaction(&transaction, admin_id, file_id, true)
                .map_err(map_database_error)?
                .ok_or_else(KnowledgeFileError::not_found)?;
            transaction
                .commit()
                .map_err(|error| map_database_error(DatabaseError::from_sql(error)))
        })();
        if let Err(error) = transaction_result {
            if let Some(staged) = staged.as_ref() {
                restore_staged_file(&self.files, staged, file_id);
            }
            return Err(error);
        }
        if let Some(staged) = staged.as_ref() {
            self.files
                .finish_staged_file_deletion(staged)
                .map_err(|error| {
                    warn!(
                        file_id = %short_id(file_id),
                        error_code = error.code(),
                        "knowledge file db delete committed but staged file cleanup failed"
                    );
                    KnowledgeFileError::storage()
                })?;
        }
        Ok(())
    }

    pub(crate) fn recover_processing(&self) -> Result<usize, KnowledgeFileError> {
        self.store.recover_processing().map_err(map_database_error)
    }

    fn process_claimed(
        &self,
        claimed: ClaimedKnowledgeFile,
    ) -> Result<KnowledgeWorkerOutcome, KnowledgeFileError> {
        let started = Instant::now();
        let file_id = claimed.file_id.clone();
        let result = match self.files.read_file_for_module(
            claimed.admin_id,
            &file_id,
            UserFileModule::Knowledge,
        ) {
            Ok(content) => self.index.process_managed_file(
                &file_id,
                &claimed.filename,
                &content.bytes,
                self.max_file_bytes,
                Some(&claimed.created_at),
            ),
            Err(_) => Err(crate::error::LlmError::new(
                "source_missing",
                "knowledge source file is unavailable",
                "knowledge",
            )),
        };
        match result {
            Ok(result) => {
                let applied = self
                    .store
                    .mark_ready(
                        &file_id,
                        &result.content_hash,
                        result.chunk_count,
                        result.embedding_count,
                    )
                    .map_err(|error| {
                        if let Err(cleanup_error) = self.store.cleanup_lost_claim(&file_id) {
                            warn!(
                                file_id = %short_id(&file_id),
                                error_code = cleanup_error.code(),
                                status = "processing",
                                "failed to clean up index after mark_ready failure; a later cycle will recover it"
                            );
                        }
                        map_database_error(error)
                    })?;
                if applied {
                    info!(
                        file_id = %short_id(&file_id),
                        status = "ready",
                        chunk_count = result.chunk_count,
                        embedding_count = result.embedding_count,
                        elapsed_ms = started.elapsed().as_millis(),
                        "knowledge managed file processing completed"
                    );
                    Ok(KnowledgeWorkerOutcome::Ready)
                } else {
                    if let Err(cleanup_error) = self.store.cleanup_lost_claim(&file_id) {
                        warn!(
                            file_id = %short_id(&file_id),
                            error_code = cleanup_error.code(),
                            status = "cancelled",
                            "failed to clean up index after mark_ready not applied; a later cycle will recover it"
                        );
                        return Err(map_database_error(cleanup_error));
                    }
                    warn!(
                        file_id = %short_id(&file_id),
                        status = "cancelled",
                        elapsed_ms = started.elapsed().as_millis(),
                        "knowledge managed file claim became stale; derived index cleaned up"
                    );
                    Ok(KnowledgeWorkerOutcome::Cancelled)
                }
            }
            Err(error) => {
                let failure = processing_failure(&error.code);
                if let Err(cleanup_error) = self.store.cleanup_lost_claim(&file_id) {
                    warn!(
                        file_id = %short_id(&file_id),
                        error_code = cleanup_error.code(),
                        status = "processing",
                        "failed to clean up derived index after processing failure; a later cycle will recover it"
                    );
                }
                let applied = self
                    .store
                    .mark_failed(&file_id, failure.0, failure.1)
                    .map_err(map_database_error)?;
                if applied {
                    warn!(
                        file_id = %short_id(&file_id),
                        status = "failed",
                        error_code = failure.0,
                        elapsed_ms = started.elapsed().as_millis(),
                        "knowledge managed file processing failed"
                    );
                    Ok(KnowledgeWorkerOutcome::Failed)
                } else {
                    Ok(KnowledgeWorkerOutcome::Cancelled)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KnowledgeWorkerStats {
    pub claimed: usize,
    pub ready: usize,
    pub failed: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnowledgeWorkerOutcome {
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone)]
pub struct KnowledgeFileWorker {
    service: KnowledgeFileService,
    // 同一个 worker 的周期必须串行；这样周期开始的 processing 恢复不会重置仍在执行的任务。
    cycle_lock: Arc<Mutex<()>>,
}

impl KnowledgeFileWorker {
    pub(crate) fn new(service: KnowledgeFileService) -> Result<Self, KnowledgeFileError> {
        let recovered = service.recover_processing()?;
        if recovered > 0 {
            info!(
                recovered,
                "knowledge managed file worker recovered leftover processing tasks"
            );
        }
        Ok(Self {
            service,
            cycle_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            let mut ticker = interval_at(
                tokio::time::Instant::now() + WORKER_INTERVAL,
                WORKER_INTERVAL,
            );
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            info!(
                interval_seconds = WORKER_INTERVAL.as_secs(),
                "knowledge managed file worker started"
            );
            loop {
                ticker.tick().await;
                if let Err(error) = self.run_once().await {
                    warn!(
                        error_code = error,
                        "knowledge managed file worker cycle failed"
                    );
                }
            }
        });
    }

    pub(crate) async fn run_once(&self) -> Result<KnowledgeWorkerStats, String> {
        let worker = self.clone();
        tokio::task::spawn_blocking(move || worker.run_once_blocking())
            .await
            .map_err(|_| "knowledge_worker_join_failed".to_owned())?
            .map_err(|error| error.code.to_owned())
    }

    fn run_once_blocking(&self) -> Result<KnowledgeWorkerStats, KnowledgeFileError> {
        let _cycle_guard = self
            .cycle_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let recovered = self.service.recover_processing()?;
        if recovered > 0 {
            info!(
                recovered,
                "knowledge managed file worker cycle recovered leftover processing tasks"
            );
        }
        let Some(claimed) = self
            .service
            .store
            .claim_next()
            .map_err(map_database_error)?
        else {
            debug!("知识库托管文件 worker 没有待处理任务");
            return Ok(KnowledgeWorkerStats::default());
        };
        let started = Instant::now();
        let outcome = match self.service.process_claimed(claimed.clone()) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.best_effort_mark_claimed_failed(&claimed, &error, started);
                return Err(error);
            }
        };
        let mut stats = KnowledgeWorkerStats {
            claimed: 1,
            ..KnowledgeWorkerStats::default()
        };
        match outcome {
            KnowledgeWorkerOutcome::Ready => stats.ready = 1,
            KnowledgeWorkerOutcome::Failed => stats.failed = 1,
            KnowledgeWorkerOutcome::Cancelled => stats.cancelled = 1,
        }
        Ok(stats)
    }

    fn best_effort_mark_claimed_failed(
        &self,
        claimed: &ClaimedKnowledgeFile,
        error: &KnowledgeFileError,
        started: Instant,
    ) {
        let failure = processing_failure(error.code());
        match self
            .service
            .store
            .mark_failed(&claimed.file_id, failure.0, failure.1)
        {
            Ok(true) => warn!(
                file_id = %short_id(&claimed.file_id),
                status = "failed",
                error_code = failure.0,
                elapsed_ms = started.elapsed().as_millis(),
                "knowledge managed file failure status written back after error"
            ),
            Ok(false) => debug!(
                file_id = %short_id(&claimed.file_id),
                status = "not_processing",
                error_code = failure.0,
                elapsed_ms = started.elapsed().as_millis(),
                "knowledge managed file failure status not applied after error"
            ),
            Err(status_error) => warn!(
                file_id = %short_id(&claimed.file_id),
                status = "processing",
                error_code = failure.0,
                recovery_code = status_error.code(),
                elapsed_ms = started.elapsed().as_millis(),
                "failed to write back failure status after knowledge managed file error; a later cycle will recover it"
            ),
        }
    }
}

fn validate_file_id(file_id: &str) -> Result<(), KnowledgeFileError> {
    let parsed = uuid::Uuid::parse_str(file_id)
        .map_err(|_| KnowledgeFileError::invalid("file_id must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != file_id {
        return Err(KnowledgeFileError::invalid(
            "file_id must be a canonical UUID",
        ));
    }
    Ok(())
}

fn map_user_data_error(error: ConsoleUserDataError) -> KnowledgeFileError {
    match error.code() {
        "bad_request" => KnowledgeFileError::invalid(error.message()),
        "not_found" => KnowledgeFileError::not_found(),
        _ => KnowledgeFileError::storage(),
    }
}

fn map_database_error(_error: DatabaseError) -> KnowledgeFileError {
    KnowledgeFileError::storage()
}

fn processing_failure(code: &str) -> (&'static str, &'static str) {
    match code {
        "file_too_large" => ("file_too_large", "知识库文件超过当前配置的大小上限"),
        "unsupported_format" => (
            "unsupported_format",
            "仅支持 Markdown 文件（.md 或 .markdown）",
        ),
        "example_template" => ("example_template", "示例模板文件不能加入知识库"),
        "invalid_encoding" => ("invalid_encoding", "文件不是有效的 UTF-8 文本"),
        "empty_document" => ("empty_document", "Markdown 文档为空或没有可索引内容"),
        "too_many_chunks" => ("too_many_chunks", "Markdown 文档切片数量超过安全上限"),
        "source_missing" => ("source_missing", "原始文件不存在或无法读取"),
        code if code.starts_with("knowledge_embedding") => {
            ("embedding_failed", "知识库向量处理失败")
        }
        code if code.starts_with("knowledge_db") => ("index_unavailable", "知识库索引存储不可用"),
        _ => ("storage_error", "知识库处理失败，请稍后重试"),
    }
}

fn restore_staged_file(files: &ConsoleUserDataService, staged: &StagedFileDeletion, file_id: &str) {
    if let Err(error) = files.restore_staged_file_deletion(staged) {
        warn!(
            file_id = %short_id(file_id),
            error_code = error.code(),
            "failed to restore original file after knowledge delete transaction failed"
        );
    }
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
#[path = "file_management_tests.rs"]
mod file_management_tests;
