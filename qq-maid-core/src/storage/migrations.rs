//! 项目级 SQLite migration 聚合入口。
//!
//! 业务模块仍各自维护表结构定义；应用启动和跨模块测试只依赖这里的统一入口，
//! 避免启动层直接知道某个具体业务模块的 migration 列表。

use crate::{
    runtime::tools::memory::{
        MEMORY_CONSOLIDATION_SCHEMA_V4, MEMORY_DOMAIN_SCHEMA_V3, MEMORY_SCHEMA_V1,
        MEMORY_SCOPE_SCHEMA_V2,
    },
    runtime::tools::rss::{
        RSS_ITEM_STATES_SCHEMA, RSS_LEGACY_SEEN_ITEMS_MIGRATION, RSS_PENDING_REBASELINE_MIGRATION,
        RSS_SUBSCRIPTIONS_SCHEMA,
    },
    runtime::tools::todo::{
        TODO_DAILY_REMINDER_PREF_SCHEMA_V5, TODO_RECURRENCE_RULE_SCHEMA_V4,
        TODO_RECURRENCE_SCHEMA_V3, TODO_REMINDER_SCHEMA_V2, TODO_SCHEMA_V1,
    },
    storage::{
        database::SqliteMigration,
        display_name::MANUAL_DISPLAY_NAMES_SCHEMA_V1,
        knowledge::{KNOWLEDGE_SCHEMA_V1, KNOWLEDGE_SCHEMA_V2},
        notification::{NOTIFICATION_OUTBOX_SCHEMA_V1, NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2},
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
    RSS_SUBSCRIPTIONS_SCHEMA,
    RSS_ITEM_STATES_SCHEMA,
    RSS_LEGACY_SEEN_ITEMS_MIGRATION,
    RSS_PENDING_REBASELINE_MIGRATION,
    TODO_SCHEMA_V1,
    TODO_REMINDER_SCHEMA_V2,
    TODO_RECURRENCE_SCHEMA_V3,
    TODO_RECURRENCE_RULE_SCHEMA_V4,
    TODO_DAILY_REMINDER_PREF_SCHEMA_V5,
    NOTIFICATION_OUTBOX_SCHEMA_V1,
    NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2,
    SESSION_SCHEMA_V1,
    SESSION_SCHEMA_V2,
    SESSION_CLEAN_REMOVED_CHAT_STATE_V3,
    SESSION_MESSAGE_TURN_ACTOR_SCHEMA_V4,
    MEMORY_SCHEMA_V1,
    MEMORY_SCOPE_SCHEMA_V2,
    MEMORY_DOMAIN_SCHEMA_V3,
    MEMORY_CONSOLIDATION_SCHEMA_V4,
    MANUAL_DISPLAY_NAMES_SCHEMA_V1,
    KNOWLEDGE_SCHEMA_V1,
    KNOWLEDGE_SCHEMA_V2,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
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

        let reopened_memory =
            MemoryStore::new(SqliteDatabase::open(&path, APP_MIGRATIONS).unwrap());
        let memories = reopened_memory.list(ListMemoryQuery::default()).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].id, memory.id);
    }
}
