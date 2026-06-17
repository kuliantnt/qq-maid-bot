//! RSS 订阅 SQLite 存储。
//!
//! RSS 轮询会同时维护订阅信息、首次基线、待推送条目和已推送游标。
//! 这些状态需要跨重启保持一致，因此使用项目通用 SQLite 句柄承载。
//! 本模块只保留 RSS 表结构和查询语义，数据库打开、目录创建和通用 PRAGMA
//! 由 `storage::database` 统一负责。

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    storage::database::{DatabaseError, SqliteDatabase, SqliteMigration},
    util::time_context::now_iso_cn,
};

/// RSS schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// SQL 保持 `IF NOT EXISTS`，保证空库首次启动和重复启动都安全；
/// 这里不删除、不重建现有 RSS 表，避免破坏已保存的订阅和去重状态。
pub const RSS_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "rss_schema_v1",
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
        CREATE UNIQUE INDEX IF NOT EXISTS idx_rss_sub_scope_url
            ON rss_subscriptions(scope_key, url);
        CREATE INDEX IF NOT EXISTS idx_rss_sub_enabled
            ON rss_subscriptions(enabled, last_checked_at);
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
            PRIMARY KEY(subscription_id, fingerprint),
            FOREIGN KEY(subscription_id) REFERENCES rss_subscriptions(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_rss_seen_pending
            ON rss_seen_items(subscription_id, pushed_at, failed_count, published_at);",
};

pub const RSS_MIGRATIONS: &[SqliteMigration] = &[RSS_SCHEMA_V1];

/// RSS 推送目标类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RssTargetType {
    Private,
    Group,
}

impl RssTargetType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "group" => Self::Group,
            _ => Self::Private,
        }
    }
}

/// 当前 QQ 会话对应的订阅目标。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssTarget {
    pub target_type: RssTargetType,
    pub target_id: String,
    pub scope_key: String,
}

/// 单条 RSS 订阅。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssSubscription {
    pub id: String,
    pub target_type: RssTargetType,
    pub target_id: String,
    pub scope_key: String,
    pub url: String,
    pub title: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_checked_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub initialized: bool,
}

/// 从 feed 中规范化出的条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssFeedItem {
    pub fingerprint: String,
    pub item_key: String,
    pub title: String,
    pub link: Option<String>,
    pub published_at: Option<String>,
    pub summary: Option<String>,
    pub source_order: i64,
}

/// 已发现但尚未成功推送的条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssPendingItem {
    pub subscription_id: String,
    pub fingerprint: String,
    pub item_key: String,
    pub title: String,
    pub link: Option<String>,
    pub published_at: Option<String>,
    pub summary: Option<String>,
    pub failed_count: u32,
}

#[derive(Debug, Clone)]
pub struct RssStore {
    database: SqliteDatabase,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct RssStoreError {
    code: &'static str,
    message: String,
}

impl RssStore {
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    pub fn create_subscription(
        &self,
        target: &RssTarget,
        url: &str,
        title: &str,
        baseline_items: &[RssFeedItem],
        retain_seen: usize,
    ) -> Result<RssSubscription, RssStoreError> {
        let mut conn = self.connection()?;
        let url = clean_required(url, "rss url")?;
        let title = clean_required(title, "rss title")?;
        if self
            .subscription_by_scope_url_unlocked(&conn, &target.scope_key, &url)?
            .is_some()
        {
            return Err(RssStoreError::bad_request(
                "rss subscription already exists",
            ));
        }

        let id = Uuid::new_v4().to_string();
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(RssStoreError::from_sql)?;
        tx.execute(
            "INSERT INTO rss_subscriptions (
                id, target_type, target_id, scope_key, url, title, enabled,
                created_at, initialized, consecutive_failures
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 1, 0)",
            params![
                id,
                target.target_type.as_str(),
                target.target_id,
                target.scope_key,
                url,
                title,
                now,
            ],
        )
        .map_err(RssStoreError::from_sql)?;
        insert_items_unlocked(&tx, &id, baseline_items, Some(&now))?;
        trim_seen_unlocked(&tx, &id, retain_seen)?;
        tx.commit().map_err(RssStoreError::from_sql)?;
        self.get_unlocked(&conn, &id)?
            .ok_or_else(|| RssStoreError::io("rss subscription disappeared after insert"))
    }

    pub fn list_by_scope(&self, scope_key: &str) -> Result<Vec<RssSubscription>, RssStoreError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, target_type, target_id, scope_key, url, title, enabled,
                        created_at, last_checked_at, last_success_at, last_error,
                        consecutive_failures, initialized
                 FROM rss_subscriptions
                 WHERE scope_key = ?1
                 ORDER BY created_at DESC, id DESC",
            )
            .map_err(RssStoreError::from_sql)?;
        let rows = stmt
            .query_map(params![scope_key], subscription_from_row)
            .map_err(RssStoreError::from_sql)?;
        collect_rows(rows)
    }

    pub fn all_enabled(&self) -> Result<Vec<RssSubscription>, RssStoreError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, target_type, target_id, scope_key, url, title, enabled,
                        created_at, last_checked_at, last_success_at, last_error,
                        consecutive_failures, initialized
                 FROM rss_subscriptions
                 WHERE enabled = 1
                 ORDER BY last_checked_at IS NOT NULL, last_checked_at ASC, created_at ASC",
            )
            .map_err(RssStoreError::from_sql)?;
        let rows = stmt
            .query_map([], subscription_from_row)
            .map_err(RssStoreError::from_sql)?;
        collect_rows(rows)
    }

    pub fn get(&self, id: &str) -> Result<Option<RssSubscription>, RssStoreError> {
        let conn = self.connection()?;
        self.get_unlocked(&conn, id)
    }

    pub fn delete_for_scope(&self, scope_key: &str, id: &str) -> Result<bool, RssStoreError> {
        let conn = self.connection()?;
        let affected = conn
            .execute(
                "DELETE FROM rss_subscriptions WHERE scope_key = ?1 AND id = ?2",
                params![scope_key, id],
            )
            .map_err(RssStoreError::from_sql)?;
        Ok(affected > 0)
    }

    pub fn record_check_success(
        &self,
        subscription_id: &str,
        title: Option<&str>,
    ) -> Result<(), RssStoreError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        let clean_title = title.and_then(clean_optional);
        conn.execute(
            "UPDATE rss_subscriptions
             SET title = COALESCE(?2, title),
                 last_checked_at = ?3,
                 last_success_at = ?3,
                 last_error = NULL,
                 consecutive_failures = 0,
                 initialized = 1
             WHERE id = ?1",
            params![subscription_id, clean_title, now],
        )
        .map_err(RssStoreError::from_sql)?;
        Ok(())
    }

    pub fn record_check_failure(
        &self,
        subscription_id: &str,
        message: &str,
    ) -> Result<(), RssStoreError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        conn.execute(
            "UPDATE rss_subscriptions
             SET last_checked_at = ?2,
                 last_error = ?3,
                 consecutive_failures = consecutive_failures + 1
             WHERE id = ?1",
            params![subscription_id, now, truncate_text(message, 300)],
        )
        .map_err(RssStoreError::from_sql)?;
        Ok(())
    }

    /// 将本轮发现的新条目写成待推送状态；已见条目不会重复入队。
    pub fn enqueue_items(
        &self,
        subscription_id: &str,
        items: &[RssFeedItem],
        retain_seen: usize,
    ) -> Result<usize, RssStoreError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(RssStoreError::from_sql)?;
        let inserted = insert_items_unlocked(&tx, subscription_id, items, None)?;
        trim_seen_unlocked(&tx, subscription_id, retain_seen)?;
        tx.commit().map_err(RssStoreError::from_sql)?;
        Ok(inserted)
    }

    pub fn pending_items(
        &self,
        subscription_id: &str,
        limit: usize,
        max_failures: u32,
    ) -> Result<Vec<RssPendingItem>, RssStoreError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT subscription_id, fingerprint, item_key, title, link,
                        published_at, summary, failed_count
                 FROM rss_seen_items
                 WHERE subscription_id = ?1
                   AND pushed_at IS NULL
                   AND failed_count < ?2
                 ORDER BY
                   published_at IS NULL ASC,
                   published_at ASC,
                   source_order ASC,
                   first_seen_at ASC
                 LIMIT ?3",
            )
            .map_err(RssStoreError::from_sql)?;
        let rows = stmt
            .query_map(
                params![subscription_id, max_failures, limit as i64],
                |row| {
                    Ok(RssPendingItem {
                        subscription_id: row.get(0)?,
                        fingerprint: row.get(1)?,
                        item_key: row.get(2)?,
                        title: row.get(3)?,
                        link: row.get(4)?,
                        published_at: row.get(5)?,
                        summary: row.get(6)?,
                        failed_count: row.get::<_, i64>(7)? as u32,
                    })
                },
            )
            .map_err(RssStoreError::from_sql)?;
        collect_rows(rows)
    }

    pub fn mark_item_pushed(
        &self,
        subscription_id: &str,
        fingerprint: &str,
    ) -> Result<(), RssStoreError> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE rss_seen_items
             SET pushed_at = ?3, last_error = NULL
             WHERE subscription_id = ?1 AND fingerprint = ?2",
            params![subscription_id, fingerprint, now_iso_cn()],
        )
        .map_err(RssStoreError::from_sql)?;
        Ok(())
    }

    pub fn record_item_push_failure(
        &self,
        subscription_id: &str,
        fingerprint: &str,
        message: &str,
    ) -> Result<(), RssStoreError> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE rss_seen_items
             SET failed_count = failed_count + 1,
                 last_error = ?3
             WHERE subscription_id = ?1 AND fingerprint = ?2",
            params![subscription_id, fingerprint, truncate_text(message, 300)],
        )
        .map_err(RssStoreError::from_sql)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn seen_item(
        &self,
        subscription_id: &str,
        fingerprint: &str,
    ) -> Result<Option<RssPendingItem>, RssStoreError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT subscription_id, fingerprint, item_key, title, link,
                    published_at, summary, failed_count
             FROM rss_seen_items
             WHERE subscription_id = ?1 AND fingerprint = ?2",
            params![subscription_id, fingerprint],
            |row| {
                Ok(RssPendingItem {
                    subscription_id: row.get(0)?,
                    fingerprint: row.get(1)?,
                    item_key: row.get(2)?,
                    title: row.get(3)?,
                    link: row.get(4)?,
                    published_at: row.get(5)?,
                    summary: row.get(6)?,
                    failed_count: row.get::<_, i64>(7)? as u32,
                })
            },
        )
        .optional()
        .map_err(RssStoreError::from_sql)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, RssStoreError> {
        self.database
            .connection()
            .map_err(RssStoreError::from_database)
    }

    fn get_unlocked(
        &self,
        conn: &Connection,
        id: &str,
    ) -> Result<Option<RssSubscription>, RssStoreError> {
        conn.query_row(
            "SELECT id, target_type, target_id, scope_key, url, title, enabled,
                    created_at, last_checked_at, last_success_at, last_error,
                    consecutive_failures, initialized
             FROM rss_subscriptions
             WHERE id = ?1",
            params![id],
            subscription_from_row,
        )
        .optional()
        .map_err(RssStoreError::from_sql)
    }

    fn subscription_by_scope_url_unlocked(
        &self,
        conn: &Connection,
        scope_key: &str,
        url: &str,
    ) -> Result<Option<String>, RssStoreError> {
        conn.query_row(
            "SELECT id FROM rss_subscriptions WHERE scope_key = ?1 AND url = ?2",
            params![scope_key, url],
            |row| row.get(0),
        )
        .optional()
        .map_err(RssStoreError::from_sql)
    }
}

impl RssStoreError {
    pub fn code(&self) -> &str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        Self::io(format!("sqlite failed: {err}"))
    }

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }
}

fn insert_items_unlocked(
    conn: &Connection,
    subscription_id: &str,
    items: &[RssFeedItem],
    pushed_at: Option<&str>,
) -> Result<usize, RssStoreError> {
    let now = now_iso_cn();
    let mut inserted = 0;
    for item in items {
        let affected = conn
            .execute(
                "INSERT OR IGNORE INTO rss_seen_items (
                    subscription_id, fingerprint, item_key, title, link, published_at,
                    summary, source_order, first_seen_at, pushed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    subscription_id,
                    item.fingerprint,
                    item.item_key,
                    item.title,
                    item.link,
                    item.published_at,
                    item.summary,
                    item.source_order,
                    now,
                    pushed_at,
                ],
            )
            .map_err(RssStoreError::from_sql)?;
        inserted += affected;
    }
    Ok(inserted)
}

/// 去重记录只保留最近 N 条，防止长期订阅无限增长。
fn trim_seen_unlocked(
    conn: &Connection,
    subscription_id: &str,
    retain_seen: usize,
) -> Result<(), RssStoreError> {
    if retain_seen == 0 {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "SELECT fingerprint
             FROM rss_seen_items
             WHERE subscription_id = ?1
             ORDER BY COALESCE(pushed_at, first_seen_at) DESC, first_seen_at DESC
             LIMIT -1 OFFSET ?2",
        )
        .map_err(RssStoreError::from_sql)?;
    let stale = stmt
        .query_map(params![subscription_id, retain_seen as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(RssStoreError::from_sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(RssStoreError::from_sql)?;
    drop(stmt);
    for fingerprint in stale {
        conn.execute(
            "DELETE FROM rss_seen_items WHERE subscription_id = ?1 AND fingerprint = ?2",
            params![subscription_id, fingerprint],
        )
        .map_err(RssStoreError::from_sql)?;
    }
    Ok(())
}

fn subscription_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RssSubscription> {
    Ok(RssSubscription {
        id: row.get(0)?,
        target_type: RssTargetType::from_db(&row.get::<_, String>(1)?),
        target_id: row.get(2)?,
        scope_key: row.get(3)?,
        url: row.get(4)?,
        title: row.get(5)?,
        enabled: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        last_checked_at: row.get(8)?,
        last_success_at: row.get(9)?,
        last_error: row.get(10)?,
        consecutive_failures: row.get::<_, i64>(11)? as u32,
        initialized: row.get::<_, i64>(12)? != 0,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, RssStoreError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(RssStoreError::from_sql)
}

fn clean_required(value: &str, field: &str) -> Result<String, RssStoreError> {
    clean_optional(value).ok_or_else(|| RssStoreError::bad_request(format!("{field} is required")))
}

fn clean_optional(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn truncate_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> RssStore {
        RssStore::new(SqliteDatabase::open_temp("qq-maid-rss-test", RSS_MIGRATIONS).unwrap())
    }

    fn test_database_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("qq-maid-app-db-test-{}.db", Uuid::new_v4()))
    }

    fn target(scope: &str) -> RssTarget {
        RssTarget {
            target_type: RssTargetType::Group,
            target_id: "g1".to_owned(),
            scope_key: scope.to_owned(),
        }
    }

    fn item(fingerprint: &str) -> RssFeedItem {
        RssFeedItem {
            fingerprint: fingerprint.to_owned(),
            item_key: fingerprint.to_owned(),
            title: format!("标题 {fingerprint}"),
            link: Some(format!("https://example.test/{fingerprint}")),
            published_at: Some("2026-06-17T00:00:00+00:00".to_owned()),
            summary: Some("摘要".to_owned()),
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
}
