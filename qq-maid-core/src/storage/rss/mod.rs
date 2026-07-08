//! RSS 订阅 SQLite 存储。
//!
//! RSS 轮询会同时维护订阅信息、首次基线、待推送条目和已推送游标。
//! 这些状态需要跨重启保持一致，因此使用项目通用 SQLite 句柄承载。
//! 本模块只保留 RSS 表结构和查询语义，数据库打开、目录创建和通用 PRAGMA
//! 由 `storage::database` 统一负责。

use qq_maid_common::time_context::now_iso_cn;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::storage::database::{DatabaseError, SqliteDatabase, SqliteMigration};

/// RSS 订阅表 schema，由应用启动时的通用数据库初始化流程统一执行。
///
/// SQL 保持 `IF NOT EXISTS`，保证空库首次启动和重复启动都安全；
/// 这里不删除、不重建订阅表，避免破坏已保存的 RSS 订阅。
pub const RSS_SUBSCRIPTIONS_SCHEMA: SqliteMigration = SqliteMigration {
    name: "rss_subscriptions_schema",
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
        ",
};

/// RSS 条目状态表。
///
/// 用 `(subscription_id, item_key)` 固定同一逻辑条目，再用 `revision_hash`
/// 记录当前内容版本，从而识别 Atom entry 的状态更新。
pub const RSS_ITEM_STATES_SCHEMA: SqliteMigration = SqliteMigration {
    name: "rss_item_states_schema",
    sql:
        "CREATE TABLE IF NOT EXISTS rss_item_states (
            subscription_id TEXT NOT NULL,
            item_key TEXT NOT NULL,
            revision_hash TEXT NOT NULL,
            title TEXT NOT NULL,
            link TEXT,
            published_at TEXT,
            updated_at TEXT,
            summary TEXT,
            source_order INTEGER NOT NULL DEFAULT 0,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            pushed_at TEXT,
            failed_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            PRIMARY KEY(subscription_id, item_key),
            FOREIGN KEY(subscription_id) REFERENCES rss_subscriptions(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_rss_item_states_pending
            ON rss_item_states(subscription_id, pushed_at, failed_count, updated_at, published_at);",
};

const LEGACY_REVISION_PREFIX: &str = "legacy:";

/// 迁移并清理旧版 RSS 条目表。
///
/// `rss_seen_items` 已不再承载运行时语义，最终必须删除；但旧库可能只在这张表里
/// 记录了“已见历史条目”。先写入 legacy revision，再删除旧表，可避免升级后把历史
/// feed 全量当成新更新推送。
pub const RSS_LEGACY_SEEN_ITEMS_MIGRATION: SqliteMigration = SqliteMigration {
    name: "rss_legacy_seen_items_migration",
    sql: "CREATE TABLE IF NOT EXISTS rss_seen_items (
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
        INSERT OR IGNORE INTO rss_item_states (
            subscription_id, item_key, revision_hash, title, link,
            published_at, updated_at, summary, source_order, first_seen_at,
            last_seen_at, pushed_at, failed_count, last_error
        )
        SELECT
            subscription_id,
            item_key,
            'legacy:' || fingerprint,
            title,
            link,
            published_at,
            published_at,
            summary,
            source_order,
            first_seen_at,
            COALESCE(pushed_at, first_seen_at),
            pushed_at,
            failed_count,
            last_error
        FROM rss_seen_items
        ORDER BY COALESCE(pushed_at, first_seen_at) DESC, first_seen_at DESC;
        DROP TABLE IF EXISTS rss_seen_items;",
};

/// 一次性清理旧 revision 逻辑已经排入的 RSS 待推送队列。
///
/// 2026-06-18 前后的开发版本会把 Statuspage 组件列表顺序抖动误判成大量更新。
/// 这里只在当前库第一次看到该 marker 时重设已存在的 pending，避免部署后继续刷屏；
/// marker 写入后，后续真实推送失败产生的 pending 仍按原重试语义保留。
pub const RSS_PENDING_REBASELINE_MIGRATION: SqliteMigration = SqliteMigration {
    name: "rss_pending_rebaseline_20260618",
    sql: "CREATE TABLE IF NOT EXISTS rss_internal_migrations (
            name TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        );
        UPDATE rss_item_states
        SET pushed_at = COALESCE(last_seen_at, first_seen_at),
            last_error = NULL
        WHERE pushed_at IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM rss_internal_migrations
              WHERE name = 'rss_pending_rebaseline_20260618'
          );
        INSERT OR IGNORE INTO rss_internal_migrations (name, applied_at)
        VALUES ('rss_pending_rebaseline_20260618', datetime('now'));",
};

pub const RSS_MIGRATIONS: &[SqliteMigration] = &[
    RSS_SUBSCRIPTIONS_SCHEMA,
    RSS_ITEM_STATES_SCHEMA,
    RSS_LEGACY_SEEN_ITEMS_MIGRATION,
    RSS_PENDING_REBASELINE_MIGRATION,
];

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
    pub item_key: String,
    pub revision_hash: String,
    pub title: String,
    pub link: Option<String>,
    pub published_at: Option<String>,
    pub updated_at: Option<String>,
    pub summary: Option<String>,
    pub source_order: i64,
}

/// 已发现但尚未成功推送的条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssPendingItem {
    pub subscription_id: String,
    pub item_key: String,
    pub revision_hash: String,
    pub title: String,
    pub link: Option<String>,
    pub published_at: Option<String>,
    pub updated_at: Option<String>,
    pub summary: Option<String>,
    pub failed_count: u32,
}

/// 当前 scope 下可供查询展示的 RSS 最近条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RssRecentItem {
    pub subscription_id: String,
    pub subscription_title: String,
    pub subscription_url: String,
    pub item_key: String,
    pub revision_hash: String,
    pub title: String,
    pub link: Option<String>,
    pub published_at: Option<String>,
    pub updated_at: Option<String>,
    pub summary: Option<String>,
    pub pushed_at: Option<String>,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedItemChange {
    New,
    Updated,
    Unchanged,
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

    /// 将本轮发现的新条目或内容更新写成待推送状态；未变化条目只刷新 last_seen_at。
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
                "SELECT subscription_id, item_key, revision_hash, title, link,
                        published_at, updated_at, summary, failed_count
                 FROM rss_item_states
                 WHERE subscription_id = ?1
                   AND pushed_at IS NULL
                   AND failed_count < ?2
                 ORDER BY
                   COALESCE(updated_at, published_at) IS NULL ASC,
                   COALESCE(updated_at, published_at) ASC,
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
                        item_key: row.get(1)?,
                        revision_hash: row.get(2)?,
                        title: row.get(3)?,
                        link: row.get(4)?,
                        published_at: row.get(5)?,
                        updated_at: row.get(6)?,
                        summary: row.get(7)?,
                        failed_count: row.get::<_, i64>(8)? as u32,
                    })
                },
            )
            .map_err(RssStoreError::from_sql)?;
        collect_rows(rows)
    }

    /// 查询当前 scope 的最近 RSS 条目。
    ///
    /// 该方法只读取本地已入库状态，不触发远端拉取；因此适合 Tool Loop 回答
    /// “上次某订阅发布了什么”一类问题，避免模型调用工具时产生隐式订阅或刷新副作用。
    pub fn recent_items_by_scope(
        &self,
        scope_key: &str,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RssRecentItem>, RssStoreError> {
        let limit = limit.clamp(1, 20);
        let query = query.and_then(clean_optional);
        let pattern = query.as_ref().map(|value| format!("%{value}%"));
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.title, s.url,
                        i.item_key, i.revision_hash, i.title, i.link,
                        i.published_at, i.updated_at, i.summary,
                        i.pushed_at, i.last_seen_at
                 FROM rss_item_states i
                 JOIN rss_subscriptions s ON s.id = i.subscription_id
                 WHERE s.scope_key = ?1
                   AND (
                       ?2 IS NULL
                       OR s.title LIKE ?2
                       OR s.url LIKE ?2
                       OR i.title LIKE ?2
                       OR COALESCE(i.summary, '') LIKE ?2
                       OR COALESCE(i.link, '') LIKE ?2
                   )
                 ORDER BY
                   COALESCE(i.updated_at, i.published_at, i.pushed_at, i.last_seen_at) IS NULL ASC,
                   COALESCE(i.updated_at, i.published_at, i.pushed_at, i.last_seen_at) DESC,
                   i.source_order ASC,
                   i.last_seen_at DESC
                 LIMIT ?3",
            )
            .map_err(RssStoreError::from_sql)?;
        let rows = stmt
            .query_map(
                params![scope_key, pattern, limit as i64],
                recent_item_from_row,
            )
            .map_err(RssStoreError::from_sql)?;
        collect_rows(rows)
    }

    pub fn mark_item_pushed(
        &self,
        subscription_id: &str,
        item_key: &str,
    ) -> Result<(), RssStoreError> {
        // 字段名沿用历史 pushed_at；RSS 迁移到统一通知链路后，这里表示对应通知任务
        // 已成功入队，真实发送成功/失败由 notification_outbox 记录。
        let conn = self.connection()?;
        let affected = conn
            .execute(
                "UPDATE rss_item_states
             SET pushed_at = ?3, last_error = NULL
             WHERE subscription_id = ?1 AND item_key = ?2",
                params![subscription_id, item_key, now_iso_cn()],
            )
            .map_err(RssStoreError::from_sql)?;
        if affected == 0 {
            return Err(RssStoreError::io("rss item state not found"));
        }
        Ok(())
    }

    pub fn record_item_push_failure(
        &self,
        subscription_id: &str,
        item_key: &str,
        message: &str,
    ) -> Result<(), RssStoreError> {
        let conn = self.connection()?;
        let affected = conn
            .execute(
                "UPDATE rss_item_states
             SET failed_count = failed_count + 1,
                 last_error = ?3
             WHERE subscription_id = ?1 AND item_key = ?2",
                params![subscription_id, item_key, truncate_text(message, 300)],
            )
            .map_err(RssStoreError::from_sql)?;
        if affected == 0 {
            return Err(RssStoreError::io("rss item state not found"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn seen_item(
        &self,
        subscription_id: &str,
        item_key: &str,
    ) -> Result<Option<RssPendingItem>, RssStoreError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT subscription_id, item_key, revision_hash, title, link,
                    published_at, updated_at, summary, failed_count
             FROM rss_item_states
             WHERE subscription_id = ?1 AND item_key = ?2",
            params![subscription_id, item_key],
            |row| {
                Ok(RssPendingItem {
                    subscription_id: row.get(0)?,
                    item_key: row.get(1)?,
                    revision_hash: row.get(2)?,
                    title: row.get(3)?,
                    link: row.get(4)?,
                    published_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    summary: row.get(7)?,
                    failed_count: row.get::<_, i64>(8)? as u32,
                })
            },
        )
        .optional()
        .map_err(RssStoreError::from_sql)
    }

    fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, RssStoreError> {
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
    let mut changed = 0;
    for item in items {
        let change = upsert_item_state_unlocked(conn, subscription_id, item, &now, pushed_at)?;
        if matches!(change, FeedItemChange::New | FeedItemChange::Updated) {
            changed += 1;
        }
    }
    Ok(changed)
}

fn upsert_item_state_unlocked(
    conn: &Connection,
    subscription_id: &str,
    item: &RssFeedItem,
    now: &str,
    pushed_at: Option<&str>,
) -> Result<FeedItemChange, RssStoreError> {
    let existing = conn
        .query_row(
            "SELECT revision_hash, published_at, updated_at, pushed_at
             FROM rss_item_states
             WHERE subscription_id = ?1 AND item_key = ?2",
            params![subscription_id, item.item_key],
            |row| {
                Ok(ExistingItemState {
                    revision_hash: row.get(0)?,
                    published_at: row.get(1)?,
                    updated_at: row.get(2)?,
                    pushed_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(RssStoreError::from_sql)?;

    let Some(existing) = existing else {
        conn.execute(
            "INSERT INTO rss_item_states (
                subscription_id, item_key, revision_hash, title, link,
                published_at, updated_at, summary, source_order, first_seen_at,
                last_seen_at, pushed_at, failed_count, last_error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, 0, NULL)",
            params![
                subscription_id,
                item.item_key,
                item.revision_hash,
                item.title,
                item.link,
                item.published_at,
                item.updated_at,
                item.summary,
                item.source_order,
                now,
                pushed_at,
            ],
        )
        .map_err(RssStoreError::from_sql)?;
        return Ok(FeedItemChange::New);
    };

    if existing.revision_hash == item.revision_hash {
        update_item_seen_unlocked(
            conn,
            subscription_id,
            item,
            now,
            existing.pushed_at.as_deref(),
        )?;
        return Ok(FeedItemChange::Unchanged);
    }

    if is_pushed_legacy_state(&existing) {
        // legacy revision 来自旧 `rss_seen_items`，只代表“这个 item_key 已见”。
        // 首次升级后用当前 feed 内容刷新 revision，但保留 pushed_at，避免把历史条目补推一遍。
        update_item_seen_unlocked(
            conn,
            subscription_id,
            item,
            now,
            existing.pushed_at.as_deref(),
        )?;
        return Ok(FeedItemChange::Unchanged);
    }

    if is_same_entry_time(&existing, item) {
        // 部分状态页 feed 的组件列表顺序会抖动，或 revision 归一化算法升级后 hash 会变化。
        // 只按上次成功检查时间判断会吞掉延迟公开的真实更新；这里必须看到条目自身
        // published/updated 时间没有变化，才把 revision 差异当作已观测条目的重新基线。
        let pushed_at = existing.pushed_at.as_deref().or(Some(now));
        update_item_seen_unlocked(conn, subscription_id, item, now, pushed_at)?;
        return Ok(FeedItemChange::Unchanged);
    }

    conn.execute(
        "UPDATE rss_item_states
         SET revision_hash = ?3,
             title = ?4,
             link = ?5,
             published_at = ?6,
             updated_at = ?7,
             summary = ?8,
             source_order = ?9,
             last_seen_at = ?10,
             pushed_at = NULL,
             failed_count = 0,
             last_error = NULL
         WHERE subscription_id = ?1 AND item_key = ?2",
        params![
            subscription_id,
            item.item_key,
            item.revision_hash,
            item.title,
            item.link,
            item.published_at,
            item.updated_at,
            item.summary,
            item.source_order,
            now,
        ],
    )
    .map_err(RssStoreError::from_sql)?;
    Ok(FeedItemChange::Updated)
}

fn update_item_seen_unlocked(
    conn: &Connection,
    subscription_id: &str,
    item: &RssFeedItem,
    now: &str,
    pushed_at: Option<&str>,
) -> Result<(), RssStoreError> {
    conn.execute(
        "UPDATE rss_item_states
         SET revision_hash = ?3,
             title = ?4,
             link = ?5,
             published_at = ?6,
             updated_at = ?7,
             summary = ?8,
             source_order = ?9,
             last_seen_at = ?10,
             pushed_at = COALESCE(?11, pushed_at)
         WHERE subscription_id = ?1 AND item_key = ?2",
        params![
            subscription_id,
            item.item_key,
            item.revision_hash,
            item.title,
            item.link,
            item.published_at,
            item.updated_at,
            item.summary,
            item.source_order,
            now,
            pushed_at,
        ],
    )
    .map_err(RssStoreError::from_sql)?;
    Ok(())
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
    // pending 记录还承担重试状态，不能因为保留数量太小被提前裁掉。
    let mut stmt = conn
        .prepare(
            "SELECT item_key
             FROM rss_item_states
             WHERE subscription_id = ?1
               AND pushed_at IS NOT NULL
             ORDER BY COALESCE(pushed_at, last_seen_at, first_seen_at) DESC, first_seen_at DESC
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
    for item_key in stale {
        conn.execute(
            "DELETE FROM rss_item_states WHERE subscription_id = ?1 AND item_key = ?2",
            params![subscription_id, item_key],
        )
        .map_err(RssStoreError::from_sql)?;
    }
    Ok(())
}

struct ExistingItemState {
    revision_hash: String,
    published_at: Option<String>,
    updated_at: Option<String>,
    pushed_at: Option<String>,
}

fn is_pushed_legacy_state(existing: &ExistingItemState) -> bool {
    existing.revision_hash.starts_with(LEGACY_REVISION_PREFIX) && existing.pushed_at.is_some()
}

fn is_same_entry_time(existing: &ExistingItemState, item: &RssFeedItem) -> bool {
    existing.published_at == item.published_at && existing.updated_at == item.updated_at
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

fn recent_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RssRecentItem> {
    Ok(RssRecentItem {
        subscription_id: row.get(0)?,
        subscription_title: row.get(1)?,
        subscription_url: row.get(2)?,
        item_key: row.get(3)?,
        revision_hash: row.get(4)?,
        title: row.get(5)?,
        link: row.get(6)?,
        published_at: row.get(7)?,
        updated_at: row.get(8)?,
        summary: row.get(9)?,
        pushed_at: row.get(10)?,
        last_seen_at: row.get(11)?,
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
mod tests;
