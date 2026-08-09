use std::{fs, path::PathBuf};

use rusqlite::params;

use super::*;
use crate::{
    management::{
        ConsoleUserDataService, PreferenceValuePatch, UserFileModule, UserPreferencesPatch,
    },
    runtime::tools::knowledge::{
        KnowledgeEvidenceStatus, KnowledgeStore,
        file_storage::{KnowledgeFileListQuery, KnowledgeFileSort, KnowledgeFileStatus},
        index::{KnowledgeIndex, managed_document_key},
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
        let service = KnowledgeFileService::new(files.clone(), index.clone(), 1024 * 1024).unwrap();
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

#[derive(Debug, Default, PartialEq, Eq)]
struct DerivedIndexCounts {
    documents: i64,
    chunks: i64,
    fts_rows: i64,
    embeddings: i64,
}

fn derived_index_counts(fixture: &Fixture, file_id: &str) -> DerivedIndexCounts {
    let document_key = managed_document_key(file_id);
    let connection = fixture._database.connection().unwrap();
    let documents = connection
        .query_row(
            "SELECT COUNT(*) FROM knowledge_documents WHERE relative_path = ?1",
            [&document_key],
            |row| row.get(0),
        )
        .unwrap();
    let chunks = connection
        .query_row(
            "SELECT COUNT(*)
                 FROM knowledge_chunks c
                 JOIN knowledge_documents d ON d.id = c.document_id
                 WHERE d.relative_path = ?1",
            [&document_key],
            |row| row.get(0),
        )
        .unwrap();
    let fts_rows = connection
        .query_row(
            "SELECT COUNT(*) FROM knowledge_chunks_fts
                 WHERE rowid IN (
                   SELECT c.row_id
                   FROM knowledge_chunks c
                   JOIN knowledge_documents d ON d.id = c.document_id
                   WHERE d.relative_path = ?1
                 )",
            [&document_key],
            |row| row.get(0),
        )
        .unwrap();
    let embeddings = connection
        .query_row(
            "SELECT COUNT(*)
                 FROM knowledge_chunk_embeddings e
                 JOIN knowledge_chunks c ON c.chunk_id = e.chunk_id
                 JOIN knowledge_documents d ON d.id = c.document_id
                 WHERE d.relative_path = ?1",
            [&document_key],
            |row| row.get(0),
        )
        .unwrap();
    DerivedIndexCounts {
        documents,
        chunks,
        fts_rows,
        embeddings,
    }
}

fn insert_test_embedding(fixture: &Fixture, file_id: &str) {
    let document_key = managed_document_key(file_id);
    let connection = fixture._database.connection().unwrap();
    let (chunk_id, content_hash): (String, String) = connection
        .query_row(
            "SELECT c.chunk_id, c.content_hash
                 FROM knowledge_chunks c
                 JOIN knowledge_documents d ON d.id = c.document_id
                 WHERE d.relative_path = ?1
                 LIMIT 1",
            [&document_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO knowledge_chunk_embeddings
                   (chunk_id, model, dimensions, embedding_version,
                    content_hash, vector, updated_at)
                 VALUES (?1, 'test-model', 2, 1, ?2, ?3, '2026-01-01T00:00:00Z')",
            params![chunk_id, content_hash, vec![0_u8; 8]],
        )
        .unwrap();
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
            "deployment-handbook-642.md".to_owned(),
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
    assert_eq!(
        fixture
            .index
            .search_evidence("deployment-handbook-642")
            .status,
        KnowledgeEvidenceStatus::Ok
    );
    insert_test_embedding(&fixture, file_id);
    let listed = fixture
        .service
        .list(fixture.admin_id, &Fixture::query())
        .unwrap();
    let listed_file = listed
        .items
        .iter()
        .find(|entry| entry.file_id.as_deref() == Some(file_id))
        .unwrap();
    // 托管表初始记录为 0 时，后续回填的 embedding 也应从派生表实时统计。
    assert_eq!(listed_file.embedding_count, Some(1));
    let indexed_counts = derived_index_counts(&fixture, file_id);
    assert!(indexed_counts.documents > 0);
    assert!(indexed_counts.chunks > 0);
    assert!(indexed_counts.fts_rows > 0);
    assert!(indexed_counts.embeddings > 0);

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
    assert_eq!(
        derived_index_counts(&fixture, file_id),
        DerivedIndexCounts::default()
    );
}

#[test]
fn background_and_knowledge_file_domains_are_isolated() {
    let fixture = Fixture::new();
    let background = fixture
        .files
        .create_file(
            fixture.admin_id,
            "background.webp".to_owned(),
            "image/webp".to_owned(),
            b"background".to_vec(),
        )
        .unwrap();
    let knowledge = fixture
        .service
        .upload(
            fixture.admin_id,
            "knowledge.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Knowledge\n\nmanaged-domain-marker".to_vec(),
        )
        .unwrap();
    let knowledge_id = knowledge.file_id.as_deref().unwrap();

    let backgrounds = fixture.files.list_files(fixture.admin_id, 100, 0).unwrap();
    assert_eq!(backgrounds.total_count, 1);
    assert_eq!(backgrounds.items[0].file_id, background.file_id);
    assert_eq!(backgrounds.items[0].module.as_str(), "background");
    assert_eq!(
        fixture
            .files
            .read_file(fixture.admin_id, knowledge_id)
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        fixture
            .files
            .delete_file(fixture.admin_id, knowledge_id)
            .unwrap_err()
            .code(),
        "not_found"
    );

    let knowledge_page = fixture
        .service
        .list(fixture.admin_id, &Fixture::query())
        .unwrap();
    assert!(knowledge_page.items.iter().any(|entry| {
        entry.file_id.as_deref() == Some(knowledge_id) && entry.source_kind == "managed"
    }));
    assert!(
        !knowledge_page
            .items
            .iter()
            .any(|entry| entry.file_id.as_deref() == Some(&background.file_id))
    );
    assert_eq!(
        fixture
            .service
            .read(fixture.admin_id, &background.file_id)
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        fixture
            .service
            .delete(fixture.admin_id, &background.file_id)
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[test]
fn legacy_background_mime_remains_selectable() {
    let fixture = Fixture::new();
    let legacy = fixture
        .files
        .create_file(
            fixture.admin_id,
            "legacy-background.heic".to_owned(),
            "application/octet-stream".to_owned(),
            b"legacy-background".to_vec(),
        )
        .unwrap();

    fixture
        .files
        .update_preferences(
            fixture.admin_id,
            UserPreferencesPatch {
                background_file_ids: Some(vec![legacy.file_id.clone()]),
                active_background_file_id: PreferenceValuePatch::Set(legacy.file_id.clone()),
                ..UserPreferencesPatch::default()
            },
        )
        .unwrap();

    let preferences = fixture.files.get_preferences(fixture.admin_id).unwrap();
    assert_eq!(
        preferences.background_file_ids,
        vec![legacy.file_id.clone()]
    );
    assert_eq!(
        preferences.active_background_file_id.as_deref(),
        Some(legacy.file_id.as_str())
    );
}

#[test]
fn knowledge_file_sort_compares_rfc3339_instants_across_offsets() {
    let fixture = Fixture::new();
    let earlier = fixture
        .service
        .upload(
            fixture.admin_id,
            "earlier.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Earlier".to_vec(),
        )
        .unwrap();
    let later = fixture
        .service
        .upload(
            fixture.admin_id,
            "later.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Later".to_vec(),
        )
        .unwrap();
    let connection = fixture._database.connection().unwrap();
    connection
        .execute(
            "UPDATE knowledge_managed_files SET updated_at = ?1 WHERE file_id = ?2",
            params![
                "2026-01-01T17:00:00+08:00",
                earlier.file_id.as_deref().unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE knowledge_managed_files SET updated_at = ?1 WHERE file_id = ?2",
            params!["2026-01-01T10:00:00Z", later.file_id.as_deref().unwrap()],
        )
        .unwrap();

    let page = fixture
        .service
        .list(fixture.admin_id, &Fixture::query())
        .unwrap();
    assert_eq!(page.items[0].filename, "later.md");
    assert_eq!(page.items[1].filename, "earlier.md");
}

#[tokio::test]
async fn pending_file_can_be_deleted_before_worker_claims_it() {
    let fixture = Fixture::new();
    let uploaded = fixture
        .service
        .upload(
            fixture.admin_id,
            "pending-delete.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Pending delete\n\npending-delete-marker".to_vec(),
        )
        .unwrap();
    let file_id = uploaded.file_id.as_deref().unwrap();

    fixture.service.delete(fixture.admin_id, file_id).unwrap();

    assert!(fixture.service.store.claim_next().unwrap().is_none());
    assert!(
        fixture
            .service
            .store
            .find_owned(fixture.admin_id, file_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        derived_index_counts(&fixture, file_id),
        DerivedIndexCounts::default()
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
async fn processing_file_delete_returns_conflict_and_ready_delete_cleans_all_derived_rows() {
    let fixture = Fixture::new();
    let uploaded = fixture
        .service
        .upload(
            fixture.admin_id,
            "processing-delete.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Processing delete\n\nprocessing-delete-marker".to_vec(),
        )
        .unwrap();
    let file_id = uploaded.file_id.as_deref().unwrap();
    let claimed = fixture.service.store.claim_next().unwrap().unwrap();

    let error = fixture
        .service
        .delete(fixture.admin_id, file_id)
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert_eq!(
        fixture
            .service
            .store
            .find_owned(fixture.admin_id, file_id)
            .unwrap()
            .unwrap()
            .status,
        KnowledgeFileStatus::Processing
    );

    assert_eq!(
        fixture.service.process_claimed(claimed).unwrap(),
        KnowledgeWorkerOutcome::Ready
    );
    fixture.service.delete(fixture.admin_id, file_id).unwrap();
    assert_eq!(
        derived_index_counts(&fixture, file_id),
        DerivedIndexCounts::default()
    );
}

#[test]
fn mark_ready_not_applied_cleans_the_index_written_by_the_claim() {
    let fixture = Fixture::new();
    let uploaded = fixture
        .service
        .upload(
            fixture.admin_id,
            "lost-claim.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Lost claim\n\nlost-claim-marker".to_vec(),
        )
        .unwrap();
    let file_id = uploaded.file_id.as_deref().unwrap();
    let claimed = fixture.service.store.claim_next().unwrap().unwrap();
    let source = fixture
        .files
        .read_file_for_module(fixture.admin_id, file_id, UserFileModule::Knowledge)
        .unwrap();
    fixture
        .index
        .process_managed_file(
            file_id,
            &claimed.filename,
            &source.bytes,
            fixture.service.max_file_bytes(),
            Some(&claimed.created_at),
        )
        .unwrap();
    assert!(derived_index_counts(&fixture, file_id).documents > 0);
    fixture
        .service
        .store
        .database()
        .connection()
        .unwrap()
        .execute(
            "UPDATE knowledge_managed_files SET status = 'pending' WHERE file_id = ?1",
            [file_id],
        )
        .unwrap();

    assert_eq!(
        fixture.service.process_claimed(claimed).unwrap(),
        KnowledgeWorkerOutcome::Cancelled
    );
    assert_eq!(
        derived_index_counts(&fixture, file_id),
        DerivedIndexCounts::default()
    );
}

#[tokio::test]
async fn worker_cycle_recovers_processing_without_restarting_the_worker() {
    let fixture = Fixture::new();
    let uploaded = fixture
        .service
        .upload(
            fixture.admin_id,
            "cycle-recovery.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Cycle recovery\n\ncycle-recovery-marker".to_vec(),
        )
        .unwrap();
    let file_id = uploaded.file_id.as_deref().unwrap();
    let worker = KnowledgeFileWorker::new(fixture.service.clone()).unwrap();
    let claimed = fixture.service.store.claim_next().unwrap().unwrap();
    let source = fixture
        .files
        .read_file_for_module(fixture.admin_id, file_id, UserFileModule::Knowledge)
        .unwrap();
    fixture
        .index
        .process_managed_file(
            file_id,
            &claimed.filename,
            &source.bytes,
            fixture.service.max_file_bytes(),
            Some(&claimed.created_at),
        )
        .unwrap();
    assert!(derived_index_counts(&fixture, file_id).documents > 0);

    let stats = worker.run_once().await.unwrap();
    assert_eq!(stats.claimed, 1);
    assert_eq!(stats.ready, 1);
    assert_eq!(
        fixture
            .service
            .store
            .find_owned(fixture.admin_id, file_id)
            .unwrap()
            .unwrap()
            .status,
        KnowledgeFileStatus::Ready
    );
    assert!(derived_index_counts(&fixture, file_id).documents > 0);
}

#[tokio::test]
async fn worker_cycles_do_not_claim_the_same_file_concurrently() {
    let fixture = Fixture::new();
    fixture
        .service
        .upload(
            fixture.admin_id,
            "single-claim.md".to_owned(),
            "text/markdown".to_owned(),
            b"# Single claim\n\nsingle-claim-marker".to_vec(),
        )
        .unwrap();
    let worker = KnowledgeFileWorker::new(fixture.service.clone()).unwrap();
    let (first, second) = tokio::join!(worker.run_once(), worker.run_once());
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.claimed + second.claimed, 1);
    assert_eq!(first.ready + second.ready, 1);
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
        .read_file_for_module(
            fixture.admin_id,
            missing.file_id.as_deref().unwrap(),
            UserFileModule::Knowledge,
        )
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
    let missing_file_id = missing.file_id.as_deref().unwrap();
    fixture
        .service
        .delete(fixture.admin_id, missing_file_id)
        .unwrap();
    assert!(
        fixture
            .service
            .store
            .find_owned(fixture.admin_id, missing_file_id)
            .unwrap()
            .is_none()
    );
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
        page.items
            .iter()
            .any(|entry| { entry.file_id.as_deref() == Some(first.file_id.as_deref().unwrap()) })
    );
    assert!(
        !page
            .items
            .iter()
            .any(|entry| { entry.file_id.as_deref() == Some(second.file_id.as_deref().unwrap()) })
    );
}

#[test]
fn knowledge_file_cannot_be_added_to_background_preferences_and_delete_removes_source() {
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
    let error = fixture
        .files
        .update_preferences(
            fixture.admin_id,
            UserPreferencesPatch {
                background_file_ids: Some(vec![file_id.clone()]),
                active_background_file_id: PreferenceValuePatch::Set(file_id.clone()),
                ..UserPreferencesPatch::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "bad_request");
    assert!(
        fixture
            .files
            .read_file_for_module(fixture.admin_id, &file_id, UserFileModule::Knowledge)
            .is_ok()
    );

    fixture.service.delete(fixture.admin_id, &file_id).unwrap();
    assert_eq!(
        fixture
            .files
            .read_file_for_module(fixture.admin_id, &file_id, UserFileModule::Knowledge)
            .unwrap_err()
            .code(),
        "not_found"
    );
    let preferences = fixture.files.get_preferences(fixture.admin_id).unwrap();
    assert!(preferences.background_file_ids.is_empty());
    assert!(preferences.active_background_file_id.is_none());
}
