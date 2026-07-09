//! 统一通知投递 Worker。
//!
//! Worker 只处理 Outbox 任务的领取、投递结果回写和失败重试，不根据 source_type
//! 反查业务表，也不重新组装 Todo / RSS 等业务语义。

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::Duration as ChronoDuration;
use qq_maid_common::time_context::shanghai_offset;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::{
    runtime::{
        push::{PushError, PushIntent, PushSink},
        todo::reminder_task::sync_reminder_task,
    },
    storage::{
        notification::{NotificationOutboxStore, NotificationTask},
        todo::TodoStore,
    },
};

const DEFAULT_WORKER_POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(60);
const DEFAULT_BATCH_LIMIT: usize = 20;

#[derive(Debug, Clone)]
pub struct NotificationWorkerConfig {
    pub enabled: bool,
    pub poll_interval: Duration,
    pub lock_timeout: Duration,
    pub retry_delay: Duration,
    pub batch_limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationWorkerStats {
    pub claimed_count: usize,
    pub sent_count: usize,
    pub failed_count: usize,
    pub invalid_payload_count: usize,
}

#[derive(Clone)]
pub struct NotificationWorker {
    store: NotificationOutboxStore,
    todo_store: Option<TodoStore>,
    push_sink: Arc<dyn PushSink>,
    config: NotificationWorkerConfig,
    worker_id: String,
}

#[derive(Debug, Deserialize)]
struct NotificationPushPayload {
    message_type: String,
    text: String,
    #[serde(default)]
    fallback_text: Option<String>,
}

impl NotificationWorker {
    pub fn new(
        store: NotificationOutboxStore,
        push_sink: Arc<dyn PushSink>,
        config: NotificationWorkerConfig,
    ) -> Self {
        Self {
            store,
            todo_store: None,
            push_sink,
            config,
            worker_id: new_worker_id(),
        }
    }

    pub fn with_todo_store(mut self, todo_store: TodoStore) -> Self {
        self.todo_store = Some(todo_store);
        self
    }

    pub fn spawn(self) {
        if !self.config.enabled {
            info!("notification worker disabled");
            return;
        }
        tokio::spawn(async move {
            info!(
                batch_limit = self.config.batch_limit,
                poll_interval_seconds = self.config.poll_interval.as_secs(),
                "notification worker enabled"
            );
            self.run_loop().await;
        });
    }

    async fn run_loop(self) {
        loop {
            if let Err(err) = self.run_once().await {
                warn!(error = %err, "notification worker cycle failed");
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    pub async fn run_once(&self) -> Result<NotificationWorkerStats, String> {
        let stale_before = stale_before_iso(self.config.lock_timeout);
        let tasks = self
            .store
            .claim_due(&self.worker_id, self.config.batch_limit, &stale_before)
            .map_err(|err| err.message().to_owned())?;
        let mut stats = NotificationWorkerStats {
            claimed_count: tasks.len(),
            ..NotificationWorkerStats::default()
        };
        for task in tasks {
            match self.deliver(task.clone()).await {
                Ok(()) => {
                    self.store
                        .mark_sent(task.id)
                        .map_err(|err| err.message().to_owned())?;
                    self.reschedule_recurring_todo_reminder(&task).await;
                    stats.sent_count += 1;
                }
                Err(DeliveryError::InvalidPayload(message)) => {
                    self.store
                        .mark_failed(
                            task.id,
                            &message,
                            retry_delay_seconds(self.config.retry_delay),
                        )
                        .map_err(|err| err.message().to_owned())?;
                    stats.invalid_payload_count += 1;
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        "notification task payload invalid"
                    );
                }
                Err(DeliveryError::Push(err)) => {
                    let summary = safe_push_error(&err);
                    self.store
                        .mark_failed(
                            task.id,
                            &summary,
                            retry_delay_seconds(self.config.retry_delay),
                        )
                        .map_err(|err| err.message().to_owned())?;
                    stats.failed_count += 1;
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        error = %summary,
                        "notification push failed"
                    );
                }
            }
        }
        if stats.claimed_count > 0 {
            debug!(
                claimed = stats.claimed_count,
                sent = stats.sent_count,
                failed = stats.failed_count,
                invalid_payload = stats.invalid_payload_count,
                "notification worker cycle finished"
            );
        }
        Ok(stats)
    }

    async fn deliver(&self, task: NotificationTask) -> Result<(), DeliveryError> {
        let payload: NotificationPushPayload = serde_json::from_value(task.payload.clone())
            .map_err(|err| DeliveryError::InvalidPayload(format!("invalid push payload: {err}")))?;
        if payload.message_type.trim().is_empty() || payload.text.trim().is_empty() {
            return Err(DeliveryError::InvalidPayload(
                "push payload requires message_type and text".to_owned(),
            ));
        }
        self.push_sink
            .push(PushIntent {
                target: task.target,
                message_type: payload.message_type,
                text: payload.text,
                fallback_text: payload.fallback_text,
            })
            .await
            .map(|_| ())
            .map_err(DeliveryError::Push)
    }

    async fn reschedule_recurring_todo_reminder(&self, task: &NotificationTask) {
        if task.source_type != "todo" || task.kind != "todo_reminder" {
            return;
        }
        let Some(todo_store) = &self.todo_store else {
            return;
        };
        match todo_store.advance_recurring_by_id(&task.source_id) {
            Ok(Some((owner, item))) => {
                if let Err(err) = sync_reminder_task(&self.store, &owner, &item) {
                    warn!(
                        task_id = task.id,
                        todo_id = %task.source_id,
                        error = %err,
                        "recurring todo reminder reschedule failed"
                    );
                } else {
                    debug!(
                        task_id = task.id,
                        todo_id = %task.source_id,
                        "recurring todo reminder rescheduled"
                    );
                }
            }
            Ok(None) => {}
            Err(err) => {
                warn!(
                    task_id = task.id,
                    todo_id = %task.source_id,
                    error = %err.message(),
                    "recurring todo reminder advance failed"
                );
            }
        }
    }
}

impl Default for NotificationWorkerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: DEFAULT_WORKER_POLL_INTERVAL,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            retry_delay: DEFAULT_RETRY_DELAY,
            batch_limit: DEFAULT_BATCH_LIMIT,
        }
    }
}

enum DeliveryError {
    InvalidPayload(String),
    Push(PushError),
}

fn stale_before_iso(lock_timeout: Duration) -> String {
    let now = chrono::Utc::now().with_timezone(&shanghai_offset());
    let timeout = ChronoDuration::from_std(lock_timeout).unwrap_or_else(|_| ChronoDuration::zero());
    (now - timeout).to_rfc3339()
}

fn retry_delay_seconds(value: Duration) -> i64 {
    i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
}

fn new_worker_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    format!("notification-worker-{nanos}")
}

fn safe_push_error(err: &PushError) -> String {
    match err {
        PushError::Failed { summary } => summary.chars().take(200).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use crate::{
        runtime::{
            push::{PushResult, PushTarget, PushTargetType},
            todo::{TodoItemDraft, TodoRecurrenceKind, TodoRecurrenceUnit, TodoTimePrecision},
        },
        storage::{
            APP_MIGRATIONS,
            database::SqliteDatabase,
            notification::{NOTIFICATION_MIGRATIONS, NotificationStatus, NotificationUpsert},
            todo::TodoStore,
        },
    };

    use super::*;

    #[derive(Default)]
    struct TestPushSink {
        requests: Mutex<Vec<PushIntent>>,
        fail: bool,
    }

    #[async_trait]
    impl PushSink for TestPushSink {
        async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
            if self.fail {
                return Err(PushError::Failed {
                    summary: "temporary".to_owned(),
                });
            }
            self.requests.lock().unwrap().push(intent);
            Ok(PushResult { message_id: None })
        }
    }

    fn test_store() -> NotificationOutboxStore {
        let database =
            SqliteDatabase::open_temp("notification-worker-tests", NOTIFICATION_MIGRATIONS)
                .unwrap();
        NotificationOutboxStore::new(database)
    }

    fn test_app_stores() -> (TodoStore, NotificationOutboxStore) {
        let database =
            SqliteDatabase::open_temp("notification-worker-app-tests", APP_MIGRATIONS).unwrap();
        (
            TodoStore::new(database.clone()),
            NotificationOutboxStore::new(database),
        )
    }

    fn upsert_due(store: &NotificationOutboxStore) {
        store
            .upsert(NotificationUpsert {
                source_type: "todo".to_owned(),
                source_id: "1".to_owned(),
                dedupe_key: "todo:1:reminder".to_owned(),
                target: PushTarget::qq_official(PushTargetType::Private, "u1"),
                channel: "qq".to_owned(),
                kind: "todo_reminder".to_owned(),
                payload: json!({
                    "message_type": "text",
                    "text": "提醒",
                    "fallback_text": "提醒"
                }),
                scheduled_at: "2020-01-01T09:00:00+08:00".to_owned(),
                max_attempts: 3,
                reactivate_cancelled: false,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn worker_sends_due_notification() {
        let store = test_store();
        upsert_due(&store);
        let sink = Arc::new(TestPushSink::default());
        let worker = NotificationWorker::new(
            store.clone(),
            sink.clone(),
            NotificationWorkerConfig::default(),
        );

        let stats = worker.run_once().await.unwrap();
        let task = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(task.status, NotificationStatus::Sent);
        assert_eq!(sink.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn worker_reschedules_recurring_todo_reminder_after_sent() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::days(1);
        let previous_local = previous.format("%Y-%m-%d %H:%M:%S").to_string();
        let todo = todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: "报时".to_owned(),
                    detail: None,
                    raw_text: Some("每天提醒我报时".to_owned()),
                    due_date: None,
                    due_at: None,
                    reminder_at: Some(previous_local.clone()),
                    time_precision: TodoTimePrecision::DateTime,
                    recurrence_kind: TodoRecurrenceKind::Daily,
                    recurrence_interval_days: 1,
                    recurrence_interval: 1,
                    recurrence_unit: TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
        notification_store
            .upsert(NotificationUpsert {
                source_type: "todo".to_owned(),
                source_id: todo.id.clone(),
                dedupe_key: format!("todo:{}:reminder:{}", todo.id, previous.to_rfc3339()),
                target: PushTarget::qq_official(PushTargetType::Private, "u1"),
                channel: "qq".to_owned(),
                kind: "todo_reminder".to_owned(),
                payload: json!({
                    "message_type": "text",
                    "text": "提醒",
                    "fallback_text": "提醒"
                }),
                scheduled_at: previous.to_rfc3339(),
                max_attempts: 3,
                reactivate_cancelled: false,
            })
            .unwrap();
        let sink = Arc::new(TestPushSink::default());
        let worker = NotificationWorker::new(
            notification_store.clone(),
            sink,
            NotificationWorkerConfig::default(),
        )
        .with_todo_store(todo_store.clone());

        let stats = worker.run_once().await.unwrap();
        let tasks = notification_store.list_all_for_test().unwrap();
        let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].status, NotificationStatus::Sent);
        assert_eq!(tasks[1].status, NotificationStatus::Pending);
        assert_ne!(
            updated.reminder_at.as_deref(),
            Some(previous_local.as_str())
        );
        assert_eq!(tasks[1].source_id, todo.id);
        assert_eq!(tasks[1].kind, "todo_reminder");
    }

    #[tokio::test]
    async fn worker_marks_failed_push_for_retry() {
        let store = test_store();
        upsert_due(&store);
        let worker = NotificationWorker::new(
            store.clone(),
            Arc::new(TestPushSink {
                requests: Mutex::new(Vec::new()),
                fail: true,
            }),
            NotificationWorkerConfig::default(),
        );

        let stats = worker.run_once().await.unwrap();
        let task = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();

        assert_eq!(stats.failed_count, 1);
        assert_eq!(task.status, NotificationStatus::Retry);
    }
}
