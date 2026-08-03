//! 知识库托管文件的上传、处理、重试和删除领域流程。
//!
//! Handler 只负责协议与认证；这里保证“原始文件已保存”和“知识索引已 ready”严格分离，
//! 并由单独 worker 执行阻塞的 Markdown/FTS/embedding 流程。

use std::time::{Duration, Instant};

use rusqlite::TransactionBehavior;
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::{
    management::{
        ConsoleUserDataError, ConsoleUserDataService, StagedFileDeletion, UserFileContent,
    },
    runtime::tools::knowledge::{
        file_storage::{
            ClaimedKnowledgeFile, KnowledgeFileEntry, KnowledgeFileListQuery, KnowledgeFilePage,
            KnowledgeFileStore, RetryOutcome,
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
            .create_file_with_limit(admin_id, filename, content_type, bytes, self.max_file_bytes)
            .map_err(map_user_data_error)?;
        if let Err(error) = self.store.insert_pending(&file.file_id, &file.created_at) {
            if let Err(cleanup_error) = self.files.delete_file(admin_id, &file.file_id) {
                warn!(
                    file_id = %short_id(&file.file_id),
                    cleanup_code = cleanup_error.code(),
                    "知识库关联写入失败后无法清理原始文件"
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
            .read_file(admin_id, file_id)
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
        if KnowledgeFileStore::find_owned_in_transaction(&transaction, admin_id, file_id)
            .map_err(map_database_error)?
            .is_none()
        {
            transaction
                .commit()
                .map_err(|error| map_database_error(DatabaseError::from_sql(error)))?;
            return Err(KnowledgeFileError::not_found());
        }

        let keep_source = self
            .files
            .has_other_file_references_in_transaction(&transaction, admin_id, file_id)
            .map_err(map_user_data_error)?;
        let staged = if keep_source {
            None
        } else {
            match self
                .files
                .stage_owned_file_deletion(&transaction, admin_id, file_id)
            {
                Ok(staged) => Some(staged),
                Err(error) => return Err(map_user_data_error(error)),
            }
        };
        let transaction_result = (|| {
            if !keep_source {
                self.files
                    .clean_file_references_in_transaction(&transaction, admin_id, file_id)
                    .map_err(map_user_data_error)?;
            }
            self.store
                .delete_managed_in_transaction(&transaction, admin_id, file_id, !keep_source)
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
                        "知识库文件数据库删除已提交，但暂存文件清理失败"
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
        let result = match self.files.read_file(claimed.admin_id, &file_id) {
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
                    .map_err(map_database_error)?;
                if applied {
                    info!(
                        file_id = %short_id(&file_id),
                        status = "ready",
                        chunk_count = result.chunk_count,
                        embedding_count = result.embedding_count,
                        elapsed_ms = started.elapsed().as_millis(),
                        "知识库托管文件处理完成"
                    );
                    Ok(KnowledgeWorkerOutcome::Ready)
                } else {
                    Ok(KnowledgeWorkerOutcome::Cancelled)
                }
            }
            Err(error) => {
                let failure = processing_failure(&error.code);
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
                        "知识库托管文件处理失败"
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
}

impl KnowledgeFileWorker {
    pub(crate) fn new(service: KnowledgeFileService) -> Result<Self, KnowledgeFileError> {
        let recovered = service.recover_processing()?;
        if recovered > 0 {
            info!(
                recovered,
                "知识库托管文件 worker 已恢复遗留 processing 任务"
            );
        }
        Ok(Self { service })
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
                "知识库托管文件 worker 已启动"
            );
            loop {
                ticker.tick().await;
                if let Err(error) = self.run_once().await {
                    warn!(error_code = error, "知识库托管文件 worker 周期失败");
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
        let Some(claimed) = self
            .service
            .store
            .claim_next()
            .map_err(map_database_error)?
        else {
            debug!("知识库托管文件 worker 没有待处理任务");
            return Ok(KnowledgeWorkerStats::default());
        };
        let outcome = self.service.process_claimed(claimed)?;
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
            "知识库删除事务失败后无法恢复原始文件"
        );
    }
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::{
        management::{ConsoleUserDataService, PreferenceValuePatch, UserPreferencesPatch},
        runtime::tools::knowledge::{
            KnowledgeEvidenceStatus, KnowledgeStore,
            file_storage::{KnowledgeFileListQuery, KnowledgeFileSort, KnowledgeFileStatus},
            index::KnowledgeIndex,
        },
        storage::{APP_MIGRATIONS, database::SqliteDatabase},
    };

    struct Fixture {
        _database: SqliteDatabase,
        directory: PathBuf,
        files: ConsoleUserDataService,
        index: KnowledgeIndex,
        service: KnowledgeFileService,
        admin_id: i64,
        other_admin_id: i64,
    }

    impl Fixture {
        fn new() -> Self {
            let (database, directory) =
                SqliteDatabase::open_temp_directory("knowledge-file-lifecycle", APP_MIGRATIONS)
                    .unwrap();
            let connection = database.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO console_admins
                     (username, password_hash, disabled, created_at)
                     VALUES ('knowledge-admin', 'test-hash', 0, 0)",
                    [],
                )
                .unwrap();
            let admin_id = connection.last_insert_rowid();
            connection
                .execute(
                    "INSERT INTO console_admins
                     (username, password_hash, disabled, created_at)
                     VALUES ('knowledge-admin-2', 'test-hash', 0, 0)",
                    [],
                )
                .unwrap();
            let other_admin_id = connection.last_insert_rowid();
            drop(connection);

            let knowledge_dir = directory.join("knowledge");
            fs::create_dir_all(&knowledge_dir).unwrap();
            let files = ConsoleUserDataService::new(database.clone());
            let index = KnowledgeIndex::new(KnowledgeStore::new(database.clone()), knowledge_dir);
            let service =
                KnowledgeFileService::new(files.clone(), index.clone(), 1024 * 1024).unwrap();
            Self {
                _database: database,
                directory,
                files,
                index,
                service,
                admin_id,
                other_admin_id,
            }
        }

        fn query() -> KnowledgeFileListQuery {
            KnowledgeFileListQuery {
                search: String::new(),
                status: None,
                sort: KnowledgeFileSort::UpdatedAt,
                descending: true,
                limit: 100,
                offset: 0,
            }
        }

        async fn process_one(&self) -> KnowledgeWorkerStats {
            KnowledgeFileWorker::new(self.service.clone())
                .unwrap()
                .run_once()
                .await
                .unwrap()
        }
    }

    #[test]
    fn processing_failure_messages_are_stable_and_safe() {
        assert_eq!(processing_failure("invalid_encoding").0, "invalid_encoding");
        assert_eq!(
            processing_failure("knowledge_embedding_error").0,
            "embedding_failed"
        );
        assert!(!processing_failure("unknown_with_path").1.contains('/'));
    }

    #[tokio::test]
    async fn managed_file_becomes_searchable_and_delete_cleans_derived_data() {
        let fixture = Fixture::new();
        let uploaded = fixture
            .service
            .upload(
                fixture.admin_id,
                "managed.md".to_owned(),
                "text/markdown".to_owned(),
                b"# Managed\n\nmanaged-lifecycle-marker".to_vec(),
            )
            .unwrap();
        assert_eq!(uploaded.status, KnowledgeFileStatus::Pending);

        let stats = fixture.process_one().await;
        assert_eq!(stats.claimed, 1);
        assert_eq!(stats.ready, 1);
        let file_id = uploaded.file_id.as_deref().unwrap();
        let ready = fixture
            .service
            .store
            .find_owned(fixture.admin_id, file_id)
            .unwrap()
            .unwrap();
        assert_eq!(ready.status, KnowledgeFileStatus::Ready);
        assert!(ready.chunk_count.unwrap_or_default() > 0);
        assert_eq!(
            fixture
                .index
                .search_evidence("managed-lifecycle-marker")
                .status,
            KnowledgeEvidenceStatus::Ok
        );

        fixture.service.delete(fixture.admin_id, file_id).unwrap();
        assert_eq!(
            fixture
                .index
                .search_evidence("managed-lifecycle-marker")
                .status,
            KnowledgeEvidenceStatus::NoHit
        );
        assert_eq!(
            fixture
                .files
                .read_file(fixture.admin_id, file_id)
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[tokio::test]
    async fn orphaned_managed_document_is_not_treated_as_a_directory_document() {
        let fixture = Fixture::new();
        let uploaded = fixture
            .service
            .upload(
                fixture.admin_id,
                "orphan.md".to_owned(),
                "text/markdown".to_owned(),
                b"# Orphan\n\norphan-managed-marker".to_vec(),
            )
            .unwrap();
        fixture.process_one().await;
        let file_id = uploaded.file_id.as_deref().unwrap();
        fixture
            .service
            .store
            .database()
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM knowledge_managed_files WHERE file_id = ?1",
                [file_id],
            )
            .unwrap();

        assert_eq!(
            fixture
                .index
                .search_evidence("orphan-managed-marker")
                .status,
            KnowledgeEvidenceStatus::NoHit
        );
        assert!(
            !fixture
                .service
                .list(fixture.admin_id, &Fixture::query())
                .unwrap()
                .items
                .iter()
                .any(|entry| entry.filename == "orphan.md")
        );
    }

    #[tokio::test]
    async fn invalid_managed_files_fail_with_safe_codes_and_can_retry() {
        let fixture = Fixture::new();
        let cases = [
            (
                "notes.txt",
                b"not markdown".as_slice(),
                "unsupported_format",
            ),
            ("bad.md", &[0xff, 0xfe][..], "invalid_encoding"),
            ("empty.md", b" \n\t".as_slice(), "empty_document"),
            (
                "settings.example.md",
                b"# example".as_slice(),
                "example_template",
            ),
        ];

        for (filename, bytes, error_code) in cases {
            let uploaded = fixture
                .service
                .upload(
                    fixture.admin_id,
                    filename.to_owned(),
                    "text/markdown".to_owned(),
                    bytes.to_vec(),
                )
                .unwrap();
            let stats = fixture.process_one().await;
            assert_eq!(stats.failed, 1);
            let file_id = uploaded.file_id.as_deref().unwrap();
            let failed = fixture
                .service
                .store
                .find_owned(fixture.admin_id, file_id)
                .unwrap()
                .unwrap();
            assert_eq!(failed.status, KnowledgeFileStatus::Failed);
            assert_eq!(failed.error_code.as_deref(), Some(error_code));
            assert!(
                !failed
                    .error_summary
                    .as_deref()
                    .unwrap_or_default()
                    .contains('/')
            );

            let retried = fixture.service.retry(fixture.admin_id, file_id).unwrap();
            assert_eq!(retried.status, KnowledgeFileStatus::Pending);
            assert_eq!(fixture.process_one().await.failed, 1);
        }

        let oversized = fixture.service.upload(
            fixture.admin_id,
            "oversized.md".to_owned(),
            "text/markdown".to_owned(),
            vec![b'x'; 1024 * 1024 + 1],
        );
        assert_eq!(oversized.unwrap_err().code, "payload_too_large");

        let missing = fixture
            .service
            .upload(
                fixture.admin_id,
                "missing.md".to_owned(),
                "text/markdown".to_owned(),
                b"# Missing source".to_vec(),
            )
            .unwrap();
        let missing_content = fixture
            .files
            .read_file(fixture.admin_id, missing.file_id.as_deref().unwrap())
            .unwrap();
        fs::remove_file(
            fixture
                .files
                .file_root
                .join(missing_content.metadata.storage_filename),
        )
        .unwrap();
        assert_eq!(fixture.process_one().await.failed, 1);
        let missing_failed = fixture
            .service
            .store
            .find_owned(fixture.admin_id, missing.file_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(missing_failed.error_code.as_deref(), Some("source_missing"));
    }

    #[tokio::test]
    async fn claim_is_single_use_and_directory_documents_are_distinguished() {
        let fixture = Fixture::new();
        let legacy_path = fixture.directory.join("knowledge").join("legacy.md");
        fs::write(&legacy_path, "# Legacy\n\nlegacy-directory-marker").unwrap();
        fixture.index.sync().unwrap();

        let uploaded = fixture
            .service
            .upload(
                fixture.admin_id,
                "claim.md".to_owned(),
                "text/markdown".to_owned(),
                b"# Claim\n\nclaim-marker".to_vec(),
            )
            .unwrap();
        let claimed = fixture.service.store.claim_next().unwrap().unwrap();
        assert_eq!(claimed.file_id, uploaded.file_id.as_deref().unwrap());
        assert!(fixture.service.store.claim_next().unwrap().is_none());
        fixture.service.store.recover_processing().unwrap();
        assert_eq!(fixture.process_one().await.ready, 1);

        let page = fixture
            .service
            .list(fixture.admin_id, &Fixture::query())
            .unwrap();
        let legacy = page
            .items
            .iter()
            .find(|entry| entry.filename == "legacy.md")
            .unwrap();
        assert_eq!(legacy.source_kind, "directory");
        assert!(legacy.file_id.is_none());
        assert_eq!(legacy.document_key.as_deref(), Some("legacy.md"));
        assert!(page.items.iter().any(|entry| {
            entry.file_id.as_deref() == Some(uploaded.file_id.as_deref().unwrap())
                && entry.source_kind == "managed"
        }));
    }

    #[test]
    fn managed_file_list_isolated_by_authenticated_admin() {
        let fixture = Fixture::new();
        let first = fixture
            .service
            .upload(
                fixture.admin_id,
                "first.md".to_owned(),
                "text/markdown".to_owned(),
                b"# first".to_vec(),
            )
            .unwrap();
        let second = fixture
            .service
            .upload(
                fixture.other_admin_id,
                "second.md".to_owned(),
                "text/markdown".to_owned(),
                b"# second".to_vec(),
            )
            .unwrap();

        let page = fixture
            .service
            .list(fixture.admin_id, &Fixture::query())
            .unwrap();
        assert!(
            page.items.iter().any(|entry| {
                entry.file_id.as_deref() == Some(first.file_id.as_deref().unwrap())
            })
        );
        assert!(
            !page.items.iter().any(|entry| {
                entry.file_id.as_deref() == Some(second.file_id.as_deref().unwrap())
            })
        );
    }

    #[test]
    fn knowledge_delete_keeps_a_file_used_by_background_preferences() {
        let fixture = Fixture::new();
        let uploaded = fixture
            .service
            .upload(
                fixture.admin_id,
                "also-background.md".to_owned(),
                "text/markdown".to_owned(),
                b"# Shared source".to_vec(),
            )
            .unwrap();
        let file_id = uploaded.file_id.clone().unwrap();
        fixture
            .files
            .update_preferences(
                fixture.admin_id,
                UserPreferencesPatch {
                    background_file_ids: Some(vec![file_id.clone()]),
                    active_background_file_id: PreferenceValuePatch::Set(file_id.clone()),
                    ..UserPreferencesPatch::default()
                },
            )
            .unwrap();

        fixture.service.delete(fixture.admin_id, &file_id).unwrap();
        assert_eq!(
            fixture
                .files
                .read_file(fixture.admin_id, &file_id)
                .unwrap()
                .bytes,
            b"# Shared source"
        );
        let preferences = fixture.files.get_preferences(fixture.admin_id).unwrap();
        assert_eq!(preferences.background_file_ids, vec![file_id.clone()]);
        assert_eq!(
            preferences.active_background_file_id.as_deref(),
            Some(file_id.as_str())
        );
    }
}
