//! 项目级 SQLite migration 聚合入口。
//!
//! 业务模块仍各自维护表结构定义；应用启动和跨模块测试只依赖这里的统一入口，
//! 避免启动层直接知道某个具体业务模块的 migration 列表。

use crate::{
    config::center::CONFIG_SECRET_SCHEMA_V1,
    management::{
        CONSOLE_ADMIN_SCHEMA_V1, CONSOLE_AUDIT_SCHEMA_V2, CONSOLE_AUDIT_SCHEMA_V3,
        CONSOLE_USER_DATA_SCHEMA_V1, CONSOLE_USER_DATA_SCHEMA_V2, CONSOLE_USER_DATA_SCHEMA_V3,
    },
    runtime::tools::knowledge::{
        KNOWLEDGE_SCHEMA_V1, KNOWLEDGE_SCHEMA_V2, KNOWLEDGE_SCHEMA_V3, KNOWLEDGE_SCHEMA_V4,
    },
    runtime::tools::memory::{
        MEMORY_CONSOLIDATION_SCHEMA_V4, MEMORY_DOMAIN_SCHEMA_V3, MEMORY_MANAGEMENT_SCHEMA_V5,
        MEMORY_SCHEMA_V1, MEMORY_SCOPE_SCHEMA_V2,
    },
    runtime::tools::ops::OPS_EXECUTION_SCHEMA_V1,
    runtime::tools::rss::{
        RSS_ITEM_STATES_SCHEMA, RSS_LEGACY_SEEN_ITEMS_MIGRATION, RSS_PENDING_REBASELINE_MIGRATION,
        RSS_SUBSCRIPTIONS_SCHEMA,
    },
    runtime::tools::todo::{
        TODO_DAILY_REMINDER_PREF_SCHEMA_V5, TODO_RECURRENCE_RULE_SCHEMA_V4,
        TODO_RECURRENCE_SCHEMA_V3, TODO_REMINDER_SCHEMA_V2, TODO_SCHEMA_V1,
    },
    runtime::tools::voice::VOICE_PREFERENCE_SCHEMA_V1,
    storage::{
        database::SqliteMigration,
        display_name::MANUAL_DISPLAY_NAMES_SCHEMA_V1,
        notification::{
            NOTIFICATION_OUTBOX_PART_PROGRESS_SCHEMA_V3, NOTIFICATION_OUTBOX_SCHEMA_V1,
            NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2,
        },
        session::{
            SESSION_CLEAN_REMOVED_CHAT_STATE_V3, SESSION_MESSAGE_TURN_ACTOR_SCHEMA_V4,
            SESSION_SCHEMA_V1, SESSION_SCHEMA_V2,
        },
    },
};

/// 应用通用 SQLite 数据库需要执行的 migration，顺序即项目级 schema 初始化顺序。
///
/// 这里聚合各业务模块暴露的 migration，不复制业务 SQL，避免通用层反向承载表语义。
pub const APP_MIGRATIONS: &[SqliteMigration] = &[
    CONFIG_SECRET_SCHEMA_V1,
    CONSOLE_ADMIN_SCHEMA_V1,
    CONSOLE_AUDIT_SCHEMA_V2,
    CONSOLE_AUDIT_SCHEMA_V3,
    CONSOLE_USER_DATA_SCHEMA_V1,
    CONSOLE_USER_DATA_SCHEMA_V2,
    RSS_SUBSCRIPTIONS_SCHEMA,
    RSS_ITEM_STATES_SCHEMA,
    RSS_LEGACY_SEEN_ITEMS_MIGRATION,
    RSS_PENDING_REBASELINE_MIGRATION,
    TODO_SCHEMA_V1,
    TODO_REMINDER_SCHEMA_V2,
    TODO_RECURRENCE_SCHEMA_V3,
    TODO_RECURRENCE_RULE_SCHEMA_V4,
    TODO_DAILY_REMINDER_PREF_SCHEMA_V5,
    VOICE_PREFERENCE_SCHEMA_V1,
    NOTIFICATION_OUTBOX_SCHEMA_V1,
    NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2,
    NOTIFICATION_OUTBOX_PART_PROGRESS_SCHEMA_V3,
    OPS_EXECUTION_SCHEMA_V1,
    SESSION_SCHEMA_V1,
    SESSION_SCHEMA_V2,
    SESSION_CLEAN_REMOVED_CHAT_STATE_V3,
    SESSION_MESSAGE_TURN_ACTOR_SCHEMA_V4,
    MEMORY_SCHEMA_V1,
    MEMORY_SCOPE_SCHEMA_V2,
    MEMORY_DOMAIN_SCHEMA_V3,
    MEMORY_CONSOLIDATION_SCHEMA_V4,
    MEMORY_MANAGEMENT_SCHEMA_V5,
    MANUAL_DISPLAY_NAMES_SCHEMA_V1,
    KNOWLEDGE_SCHEMA_V1,
    KNOWLEDGE_SCHEMA_V2,
    KNOWLEDGE_SCHEMA_V3,
    KNOWLEDGE_SCHEMA_V4,
    // V3 依赖 knowledge_managed_files，用途字段必须在知识库托管表创建后补上。
    CONSOLE_USER_DATA_SCHEMA_V3,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        management::{BackgroundMode, ConsoleUserDataService, UserPreferencesPatch},
        runtime::tools::{
            memory::{CreateMemoryRequest, ListMemoryQuery, MemoryStore},
            rss::{RssFeedItem, RssStore, RssTarget, RssTargetType},
            todo::{TodoItemDraft, TodoStore, TodoTimePrecision},
        },
        storage::{
            database::SqliteDatabase,
            session::{SessionMeta, SessionStore},
        },
    };

    #[test]
    fn app_migrations_create_rss_schema_and_replay_safely() {
        let path =
            std::env::temp_dir().join(format!("qq-maid-app-migration-{}.db", uuid::Uuid::new_v4()));
        let database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let store = RssStore::new(database);
        let target = RssTarget {
            target_type: RssTargetType::Group,
            target_id: "g1".to_owned(),
            scope_key: "group:g1".to_owned(),
        };
        let subscription = store
            .create_subscription(
                &target,
                "https://example.test/feed.xml",
                "测试 Feed",
                &[RssFeedItem {
                    item_key: "baseline".to_owned(),
                    revision_hash: "baseline-rev".to_owned(),
                    title: "基线条目".to_owned(),
                    link: Some("https://example.test/baseline".to_owned()),
                    published_at: None,
                    updated_at: None,
                    summary: None,
                    source_order: 0,
                }],
                50,
            )
            .unwrap();
        drop(store);

        // APP_MIGRATIONS 当前依赖幂等 SQL；重开同一个库应保留 RSS 数据并安全重放。
        let reopened = RssStore::new(SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap());
        let subscriptions = reopened.list_by_scope("group:g1").unwrap();

        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].id, subscription.id);
        assert!(
            reopened
                .seen_item(&subscription.id, "baseline")
                .unwrap()
                .is_some()
        );

        let todo_database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let todo_store = TodoStore::new(todo_database);
        let owner = TodoStore::owner(Some("u1"), "group:g1");
        let todo = todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: "检查 SQLite migration".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
        drop(todo_store);

        let reopened_todo = TodoStore::new(SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap());
        assert_eq!(reopened_todo.list_pending(&owner).unwrap()[0].id, todo.id);

        let session_database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let session_store = SessionStore::new(session_database);
        let session_meta = SessionMeta::new(
            "group:g1",
            Some("u1".to_owned()),
            Some("g1".to_owned()),
            None,
            None,
            "qq_official",
        );
        let mut session = session_store
            .create(&session_meta, "SQLite 会话", true)
            .unwrap();
        session.append_message("user", "检查 Session migration");
        session_store.save(&mut session).unwrap();
        drop(session_store);

        let reopened_session =
            SessionStore::new(SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap());
        let active = reopened_session
            .get_or_create_active(&session_meta)
            .unwrap();
        assert_eq!(active.title, "SQLite 会话");
        assert_eq!(active.history[0].content, "检查 Session migration");

        let memory_database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let memory_store = MemoryStore::new(memory_database);
        let memory = memory_store
            .create(CreateMemoryRequest {
                user_id: Some("u1".to_owned()),
                group_id: Some("g1".to_owned()),
                content: "Memory 也写入统一 app.db".to_owned(),
                source_text: "/memory Memory 也写入统一 app.db".to_owned(),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
            })
            .unwrap();
        drop(memory_store);

        let reopened_database = SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap();
        let reopened_memory = MemoryStore::new(reopened_database.clone());
        let memories = reopened_memory.list(ListMemoryQuery::default()).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].id, memory.id);
        assert_eq!(memories[0].revision, 1);

        let connection = reopened_database.connection().unwrap();
        let audit_columns = connection
            .prepare("SELECT name FROM pragma_table_info('console_audit_events')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in [
            "request_id",
            "target_digest",
            "before_version",
            "after_version",
            "safe_error_code",
        ] {
            assert!(audit_columns.iter().any(|value| value == column));
        }
        drop(connection);
    }

    #[test]
    fn console_user_data_v2_upgrades_legacy_v1_preferences_without_data_loss() {
        let directory = std::env::temp_dir().join(format!(
            "qq-maid-console-user-data-v2-{}.db",
            uuid::Uuid::new_v4()
        ));
        // 先以旧 schema（V1，无 background_mode 列）建库并写入历史偏好。
        let legacy = SqliteDatabase::open(
            &directory,
            &[CONSOLE_ADMIN_SCHEMA_V1, CONSOLE_USER_DATA_SCHEMA_V1],
        )
        .unwrap();
        legacy
            .connection()
            .unwrap()
            .execute_batch(
                "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                 VALUES ('admin', 'legacy-hash', 0, 1);
                 INSERT INTO console_user_preferences
                   (admin_id, custom_colors_json, background_file_ids_json,
                    active_background_file_id, kuliantnt, created_at, updated_at)
                 VALUES (1, '[\"#112233\"]', '[]', NULL, 1,
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        drop(legacy);

        // 用完整 APP_MIGRATIONS 重开：V1 已应用被跳过，V2 补 background_mode 列。
        let upgraded = SqliteDatabase::open(&directory, APP_MIGRATIONS).unwrap();
        let service = ConsoleUserDataService::new(upgraded.clone());
        let preferences = service.get_preferences(1).unwrap();
        assert_eq!(preferences.custom_colors, vec!["#112233".to_owned()]);
        assert!(preferences.kuliantnt);
        assert_eq!(preferences.background_mode, BackgroundMode::Default);

        // 新字段可写可读，且不会改变旧数据语义。
        let updated = service
            .update_preferences(
                1,
                UserPreferencesPatch {
                    background_mode: Some(BackgroundMode::Special),
                    ..UserPreferencesPatch::default()
                },
            )
            .unwrap();
        assert_eq!(updated.background_mode, BackgroundMode::Special);
        assert!(updated.kuliantnt);

        // 再次重开不重复执行 migration，数据保持一致。
        let reopened = SqliteDatabase::open(&directory, APP_MIGRATIONS).unwrap();
        let reread = ConsoleUserDataService::new(reopened)
            .get_preferences(1)
            .unwrap();
        assert_eq!(reread.background_mode, BackgroundMode::Special);
        assert_eq!(reread.custom_colors, vec!["#112233".to_owned()]);

        let _ = std::fs::remove_file(&directory);
    }

    #[test]
    fn console_user_file_module_migration_defaults_legacy_files_and_promotes_managed_files() {
        let directory = std::env::temp_dir().join(format!(
            "qq-maid-console-user-file-module-{}.db",
            uuid::Uuid::new_v4()
        ));
        // 模拟 PR #644 在 module migration 之前已经存在的数据库：通用文件表没有用途列，
        // 但知识托管关联表已经存在。V3 必须只把有托管关联的文件提升为 knowledge。
        let legacy_migrations = APP_MIGRATIONS
            .iter()
            .copied()
            .filter(|migration| migration.name != CONSOLE_USER_DATA_SCHEMA_V3.name)
            .collect::<Vec<_>>();
        let legacy = SqliteDatabase::open(&directory, &legacy_migrations).unwrap();
        legacy
            .connection()
            .unwrap()
            .execute_batch(
                "INSERT INTO console_admins (username, password_hash, disabled, created_at)
                 VALUES ('module-admin', 'legacy-hash', 0, 1);
                 INSERT INTO console_user_files
                   (file_id, admin_id, original_filename, content_type, size,
                    storage_filename, created_at)
                 VALUES
                   ('legacy-background-file', 1, 'background.webp', 'image/webp', 1,
                    'legacy-background.blob', '2026-01-01T00:00:00Z'),
                   ('legacy-knowledge-file', 1, 'knowledge.md', 'text/markdown', 1,
                    'legacy-knowledge.blob', '2026-01-01T00:00:00Z');
                 INSERT INTO knowledge_managed_files
                   (file_id, document_key, status, uploaded_at, updated_at)
                 VALUES
                   ('legacy-knowledge-file', 'managed/legacy-knowledge', 'pending',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        drop(legacy);

        let upgraded = SqliteDatabase::open(&directory, APP_MIGRATIONS).unwrap();
        let connection = upgraded.connection().unwrap();
        let module = |file_id: &str| {
            connection
                .query_row(
                    "SELECT module FROM console_user_files WHERE file_id = ?1",
                    [file_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        };
        assert_eq!(module("legacy-background-file"), "background");
        assert_eq!(module("legacy-knowledge-file"), "knowledge");
        drop(connection);
        drop(upgraded);
        let _ = std::fs::remove_file(&directory);
    }
}
