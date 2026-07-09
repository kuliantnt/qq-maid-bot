use super::*;

fn test_store() -> RssStore {
    RssStore::new(SqliteDatabase::open_temp("qq-maid-rss-test", RSS_MIGRATIONS).unwrap())
}

fn test_database_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("qq-maid-app-db-test-{}.db", Uuid::new_v4()))
}

fn legacy_rss_schema() -> SqliteMigration {
    SqliteMigration {
        name: "legacy_rss_schema",
        sql: "CREATE TABLE IF NOT EXISTS rss_subscriptions (
                id TEXT PRIMARY KEY,
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                scope_key TEXT NOT NULL,
                url TEXT NOT NULL,
                title TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                last_checked_at TEXT,
                last_success_at TEXT,
                last_error TEXT,
                consecutive_failures INTEGER NOT NULL DEFAULT 0,
                initialized INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS rss_seen_items (
                subscription_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                item_key TEXT NOT NULL,
                title TEXT NOT NULL,
                link TEXT,
                published_at TEXT,
                summary TEXT,
                source_order INTEGER NOT NULL DEFAULT 0,
                first_seen_at TEXT NOT NULL,
                pushed_at TEXT,
                failed_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                PRIMARY KEY(subscription_id, fingerprint)
            );",
    }
}

fn rss_schema_without_pending_rebaseline() -> &'static [SqliteMigration] {
    &[
        RSS_SUBSCRIPTIONS_SCHEMA,
        RSS_ITEM_STATES_SCHEMA,
        RSS_LEGACY_SEEN_ITEMS_MIGRATION,
    ]
}

fn target(scope: &str) -> RssTarget {
    RssTarget {
        target_type: RssTargetType::Group,
        target_id: "g1".to_owned(),
        scope_key: scope.to_owned(),
    }
}

fn item(item_key: &str) -> RssFeedItem {
    item_with_revision(item_key, &format!("{item_key}-rev-1"), "摘要")
}

fn item_with_revision(item_key: &str, revision_hash: &str, summary: &str) -> RssFeedItem {
    item_with_revision_and_time(
        item_key,
        revision_hash,
        summary,
        "2026-06-17T00:00:00+00:00",
        "2026-06-17T00:00:00+00:00",
    )
}

fn item_with_revision_and_time(
    item_key: &str,
    revision_hash: &str,
    summary: &str,
    published_at: &str,
    updated_at: &str,
) -> RssFeedItem {
    RssFeedItem {
        item_key: item_key.to_owned(),
        revision_hash: revision_hash.to_owned(),
        title: format!("标题 {item_key}"),
        link: Some(format!("https://example.test/{item_key}")),
        published_at: Some(published_at.to_owned()),
        updated_at: Some(updated_at.to_owned()),
        summary: Some(summary.to_owned()),
        source_order: 0,
    }
}

#[test]
fn first_subscription_records_baseline_as_seen() {
    let store = test_store();
    let created = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item("a"), item("b")],
            50,
        )
        .unwrap();

    assert!(created.initialized);
    assert!(store.pending_items(&created.id, 10, 3).unwrap().is_empty());
    assert!(store.seen_item(&created.id, "a").unwrap().is_some());
}

#[test]
fn private_and_group_scope_are_isolated() {
    let store = test_store();
    store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "群订阅",
            &[],
            50,
        )
        .unwrap();
    store
        .create_subscription(
            &RssTarget {
                target_type: RssTargetType::Private,
                target_id: "u1".to_owned(),
                scope_key: "private:u1".to_owned(),
            },
            "https://example.test/feed.xml",
            "私聊订阅",
            &[],
            50,
        )
        .unwrap();

    assert_eq!(store.list_by_scope("group:g1").unwrap().len(), 1);
    assert_eq!(store.list_by_scope("private:u1").unwrap().len(), 1);
}

#[test]
fn recent_items_by_scope_filters_query_and_orders_latest_first() {
    let store = test_store();
    let group_sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/codex.xml",
            "Codex 发布",
            &[],
            50,
        )
        .unwrap();
    let private_sub = store
        .create_subscription(
            &RssTarget {
                target_type: RssTargetType::Private,
                target_id: "u1".to_owned(),
                scope_key: "private:u1".to_owned(),
            },
            "https://example.test/codex.xml",
            "私聊 Codex",
            &[],
            50,
        )
        .unwrap();
    store
        .enqueue_items(
            &group_sub.id,
            &[
                item_with_revision_and_time(
                    "codex-old",
                    "rev-old",
                    "旧摘要",
                    "2026-06-17T00:00:00+00:00",
                    "2026-06-17T00:00:00+00:00",
                ),
                item_with_revision_and_time(
                    "codex-new",
                    "rev-new",
                    "新摘要",
                    "2026-06-18T00:00:00+00:00",
                    "2026-06-18T00:00:00+00:00",
                ),
            ],
            50,
        )
        .unwrap();
    store
        .enqueue_items(
            &private_sub.id,
            &[item_with_revision_and_time(
                "private-only",
                "rev-private",
                "私聊摘要",
                "2026-06-19T00:00:00+00:00",
                "2026-06-19T00:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    let recent = store
        .recent_items_by_scope("group:g1", Some("codex"), 5)
        .unwrap();

    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].item_key, "codex-new");
    assert_eq!(recent[1].item_key, "codex-old");
    assert!(
        recent
            .iter()
            .all(|item| item.subscription_id == group_sub.id)
    );
    assert_eq!(recent[0].subscription_title, "Codex 发布");
    assert_eq!(recent[0].summary.as_deref(), Some("新摘要"));
}

#[test]
fn send_success_and_failure_update_push_state_separately() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[],
            50,
        )
        .unwrap();
    store.enqueue_items(&sub.id, &[item("a")], 50).unwrap();
    assert_eq!(store.pending_items(&sub.id, 10, 3).unwrap().len(), 1);

    store
        .record_item_push_failure(&sub.id, "a", "send failed")
        .unwrap();
    assert_eq!(store.pending_items(&sub.id, 10, 3).unwrap().len(), 1);
    store.mark_item_pushed(&sub.id, "a").unwrap();
    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());
}

#[test]
fn same_item_key_revision_update_requeues_pending_once() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item_with_revision("incident-1", "rev-a", "Investigating")],
            50,
        )
        .unwrap();

    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());

    let updated = item_with_revision_and_time(
        "incident-1",
        "rev-b",
        "Resolved",
        "2026-06-17T00:00:00+00:00",
        "2026-06-17T01:00:00+00:00",
    );
    assert_eq!(
        store
            .enqueue_items(&sub.id, std::slice::from_ref(&updated), 50)
            .unwrap(),
        1
    );
    let pending = store.pending_items(&sub.id, 10, 3).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_key, "incident-1");
    assert_eq!(pending[0].revision_hash, "rev-b");
    assert_eq!(pending[0].summary.as_deref(), Some("Resolved"));

    assert_eq!(store.enqueue_items(&sub.id, &[updated], 50).unwrap(), 0);
    assert_eq!(store.pending_items(&sub.id, 10, 3).unwrap().len(), 1);

    store.mark_item_pushed(&sub.id, "incident-1").unwrap();
    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());
}

#[test]
fn pushed_same_time_historical_revision_is_rebaselined() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item_with_revision_and_time(
                "daily-2026-06-19",
                "rev-a",
                "旧摘要",
                "2026-06-19T00:00:00+00:00",
                "2026-06-19T00:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    let rewritten = item_with_revision_and_time(
        "daily-2026-06-19",
        "rev-b",
        "源站回写后的历史摘要",
        "2026-06-19T00:00:00+00:00",
        "2026-06-19T00:00:00+00:00",
    );
    assert_eq!(store.enqueue_items(&sub.id, &[rewritten], 50).unwrap(), 0);

    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());
    let seen = store
        .seen_item(&sub.id, "daily-2026-06-19")
        .unwrap()
        .unwrap();
    assert_eq!(seen.revision_hash, "rev-b");
    assert_eq!(seen.summary.as_deref(), Some("源站回写后的历史摘要"));
}

#[test]
fn pushed_revision_with_new_entry_time_requeues() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item_with_revision_and_time(
                "incident-1",
                "rev-a",
                "Investigating",
                "2026-06-25T00:00:00+00:00",
                "2026-06-25T00:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    let updated = item_with_revision_and_time(
        "incident-1",
        "rev-b",
        "Resolved",
        "2026-06-25T00:00:00+00:00",
        "2026-06-26T02:00:00+00:00",
    );
    assert_eq!(store.enqueue_items(&sub.id, &[updated], 50).unwrap(), 1);

    let pending = store.pending_items(&sub.id, 10, 3).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_key, "incident-1");
    assert_eq!(pending[0].revision_hash, "rev-b");
}

#[test]
fn delayed_revision_with_backdated_entry_time_requeues() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item_with_revision_and_time(
                "incident-1",
                "rev-a",
                "Investigating",
                "2020-01-01T00:00:00+00:00",
                "2020-01-01T00:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    // 源站或 CDN 可能延迟公开已经带有旧 updated_at 的新 revision；
    // 不能因为条目时间早于当前检查时间就把它当作历史回写吞掉。
    let delayed_update = item_with_revision_and_time(
        "incident-1",
        "rev-b",
        "Resolved",
        "2020-01-01T00:00:00+00:00",
        "2020-01-01T00:01:00+00:00",
    );
    assert_eq!(
        store.enqueue_items(&sub.id, &[delayed_update], 50).unwrap(),
        1
    );

    let pending = store.pending_items(&sub.id, 10, 3).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].revision_hash, "rev-b");
}

#[test]
fn same_item_key_same_time_revision_noise_does_not_requeue() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item_with_revision("incident-1", "rev-a", "组件 A\n组件 B")],
            50,
        )
        .unwrap();

    assert_eq!(
        store
            .enqueue_items(
                &sub.id,
                &[item_with_revision("incident-1", "rev-b", "组件 B\n组件 A")],
                50
            )
            .unwrap(),
        0
    );
    let seen = store.seen_item(&sub.id, "incident-1").unwrap().unwrap();
    assert_eq!(seen.revision_hash, "rev-b");
    assert_eq!(seen.summary.as_deref(), Some("组件 B\n组件 A"));
    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());
}

#[test]
fn existing_pending_same_time_revision_noise_is_rebaselined() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[],
            50,
        )
        .unwrap();
    store
        .enqueue_items(
            &sub.id,
            &[item_with_revision("incident-1", "rev-a", "组件 A")],
            50,
        )
        .unwrap();
    assert_eq!(store.pending_items(&sub.id, 10, 3).unwrap().len(), 1);

    assert_eq!(
        store
            .enqueue_items(
                &sub.id,
                &[item_with_revision("incident-1", "rev-b", "组件 B")],
                50
            )
            .unwrap(),
        0
    );
    assert!(store.pending_items(&sub.id, 10, 3).unwrap().is_empty());
}

#[test]
fn retention_never_trims_pending_items() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[],
            50,
        )
        .unwrap();
    store.enqueue_items(&sub.id, &[item("pending")], 1).unwrap();
    store.mark_item_pushed(&sub.id, "pending").unwrap();
    store
        .enqueue_items(&sub.id, &[item("new-pending"), item("another-pending")], 1)
        .unwrap();

    let pending = store.pending_items(&sub.id, 10, 3).unwrap();
    let keys = pending
        .iter()
        .map(|item| item.item_key.as_str())
        .collect::<Vec<_>>();

    assert!(keys.contains(&"new-pending"));
    assert!(keys.contains(&"another-pending"));
}

#[test]
fn reopened_database_reads_existing_rss_data() {
    let path = test_database_path();
    let first_store = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let created = first_store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item("baseline")],
            50,
        )
        .unwrap();
    drop(first_store);

    let reopened_store = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let subscriptions = reopened_store.list_by_scope("group:g1").unwrap();

    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].id, created.id);
    assert!(
        reopened_store
            .seen_item(&created.id, "baseline")
            .unwrap()
            .is_some()
    );
}

#[test]
fn deleting_subscription_cascades_seen_items() {
    let store = test_store();
    let sub = store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[item("baseline")],
            50,
        )
        .unwrap();

    assert!(store.delete_for_scope("group:g1", &sub.id).unwrap());
    assert!(store.seen_item(&sub.id, "baseline").unwrap().is_none());
}

#[test]
fn legacy_seen_items_are_migrated_and_dropped_without_repush() {
    let path = test_database_path();
    let legacy_database = SqliteDatabase::open(&path, &[legacy_rss_schema()]).unwrap();
    {
        let conn = legacy_database.connection().unwrap();
        conn.execute(
            "INSERT INTO rss_subscriptions (
                id, target_type, target_id, scope_key, url, title, enabled,
                created_at, initialized, consecutive_failures
             ) VALUES ('sub-1', 'group', 'g1', 'group:g1',
                'https://example.test/feed.xml', '旧订阅', 1,
                '2026-06-17T00:00:00+08:00', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rss_seen_items (
                subscription_id, fingerprint, item_key, title, link, published_at,
                summary, source_order, first_seen_at, pushed_at
             ) VALUES ('sub-1', 'old-fingerprint', 'id:legacy-entry', '旧条目',
                'https://example.test/legacy', '2026-06-17T00:00:00+00:00',
                '旧摘要', 0, '2026-06-17T00:00:00+08:00',
                '2026-06-17T00:00:00+08:00')",
            [],
        )
        .unwrap();
    }
    drop(legacy_database);

    let store = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let legacy_table_count: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rss_seen_items'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_table_count, 0);
    assert!(store.pending_items("sub-1", 10, 3).unwrap().is_empty());

    let current = item_with_revision("id:legacy-entry", "current-rev", "当前摘要");
    assert_eq!(store.enqueue_items("sub-1", &[current], 50).unwrap(), 0);
    let seen = store
        .seen_item("sub-1", "id:legacy-entry")
        .unwrap()
        .unwrap();
    assert_eq!(seen.revision_hash, "current-rev");
    assert!(store.pending_items("sub-1", 10, 3).unwrap().is_empty());

    let updated = item_with_revision_and_time(
        "id:legacy-entry",
        "next-rev",
        "后续更新",
        "2026-06-17T00:00:00+00:00",
        "2026-06-17T01:00:00+00:00",
    );
    assert_eq!(store.enqueue_items("sub-1", &[updated], 50).unwrap(), 1);
    assert_eq!(store.pending_items("sub-1", 10, 3).unwrap().len(), 1);
}

#[test]
fn title_sanitize_migration_normalizes_dirty_cached_titles() {
    let path = test_database_path();
    let database = SqliteDatabase::open(&path, rss_schema_without_pending_rebaseline()).unwrap();
    {
        let conn = database.connection().unwrap();
        conn.execute(
            "INSERT INTO rss_subscriptions (
                id, target_type, target_id, scope_key, url, title, enabled,
                created_at, initialized, consecutive_failures
             ) VALUES ('sub-1', 'group', 'g1', 'group:g1',
                'https://example.test/feed.xml', 'v0.14.2\n[cpa_final_answer](x)', 1,
                '2026-07-09T00:00:00+08:00', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rss_item_states (
                subscription_id, item_key, revision_hash, title, link,
                published_at, updated_at, summary, source_order, first_seen_at,
                last_seen_at, pushed_at, failed_count, last_error
             ) VALUES (
                'sub-1', 'item-1', 'rev-1', 'v0.14.2\r\n[cpa_final_answer](x)',
                'https://example.test/item-1', '2026-07-08T00:00:00+00:00',
                '2026-07-08T00:00:00+00:00', 'summary', 0,
                '2026-07-09T00:00:00+08:00', '2026-07-09T00:00:00+08:00',
                '2026-07-09T00:00:00+08:00', 0, NULL
             )",
            [],
        )
        .unwrap();
    }
    drop(database);

    let store = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let subscriptions = store.list_by_scope("group:g1").unwrap();
    let seen = store.seen_item("sub-1", "item-1").unwrap().unwrap();

    assert_eq!(subscriptions[0].title, "v0.14.2 [cpa_final_answer](x)");
    assert_eq!(seen.title, "v0.14.2 [cpa_final_answer](x)");
}

#[test]
fn title_sanitize_migration_is_idempotent_and_keeps_non_title_fields() {
    let path = test_database_path();
    let database = SqliteDatabase::open(&path, rss_schema_without_pending_rebaseline()).unwrap();
    {
        let conn = database.connection().unwrap();
        conn.execute(
            "INSERT INTO rss_subscriptions (
                id, target_type, target_id, scope_key, url, title, enabled,
                created_at, initialized, consecutive_failures
             ) VALUES ('sub-1', 'group', 'g1', 'group:g1',
                'https://example.test/feed.xml', 'v0.14.2\n[cpa_final_answer](x)', 1,
                '2026-07-09T00:00:00+08:00', 1, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rss_item_states (
                subscription_id, item_key, revision_hash, title, link,
                published_at, updated_at, summary, source_order, first_seen_at,
                last_seen_at, pushed_at, failed_count, last_error
             ) VALUES (
                'sub-1', 'item-1', 'rev-1', 'v0.14.2\r\n[cpa_final_answer](x)',
                'https://example.test/item-1', '2026-07-08T00:00:00+00:00',
                '2026-07-08T00:00:00+00:00', 'summary body', 0,
                '2026-07-09T00:00:00+08:00', '2026-07-09T00:00:00+08:00',
                '2026-07-09T00:00:00+08:00', 0, NULL
             )",
            [],
        )
        .unwrap();
    }
    drop(database);

    let store = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let first_sub = store.list_by_scope("group:g1").unwrap().remove(0);
    let first_item = store.seen_item("sub-1", "item-1").unwrap().unwrap();
    drop(store);

    let reopened = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let second_sub = reopened.list_by_scope("group:g1").unwrap().remove(0);
    let second_item = reopened.seen_item("sub-1", "item-1").unwrap().unwrap();

    assert_eq!(first_sub.id, second_sub.id);
    assert_eq!(first_sub.url, second_sub.url);
    assert_eq!(first_sub.enabled, second_sub.enabled);
    assert_eq!(first_sub.title, second_sub.title);
    assert_eq!(first_item.item_key, second_item.item_key);
    assert_eq!(first_item.revision_hash, second_item.revision_hash);
    assert_eq!(first_item.link, second_item.link);
    assert_eq!(first_item.summary, second_item.summary);
    assert_eq!(first_item.title, second_item.title);
    assert_eq!(reopened.list_by_scope("group:g1").unwrap().len(), 1);
}

#[test]
fn pending_rebaseline_migration_clears_existing_pending_once() {
    let path = test_database_path();
    let old_store = RssStore::new(
        SqliteDatabase::open(&path, rss_schema_without_pending_rebaseline()).unwrap(),
    );
    let sub = old_store
        .create_subscription(
            &target("group:g1"),
            "https://example.test/feed.xml",
            "测试 Feed",
            &[],
            50,
        )
        .unwrap();
    old_store
        .enqueue_items(&sub.id, &[item("old-pending")], 50)
        .unwrap();
    assert_eq!(old_store.pending_items(&sub.id, 10, 3).unwrap().len(), 1);
    drop(old_store);

    let migrated = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    assert!(migrated.pending_items(&sub.id, 10, 3).unwrap().is_empty());

    migrated
        .enqueue_items(&sub.id, &[item("new-pending")], 50)
        .unwrap();
    assert_eq!(migrated.pending_items(&sub.id, 10, 3).unwrap().len(), 1);
    drop(migrated);

    let reopened = RssStore::new(SqliteDatabase::open(&path, RSS_MIGRATIONS).unwrap());
    let pending = reopened.pending_items(&sub.id, 10, 3).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_key, "new-pending");
}
