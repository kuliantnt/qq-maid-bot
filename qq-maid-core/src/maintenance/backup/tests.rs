use super::*;
use crate::{
    config::center::{
        ConfigCenter, ConfigCenterPaths, ManagedConfigApplyMode, ManagedConfigField,
        SECRET_MISSING_REVISION,
    },
    management::{ConsoleUserDataService, PreferenceValuePatch, UserPreferencesPatch},
    runtime::tools::{
        memory::{CreateMemoryRequest, ListMemoryQuery, MemoryStore},
        rss::{RssFeedItem, RssStore, RssTarget, RssTargetType},
        todo::{
            TodoItemDraft, TodoRecurrenceKind, TodoRecurrenceUnit, TodoStore, TodoTimePrecision,
        },
    },
    storage::{
        database::{SqliteDatabase, SqliteMigration},
        migrations::APP_MIGRATIONS,
        session::{SessionMeta, SessionStore},
    },
};

const TEST_MIGRATIONS: &[SqliteMigration] = &[SqliteMigration {
    name: "maintenance_backup_v1",
    sql: "CREATE TABLE backup_items (id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
}];

fn test_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("qq-maid-{label}-{}", uuid::Uuid::new_v4()))
}

#[test]
fn online_backup_excludes_secrets_and_restores_into_clean_instance() {
    let source = test_directory("backup-source");
    fs::create_dir_all(source.join("config/secrets")).unwrap();
    fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
    fs::write(source.join("config/.env"), "OPENAI_API_KEY=private\n").unwrap();
    fs::write(source.join("config/secrets/master.key"), "private-key").unwrap();
    let database = SqliteDatabase::open(source.join("app.db"), TEST_MIGRATIONS).unwrap();
    database
        .connection()
        .unwrap()
        .execute("INSERT INTO backup_items (value) VALUES ('preserved')", [])
        .unwrap();
    drop(database);

    let bundle = source.join("backups/bundle");
    let report = create_backup(
        &BackupOptions {
            database_file: source.join("app.db"),
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        TEST_MIGRATIONS,
    )
    .unwrap();
    assert!(!report.includes_secret_material);
    assert!(!bundle.join("config/.env").exists());
    assert!(!bundle.join("config/secrets/master.key").exists());
    verify_backup(&bundle, TEST_MIGRATIONS).unwrap();

    let target = source.join("restored");
    restore_backup(&bundle, &target, TEST_MIGRATIONS).unwrap();
    let restored = Connection::open(target.join("data/storage/app.db")).unwrap();
    let value: String = restored
        .query_row("SELECT value FROM backup_items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "preserved");
    assert!(target.join("config/runtime.toml").exists());
    let _ = fs::remove_dir_all(source);
}

#[test]
fn console_files_and_background_preferences_survive_backup_restore() {
    let source = test_directory("console-file-backup-source");
    fs::create_dir_all(source.join("config")).unwrap();
    fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
    let database_file = source.join("app.db");
    let database = SqliteDatabase::open(&database_file, APP_MIGRATIONS).unwrap();
    let admin_id = insert_test_admin(&database);
    let service = ConsoleUserDataService::new(database.clone());
    let content = b"restorable-background".to_vec();
    let file = service
        .create_file(
            admin_id,
            "background.webp".to_owned(),
            "image/webp".to_owned(),
            content.clone(),
        )
        .unwrap();
    service
        .update_preferences(
            admin_id,
            UserPreferencesPatch {
                background_file_ids: Some(vec![file.file_id.clone()]),
                active_background_file_id: PreferenceValuePatch::Set(file.file_id.clone()),
                background_mode: Some(crate::management::BackgroundMode::Default),
                ..UserPreferencesPatch::default()
            },
        )
        .unwrap();
    let storage_filename: String = database
        .connection()
        .unwrap()
        .query_row(
            "SELECT storage_filename FROM console_user_files WHERE file_id = ?1",
            [&file.file_id],
            |row| row.get(0),
        )
        .unwrap();

    let file_root = source.join(CONSOLE_FILES_DIRECTORY);
    let orphan_filename = format!("{}.blob", uuid::Uuid::new_v4().hyphenated());
    fs::write(file_root.join(&orphan_filename), "orphan").unwrap();
    fs::write(file_root.join(".upload-stale.tmp"), "temporary").unwrap();
    fs::write(file_root.join(".delete-stale.tmp"), "tombstone").unwrap();

    let bundle = source.join("bundle");
    create_backup(
        &BackupOptions {
            database_file: database_file.clone(),
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        APP_MIGRATIONS,
    )
    .unwrap();
    let manifest = verify_backup(&bundle, APP_MIGRATIONS).unwrap();
    assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
    assert!(
        manifest
            .files
            .contains_key(&format!("console-files/{storage_filename}"))
    );
    assert!(
        !manifest
            .files
            .contains_key(&format!("console-files/{orphan_filename}"))
    );
    assert_eq!(
        fs::read(bundle.join("console-files").join(&storage_filename)).unwrap(),
        content
    );

    let target = source.join("restored");
    restore_backup(&bundle, &target, APP_MIGRATIONS).unwrap();
    let restored_database =
        SqliteDatabase::open(target.join("data/storage/app.db"), APP_MIGRATIONS).unwrap();
    let restored_service = ConsoleUserDataService::new(restored_database);
    assert_eq!(
        restored_service
            .read_file(admin_id, &file.file_id)
            .unwrap()
            .bytes,
        content
    );
    let restored_preferences = restored_service.get_preferences(admin_id).unwrap();
    assert_eq!(
        restored_preferences.background_file_ids.as_slice(),
        std::slice::from_ref(&file.file_id)
    );
    assert_eq!(
        restored_preferences.active_background_file_id.as_deref(),
        Some(file.file_id.as_str())
    );
    assert_eq!(
        restored_preferences.background_mode,
        crate::management::BackgroundMode::Default
    );

    fs::write(
        bundle.join("console-files").join(&orphan_filename),
        "tampered orphan",
    )
    .unwrap();
    let mut tampered_manifest = read_manifest(&bundle).unwrap();
    let mut tampered_files = hash_bundle_files(&bundle).unwrap();
    tampered_files.remove(MANIFEST_FILE);
    tampered_manifest.files = tampered_files;
    fs::write(
        bundle.join(MANIFEST_FILE),
        toml::to_string_pretty(&tampered_manifest).unwrap(),
    )
    .unwrap();
    let error = verify_backup(&bundle, APP_MIGRATIONS).unwrap_err();
    assert_eq!(error.code(), "invalid_backup");
    assert!(
        error
            .message()
            .contains("console file list does not match database snapshot")
    );
    let _ = fs::remove_dir_all(source);
}

#[test]
fn backup_fails_when_snapshot_references_a_missing_console_file() {
    let source = test_directory("missing-console-file-backup");
    fs::create_dir_all(source.join("config")).unwrap();
    let database_file = source.join("app.db");
    let database = SqliteDatabase::open(&database_file, APP_MIGRATIONS).unwrap();
    let admin_id = insert_test_admin(&database);
    let service = ConsoleUserDataService::new(database.clone());
    let file = service
        .create_file(
            admin_id,
            "missing.webp".to_owned(),
            "image/webp".to_owned(),
            b"missing".to_vec(),
        )
        .unwrap();
    let storage_filename: String = database
        .connection()
        .unwrap()
        .query_row(
            "SELECT storage_filename FROM console_user_files WHERE file_id = ?1",
            [&file.file_id],
            |row| row.get(0),
        )
        .unwrap();
    fs::remove_file(source.join(CONSOLE_FILES_DIRECTORY).join(storage_filename)).unwrap();

    let bundle = source.join("bundle");
    let error = create_backup(
        &BackupOptions {
            database_file,
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        APP_MIGRATIONS,
    )
    .unwrap_err();
    assert_eq!(error.code(), "backup_io_error");
    assert!(error.message().contains("console file source"));
    assert!(!bundle.exists());
    let _ = fs::remove_dir_all(source);
}

#[test]
fn legacy_v1_bundle_still_uses_original_verification_and_restore_rules() {
    let source = test_directory("legacy-v1-backup");
    fs::create_dir_all(source.join("config")).unwrap();
    let database = SqliteDatabase::open(source.join("app.db"), TEST_MIGRATIONS).unwrap();
    drop(database);
    let bundle = source.join("bundle");
    create_backup(
        &BackupOptions {
            database_file: source.join("app.db"),
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        TEST_MIGRATIONS,
    )
    .unwrap();
    fs::remove_dir(bundle.join(CONSOLE_FILES_DIRECTORY)).unwrap();
    let mut manifest = read_manifest(&bundle).unwrap();
    manifest.format_version = LEGACY_FORMAT_VERSION;
    fs::write(
        bundle.join(MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    assert_eq!(
        verify_backup(&bundle, TEST_MIGRATIONS)
            .unwrap()
            .format_version,
        LEGACY_FORMAT_VERSION
    );
    let target = source.join("restored");
    restore_backup(&bundle, &target, TEST_MIGRATIONS).unwrap();
    assert!(!target.join("data/storage/console-files").exists());
    let _ = fs::remove_dir_all(source);
}

fn insert_test_admin(database: &SqliteDatabase) -> i64 {
    let connection = database.connection().unwrap();
    connection
        .execute(
            "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                 VALUES ('backup-admin', 'unused-in-test', 0, 0)",
            [],
        )
        .unwrap();
    connection.last_insert_rowid()
}

#[test]
fn encrypted_managed_config_restores_only_with_matching_master_key() {
    let source = test_directory("encrypted-config-backup-source");
    fs::create_dir_all(source.join("config")).unwrap();
    let database_file = source.join("app.db");
    let database = SqliteDatabase::open(&database_file, APP_MIGRATIONS).unwrap();
    let fields = || {
        vec![ManagedConfigField::secret(
            "provider.openai.api_key",
            "OPENAI_API_KEY",
            "core.provider",
            ManagedConfigApplyMode::Restart,
        )]
    };
    let paths = |root: &Path| ConfigCenterPaths {
        managed_config_file: root.join("config/runtime.toml"),
        master_key_file: root.join("config/secrets/master.key"),
    };
    let center = ConfigCenter::open(fields(), paths(&source), database.clone()).unwrap();
    center
        .replace_secret(
            "provider.openai.api_key",
            "restored-secret-value",
            SECRET_MISSING_REVISION,
        )
        .unwrap();
    drop(center);
    drop(database);

    let bundle = source.join("bundle");
    create_backup(
        &BackupOptions {
            database_file,
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: true,
            application_version: "test".to_owned(),
        },
        APP_MIGRATIONS,
    )
    .unwrap();

    let matching_target = source.join("restored-matching-key");
    restore_backup(&bundle, &matching_target, APP_MIGRATIONS).unwrap();
    let restored_database =
        SqliteDatabase::open(matching_target.join("data/storage/app.db"), APP_MIGRATIONS).unwrap();
    let restored_center =
        ConfigCenter::open(fields(), paths(&matching_target), restored_database).unwrap();
    assert_eq!(
        restored_center
            .resolved_environment(&std::collections::HashMap::new())
            .unwrap()["OPENAI_API_KEY"],
        "restored-secret-value"
    );
    drop(restored_center);

    let missing_target = source.join("restored-missing-key");
    restore_backup(&bundle, &missing_target, APP_MIGRATIONS).unwrap();
    let missing_key = missing_target.join("config/secrets/master.key");
    fs::remove_file(&missing_key).unwrap();
    let missing_database =
        SqliteDatabase::open(missing_target.join("data/storage/app.db"), APP_MIGRATIONS).unwrap();
    let missing_error = match ConfigCenter::open(fields(), paths(&missing_target), missing_database)
    {
        Ok(_) => panic!("encrypted config must reject a missing master key"),
        Err(error) => error,
    };
    assert_eq!(missing_error.code(), "secret_storage_error");
    assert!(
        missing_error
            .message()
            .contains("master key file is missing")
    );
    assert!(!missing_key.exists());

    let wrong_key_source = source.join("wrong-key-source");
    let wrong_key_database =
        SqliteDatabase::open(wrong_key_source.join("app.db"), APP_MIGRATIONS).unwrap();
    let wrong_key_center =
        ConfigCenter::open(fields(), paths(&wrong_key_source), wrong_key_database).unwrap();
    drop(wrong_key_center);
    let wrong_target = source.join("restored-wrong-key");
    restore_backup(&bundle, &wrong_target, APP_MIGRATIONS).unwrap();
    fs::copy(
        wrong_key_source.join("config/secrets/master.key"),
        wrong_target.join("config/secrets/master.key"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            wrong_target.join("config/secrets/master.key"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let wrong_database =
        SqliteDatabase::open(wrong_target.join("data/storage/app.db"), APP_MIGRATIONS).unwrap();
    let wrong_error = match ConfigCenter::open(fields(), paths(&wrong_target), wrong_database) {
        Ok(_) => panic!("encrypted config must reject a mismatched master key"),
        Err(error) => error,
    };
    assert_eq!(wrong_error.code(), "secret_storage_error");
    assert!(wrong_error.message().contains("failed authentication"));

    let _ = fs::remove_dir_all(source);
}

#[test]
fn verification_rejects_modified_bundle() {
    let source = test_directory("backup-tamper");
    fs::create_dir_all(source.join("config")).unwrap();
    fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
    let database = SqliteDatabase::open(source.join("app.db"), TEST_MIGRATIONS).unwrap();
    drop(database);
    let bundle = source.join("bundle");
    create_backup(
        &BackupOptions {
            database_file: source.join("app.db"),
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        TEST_MIGRATIONS,
    )
    .unwrap();
    fs::write(bundle.join("config/runtime.toml"), "tampered = true\n").unwrap();

    let error = verify_backup(&bundle, TEST_MIGRATIONS).unwrap_err();
    assert_eq!(error.code(), "invalid_backup");
    let _ = fs::remove_dir_all(source);
}

#[test]
fn backup_then_modify_and_restore_recovers_core_business_data() {
    let source = test_directory("business-backup-source");
    fs::create_dir_all(source.join("config")).unwrap();
    fs::write(source.join("config/runtime.toml"), "version = 1\n").unwrap();
    let database_file = source.join("app.db");
    let owner = TodoStore::owner(Some("backup-user"), "private:backup-user");
    let session_meta = SessionMeta::new(
        "private:backup-user",
        Some("backup-user".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    let rss_target = RssTarget {
        target_type: RssTargetType::Private,
        target_id: "backup-user".to_owned(),
        scope_key: "private:backup-user".to_owned(),
    };

    create_business_snapshot(&database_file, &owner, &session_meta, &rss_target, "backup");
    let bundle = source.join("bundle");
    create_backup(
        &BackupOptions {
            database_file: database_file.clone(),
            config_directory: source.join("config"),
            output_directory: bundle.clone(),
            include_secrets: false,
            application_version: "test".to_owned(),
        },
        APP_MIGRATIONS,
    )
    .unwrap();

    // 备份完成后继续写入，恢复结果必须只包含备份时的一组业务数据。
    create_business_snapshot(
        &database_file,
        &owner,
        &session_meta,
        &rss_target,
        "after-backup",
    );
    assert_business_record_counts(&database_file, &owner, &session_meta, &rss_target, 2);

    let target = source.join("restored");
    restore_backup(&bundle, &target, APP_MIGRATIONS).unwrap();
    assert_business_record_counts(
        &target.join("data/storage/app.db"),
        &owner,
        &session_meta,
        &rss_target,
        1,
    );
    let _ = fs::remove_dir_all(source);
}

fn create_business_snapshot(
    database_file: &Path,
    owner: &crate::runtime::tools::todo::TodoOwner,
    session_meta: &SessionMeta,
    rss_target: &RssTarget,
    suffix: &str,
) {
    let todo_store = TodoStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    todo_store
        .create(
            owner,
            TodoItemDraft {
                title: format!("backup todo {suffix}"),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    let session_store =
        SessionStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    session_store
        .create(session_meta, format!("backup session {suffix}"), true)
        .unwrap();

    let memory_store =
        MemoryStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    memory_store
        .create(CreateMemoryRequest {
            user_id: Some("backup-user".to_owned()),
            group_id: None,
            content: format!("backup memory {suffix}"),
            source_text: format!("backup memory source {suffix}"),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let rss_store = RssStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    rss_store
        .create_subscription(
            rss_target,
            &format!("https://example.test/{suffix}.xml"),
            &format!("backup feed {suffix}"),
            &[RssFeedItem {
                item_key: format!("item-{suffix}"),
                revision_hash: format!("revision-{suffix}"),
                title: format!("backup item {suffix}"),
                link: None,
                published_at: None,
                updated_at: None,
                summary: None,
                source_order: 0,
            }],
            50,
        )
        .unwrap();
}

fn assert_business_record_counts(
    database_file: &Path,
    owner: &crate::runtime::tools::todo::TodoOwner,
    session_meta: &SessionMeta,
    rss_target: &RssTarget,
    expected: usize,
) {
    let todo_store = TodoStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    assert_eq!(todo_store.list_pending(owner).unwrap().len(), expected);

    let session_store =
        SessionStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    assert_eq!(
        session_store
            .list_for_scope(&session_meta.scope_key, None)
            .unwrap()
            .len(),
        expected
    );

    let memory_store =
        MemoryStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    assert_eq!(
        memory_store.list(ListMemoryQuery::default()).unwrap().len(),
        expected
    );

    let rss_store = RssStore::new(SqliteDatabase::open(database_file, APP_MIGRATIONS).unwrap());
    assert_eq!(
        rss_store
            .list_by_scope(&rss_target.scope_key)
            .unwrap()
            .len(),
        expected
    );
}
