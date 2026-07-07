//! 统一通知 Outbox 存储。
//!
//! 业务层只把“何时、推给谁、推什么快照”写成通知任务；本模块只维护任务状态、
//! 去重、领取、重试和取消，不反向理解 RSS、Todo 等业务表。

use chrono::Duration as ChronoDuration;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

use crate::{
    runtime::push::{PushTarget, PushTargetType, QQ_OFFICIAL_PLATFORM},
    storage::{
        database::{DatabaseError, SqliteDatabase, SqliteMigration},
        session::now_iso_cn,
    },
    util::time_context::shanghai_offset,
};

pub const NOTIFICATION_OUTBOX_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "notification_outbox_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS notification_outbox (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_type TEXT NOT NULL,
            source_id TEXT NOT NULL,
            dedupe_key TEXT NOT NULL UNIQUE,
            target_type TEXT NOT NULL,
            target_id TEXT NOT NULL,
            channel TEXT NOT NULL,
            kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            scheduled_at TEXT NOT NULL,
            status TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 5,
            next_attempt_at TEXT,
            locked_by TEXT,
            locked_at TEXT,
            sent_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            cancelled_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_notification_outbox_due
            ON notification_outbox(status, scheduled_at, next_attempt_at, id);
        CREATE INDEX IF NOT EXISTS idx_notification_outbox_source
            ON notification_outbox(source_type, source_id, status);
        CREATE INDEX IF NOT EXISTS idx_notification_outbox_locked
            ON notification_outbox(status, locked_at);",
};

pub const NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "notification_outbox_target_schema_v2",
    sql: "ALTER TABLE notification_outbox ADD COLUMN platform TEXT NOT NULL DEFAULT 'qq_official';
          ALTER TABLE notification_outbox ADD COLUMN account_id TEXT;
          CREATE INDEX IF NOT EXISTS idx_notification_outbox_target
              ON notification_outbox(platform, account_id, target_type, target_id);",
};

pub const NOTIFICATION_MIGRATIONS: &[SqliteMigration] = &[
    NOTIFICATION_OUTBOX_SCHEMA_V1,
    NOTIFICATION_OUTBOX_TARGET_SCHEMA_V2,
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    Pending,
    Sending,
    Retry,
    Sent,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationTask {
    pub id: i64,
    pub source_type: String,
    pub source_id: String,
    pub dedupe_key: String,
    pub target: PushTarget,
    pub channel: String,
    pub kind: String,
    pub payload: Value,
    pub scheduled_at: String,
    pub status: NotificationStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_attempt_at: Option<String>,
    pub locked_by: Option<String>,
    pub locked_at: Option<String>,
    pub sent_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub cancelled_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationUpsert {
    pub source_type: String,
    pub source_id: String,
    pub dedupe_key: String,
    pub target: PushTarget,
    pub channel: String,
    pub kind: String,
    pub payload: Value,
    pub scheduled_at: String,
    pub max_attempts: u32,
    /// 业务确认同一个 source 重新生效时，允许把已取消任务重新排入 pending。
    pub reactivate_cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct NotificationOutboxStore {
    database: SqliteDatabase,
}

#[derive(Debug, Clone)]
pub struct NotificationError {
    code: &'static str,
    message: String,
}

impl NotificationOutboxStore {
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 创建或更新同一个去重键下尚未终结的任务。
    ///
    /// 已发送任务不会被重开，避免业务层重复提交导致同一事件再次推送；若业务确实需要
    /// 新事件，应生成新的 dedupe_key。
    pub fn upsert(
        &self,
        request: NotificationUpsert,
    ) -> Result<NotificationTask, NotificationError> {
        validate_upsert(&request)?;
        let payload_json = serde_json::to_string(&request.payload)
            .map_err(|err| NotificationError::bad_request(format!("invalid payload: {err}")))?;
        let now = now_iso_cn();
        {
            let conn = self.connection()?;
            conn.execute(
                "INSERT INTO notification_outbox (
                    source_type, source_id, dedupe_key, platform, account_id, target_type, target_id,
                    channel, kind, payload_json, scheduled_at, status, attempts, max_attempts,
                    next_attempt_at, locked_by, locked_at, sent_at, last_error,
                    created_at, updated_at, cancelled_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, NULL, NULL, NULL, NULL, NULL, ?14, ?14, NULL)
                 ON CONFLICT(dedupe_key) DO UPDATE SET
                    source_type = excluded.source_type,
                    source_id = excluded.source_id,
                    platform = excluded.platform,
                    account_id = excluded.account_id,
                    target_type = excluded.target_type,
                    target_id = excluded.target_id,
                    channel = excluded.channel,
                    kind = excluded.kind,
                    payload_json = excluded.payload_json,
                    scheduled_at = excluded.scheduled_at,
                    status = CASE
                        WHEN notification_outbox.status = 'sent' THEN notification_outbox.status
                        WHEN notification_outbox.status = 'cancelled' AND ?15 = 0 THEN notification_outbox.status
                        ELSE 'pending'
                    END,
                    attempts = CASE
                        WHEN notification_outbox.status = 'sent' THEN notification_outbox.attempts
                        WHEN notification_outbox.status = 'cancelled' AND ?15 = 0 THEN notification_outbox.attempts
                        ELSE 0
                    END,
                    max_attempts = excluded.max_attempts,
                    next_attempt_at = NULL,
                    locked_by = NULL,
                    locked_at = NULL,
                    last_error = NULL,
                    updated_at = excluded.updated_at,
                    cancelled_at = CASE
                        WHEN notification_outbox.status = 'cancelled' AND ?15 <> 0 THEN NULL
                        ELSE notification_outbox.cancelled_at
                    END
                 WHERE notification_outbox.status <> 'sent'",
                params![
                    request.source_type,
                    request.source_id,
                    request.dedupe_key,
                    request.target.platform,
                    request.target.account_id,
                    request.target.target_type.as_str(),
                    request.target.target_id,
                    request.channel,
                    request.kind,
                    payload_json,
                    request.scheduled_at,
                    NotificationStatus::Pending.as_str(),
                    i64::from(request.max_attempts),
                    now,
                    if request.reactivate_cancelled { 1 } else { 0 },
                ],
            )
            .map_err(NotificationError::from_sql)?;
        }
        self.get_by_dedupe_key(&request.dedupe_key)?
            .ok_or_else(|| NotificationError::io("notification disappeared after upsert"))
    }

    pub fn cancel_by_source(
        &self,
        source_type: &str,
        source_id: &str,
    ) -> Result<usize, NotificationError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        conn.execute(
            "UPDATE notification_outbox
             SET status = ?3,
                 updated_at = ?4,
                 cancelled_at = ?4,
                 locked_by = NULL,
                 locked_at = NULL
             WHERE source_type = ?1
               AND source_id = ?2
               AND status IN ('pending', 'retry', 'sending', 'failed')",
            params![
                source_type,
                source_id,
                NotificationStatus::Cancelled.as_str(),
                now,
            ],
        )
        .map_err(NotificationError::from_sql)
    }

    pub fn claim_due(
        &self,
        worker_id: &str,
        limit: usize,
        stale_sending_before: &str,
    ) -> Result<Vec<NotificationTask>, NotificationError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(NotificationError::from_sql)?;
        let ids = {
            let mut stmt = tx
                .prepare(
                    "SELECT id
                     FROM notification_outbox
                     WHERE (
                         status = 'pending' AND scheduled_at <= ?1
                     ) OR (
                         status = 'retry' AND COALESCE(next_attempt_at, scheduled_at) <= ?1
                     ) OR (
                         status = 'sending' AND locked_at IS NOT NULL AND locked_at <= ?2
                     )
                     ORDER BY COALESCE(next_attempt_at, scheduled_at), id
                     LIMIT ?3",
                )
                .map_err(NotificationError::from_sql)?;
            stmt.query_map(params![now, stale_sending_before, limit as i64], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(NotificationError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(NotificationError::from_sql)?
        };
        for id in &ids {
            tx.execute(
                "UPDATE notification_outbox
                 SET status = 'sending',
                     attempts = attempts + 1,
                     locked_by = ?2,
                     locked_at = ?3,
                     updated_at = ?3
                 WHERE id = ?1
                   AND status IN ('pending', 'retry', 'sending')",
                params![id, worker_id, now],
            )
            .map_err(NotificationError::from_sql)?;
        }
        let tasks = ids
            .into_iter()
            .map(|id| get_by_id_unlocked(&tx, id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        tx.commit().map_err(NotificationError::from_sql)?;
        Ok(tasks)
    }

    pub fn mark_sent(&self, id: i64) -> Result<(), NotificationError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        conn.execute(
            "UPDATE notification_outbox
             SET status = 'sent',
                 sent_at = ?2,
                 updated_at = ?2,
                 locked_by = NULL,
                 locked_at = NULL,
                 next_attempt_at = NULL,
                 last_error = NULL
             WHERE id = ?1",
            params![id, now],
        )
        .map_err(NotificationError::from_sql)?;
        Ok(())
    }

    pub fn mark_failed(
        &self,
        id: i64,
        error_summary: &str,
        retry_delay_seconds: i64,
    ) -> Result<(), NotificationError> {
        let conn = self.connection()?;
        let now = now_iso_cn();
        let (attempts, max_attempts): (u32, u32) = conn
            .query_row(
                "SELECT attempts, max_attempts FROM notification_outbox WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
            )
            .map_err(NotificationError::from_sql)?;
        let (status, next_attempt_at) = if attempts >= max_attempts {
            (NotificationStatus::Failed, None)
        } else {
            (
                NotificationStatus::Retry,
                Some(add_seconds_to_now(retry_delay_seconds)),
            )
        };
        conn.execute(
            "UPDATE notification_outbox
             SET status = ?2,
                 next_attempt_at = ?3,
                 locked_by = NULL,
                 locked_at = NULL,
                 updated_at = ?4,
                 last_error = ?5
             WHERE id = ?1",
            params![
                id,
                status.as_str(),
                next_attempt_at,
                now,
                truncate_error(error_summary),
            ],
        )
        .map_err(NotificationError::from_sql)?;
        Ok(())
    }

    pub fn get_by_dedupe_key(
        &self,
        dedupe_key: &str,
    ) -> Result<Option<NotificationTask>, NotificationError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT * FROM notification_outbox WHERE dedupe_key = ?1",
            [dedupe_key],
            notification_from_row,
        )
        .optional()
        .map_err(NotificationError::from_sql)
    }

    #[cfg(test)]
    pub fn list_all_for_test(&self) -> Result<Vec<NotificationTask>, NotificationError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT * FROM notification_outbox ORDER BY id")
            .map_err(NotificationError::from_sql)?;
        stmt.query_map([], notification_from_row)
            .map_err(NotificationError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(NotificationError::from_sql)
    }

    fn connection(
        &self,
    ) -> Result<crate::storage::database::PooledSqliteConnection, NotificationError> {
        self.database
            .connection()
            .map_err(NotificationError::from_database)
    }
}

fn get_by_id_unlocked(
    conn: &Connection,
    id: i64,
) -> Result<Option<NotificationTask>, NotificationError> {
    conn.query_row(
        "SELECT * FROM notification_outbox WHERE id = ?1",
        [id],
        notification_from_row,
    )
    .optional()
    .map_err(NotificationError::from_sql)
}

fn notification_from_row(row: &Row<'_>) -> rusqlite::Result<NotificationTask> {
    let target_type = PushTargetType::from_str(row.get::<_, String>("target_type")?.as_str())
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
        })?;
    let status =
        NotificationStatus::from_str(row.get::<_, String>("status")?.as_str()).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
        })?;
    let payload_json: String = row.get("payload_json")?;
    let payload = serde_json::from_str(&payload_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
    })?;
    Ok(NotificationTask {
        id: row.get("id")?,
        source_type: row.get("source_type")?,
        source_id: row.get("source_id")?,
        dedupe_key: row.get("dedupe_key")?,
        target: PushTarget::new(
            row.get::<_, Option<String>>("platform")?
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| QQ_OFFICIAL_PLATFORM.to_owned()),
            row.get("account_id")?,
            target_type,
            row.get::<_, String>("target_id")?,
        ),
        channel: row.get("channel")?,
        kind: row.get("kind")?,
        payload,
        scheduled_at: row.get("scheduled_at")?,
        status,
        attempts: row.get("attempts")?,
        max_attempts: row.get("max_attempts")?,
        next_attempt_at: row.get("next_attempt_at")?,
        locked_by: row.get("locked_by")?,
        locked_at: row.get("locked_at")?,
        sent_at: row.get("sent_at")?,
        last_error: row.get("last_error")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        cancelled_at: row.get("cancelled_at")?,
    })
}

fn validate_upsert(request: &NotificationUpsert) -> Result<(), NotificationError> {
    if request.source_type.trim().is_empty() {
        return Err(NotificationError::bad_request("source_type is required"));
    }
    if request.source_id.trim().is_empty() {
        return Err(NotificationError::bad_request("source_id is required"));
    }
    if request.dedupe_key.trim().is_empty() {
        return Err(NotificationError::bad_request("dedupe_key is required"));
    }
    if request.target.target_id.trim().is_empty() {
        return Err(NotificationError::bad_request("target_id is required"));
    }
    if request.target.platform.trim().is_empty() {
        return Err(NotificationError::bad_request("platform is required"));
    }
    if request.channel.trim().is_empty() {
        return Err(NotificationError::bad_request("channel is required"));
    }
    if request.kind.trim().is_empty() {
        return Err(NotificationError::bad_request("kind is required"));
    }
    if request.scheduled_at.trim().is_empty() {
        return Err(NotificationError::bad_request("scheduled_at is required"));
    }
    Ok(())
}

fn add_seconds_to_now(seconds: i64) -> String {
    let now = chrono::Utc::now().with_timezone(&shanghai_offset());
    (now + ChronoDuration::seconds(seconds.max(0))).to_rfc3339()
}

fn truncate_error(value: &str) -> String {
    const MAX_ERROR_CHARS: usize = 500;
    value.chars().take(MAX_ERROR_CHARS).collect()
}

impl NotificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Retry => "retry",
            Self::Sent => "sent",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "retry" => Ok(Self::Retry),
            "sent" => Ok(Self::Sent),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("invalid notification status `{other}`")),
        }
    }
}

impl NotificationError {
    pub fn code(&self) -> &'static str {
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

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        Self::io(format!("sqlite failed: {err}"))
    }
}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for NotificationError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn test_store() -> NotificationOutboxStore {
        let database =
            SqliteDatabase::open_temp("notification-tests", NOTIFICATION_MIGRATIONS).unwrap();
        NotificationOutboxStore::new(database)
    }

    fn upsert_request(dedupe_key: &str, scheduled_at: &str) -> NotificationUpsert {
        NotificationUpsert {
            source_type: "todo".to_owned(),
            source_id: "1".to_owned(),
            dedupe_key: dedupe_key.to_owned(),
            target: PushTarget::qq_official(PushTargetType::Private, "u1"),
            channel: "qq".to_owned(),
            kind: "todo_reminder".to_owned(),
            payload: json!({"message_type":"text","text":"提醒"}),
            scheduled_at: scheduled_at.to_owned(),
            max_attempts: 3,
            reactivate_cancelled: false,
        }
    }

    #[test]
    fn upsert_reuses_dedupe_key() {
        let store = test_store();
        let first = store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2026-07-03T09:00:00+08:00",
            ))
            .unwrap();
        let second = store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2026-07-03T10:00:00+08:00",
            ))
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(second.scheduled_at, "2026-07-03T10:00:00+08:00");
        assert_eq!(store.list_all_for_test().unwrap().len(), 1);
    }

    #[test]
    fn upsert_persists_platform_target_fields() {
        let store = test_store();
        let mut request = upsert_request("todo:1:wechat", "2026-07-03T09:00:00+08:00");
        request.target = PushTarget::new(
            "wechat_service",
            Some("gh_service".to_owned()),
            PushTargetType::Private,
            "openid-1",
        );

        let task = store.upsert(request).unwrap();

        assert_eq!(task.target.platform, "wechat_service");
        assert_eq!(task.target.account_id.as_deref(), Some("gh_service"));
        assert_eq!(task.target.target_type, PushTargetType::Private);
        assert_eq!(task.target.target_id, "openid-1");
    }

    #[test]
    fn migration_v2_defaults_legacy_rows_to_qq_official() {
        let path = std::env::temp_dir().join(format!(
            "notification-legacy-target-{}.db",
            uuid::Uuid::new_v4()
        ));
        let legacy = SqliteDatabase::open(&path, &[NOTIFICATION_OUTBOX_SCHEMA_V1]).unwrap();
        legacy
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO notification_outbox (
                    source_type, source_id, dedupe_key, target_type, target_id,
                    channel, kind, payload_json, scheduled_at, status,
                    created_at, updated_at
                 ) VALUES (
                    'todo', '1', 'todo:1:reminder', 'private', 'u1',
                    'qq', 'todo_reminder', '{\"message_type\":\"text\",\"text\":\"提醒\"}',
                    '2026-07-03T09:00:00+08:00', 'pending',
                    '2026-07-03T08:00:00+08:00', '2026-07-03T08:00:00+08:00'
                 )",
                [],
            )
            .unwrap();
        drop(legacy);

        let store = NotificationOutboxStore::new(
            SqliteDatabase::open(&path, NOTIFICATION_MIGRATIONS).unwrap(),
        );
        let task = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();

        assert_eq!(task.target.platform, QQ_OFFICIAL_PLATFORM);
        assert_eq!(task.target.account_id, None);
        assert_eq!(task.target.target_type, PushTargetType::Private);
        assert_eq!(task.target.target_id, "u1");
    }

    #[test]
    fn upsert_keeps_cancelled_by_default() {
        let store = test_store();
        store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2099-01-01T09:00:00+08:00",
            ))
            .unwrap();
        store.cancel_by_source("todo", "1").unwrap();

        let resubmitted = store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2099-01-01T10:00:00+08:00",
            ))
            .unwrap();

        assert_eq!(resubmitted.status, NotificationStatus::Cancelled);
        assert!(resubmitted.cancelled_at.is_some());
    }

    #[test]
    fn upsert_can_reactivate_cancelled_task() {
        let store = test_store();
        store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2099-01-01T09:00:00+08:00",
            ))
            .unwrap();
        store.cancel_by_source("todo", "1").unwrap();

        let mut request = upsert_request("todo:1:reminder", "2099-01-01T10:00:00+08:00");
        request.reactivate_cancelled = true;
        let reactivated = store.upsert(request).unwrap();

        assert_eq!(reactivated.status, NotificationStatus::Pending);
        assert_eq!(reactivated.attempts, 0);
        assert_eq!(reactivated.scheduled_at, "2099-01-01T10:00:00+08:00");
        assert!(reactivated.cancelled_at.is_none());
    }

    #[test]
    fn claim_marks_due_task_sending_once() {
        let store = test_store();
        store
            .upsert(upsert_request(
                "todo:1:reminder",
                "2020-01-01T09:00:00+08:00",
            ))
            .unwrap();

        let claimed = store
            .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
            .unwrap();
        let second = store
            .claim_due("worker-b", 10, "2020-01-01T00:00:00+08:00")
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert!(second.is_empty());
        assert_eq!(claimed[0].status, NotificationStatus::Sending);
        assert_eq!(claimed[0].attempts, 1);
    }

    #[test]
    fn failed_task_retries_until_limit() {
        let store = test_store();
        let mut request = upsert_request("todo:1:reminder", "2020-01-01T09:00:00+08:00");
        request.max_attempts = 1;
        let task = store.upsert(request).unwrap();
        store
            .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
            .unwrap();

        store.mark_failed(task.id, "temporary", 60).unwrap();
        let failed = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();
        assert_eq!(failed.status, NotificationStatus::Failed);
    }
}
