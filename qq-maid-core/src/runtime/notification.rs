//! 统一通知投递 Worker。
//!
//! Worker 只处理 Outbox 任务的领取、投递结果回写和失败重试，不根据 source_type
//! 反查业务表，也不重新组装 Todo / RSS 等业务语义。业务如果需要在发送成功后
//! 更新自己的状态，只能通过通用 sent hook 订阅已发送任务，由业务模块自行判断来源。

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use qq_maid_common::time_context::shanghai_offset;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::{
    runtime::push::{PushError, PushIntent, PushSink},
    service::VisibleEntitySnapshot,
    storage::notification::{NotificationOutboxStore, NotificationTask, NotificationWriteOutcome},
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
    pub lease_lost_count: usize,
}

#[derive(Clone)]
pub struct NotificationWorker {
    store: NotificationOutboxStore,
    after_sent_hook: Option<Arc<dyn NotificationSentHook>>,
    push_sink: Arc<dyn PushSink>,
    config: NotificationWorkerConfig,
    worker_id: String,
}

#[async_trait]
pub trait NotificationSentHook: Send + Sync {
    async fn after_sent(&self, task: &NotificationTask);
}

#[derive(Debug, Deserialize)]
struct NotificationPushPayload {
    #[serde(default)]
    message_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    fallback_text: Option<String>,
    #[serde(default)]
    parts: Vec<NotificationPushPart>,
    #[serde(default)]
    visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

#[derive(Debug, Deserialize)]
struct NotificationPushPart {
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
            after_sent_hook: None,
            push_sink,
            config,
            worker_id: new_worker_id(),
        }
    }

    pub fn with_after_sent_hook(mut self, hook: Arc<dyn NotificationSentHook>) -> Self {
        self.after_sent_hook = Some(hook);
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
            match self.deliver(&task).await {
                Ok(delivered_parts) => {
                    match self
                        .store
                        .mark_sent(task.id, &self.worker_id, delivered_parts)
                        .map_err(|err| err.message().to_owned())?
                    {
                        NotificationWriteOutcome::Applied => {
                            self.after_sent(&task).await;
                            stats.sent_count += 1;
                        }
                        NotificationWriteOutcome::LeaseLost => {
                            stats.lease_lost_count += 1;
                            warn!(
                                task_id = task.id,
                                "notification worker lease lost before mark_sent"
                            );
                        }
                    }
                }
                Err(DeliveryError::InvalidPayload(message)) => {
                    match self.mark_failed(&task, &message).await? {
                        MarkFailedOutcome::Applied => stats.invalid_payload_count += 1,
                        MarkFailedOutcome::LeaseLost => stats.lease_lost_count += 1,
                    }
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        "notification task payload invalid"
                    );
                }
                Err(DeliveryError::Push(err)) => {
                    let summary = safe_push_error(&err);
                    match self.mark_failed(&task, &summary).await? {
                        MarkFailedOutcome::Applied => stats.failed_count += 1,
                        MarkFailedOutcome::LeaseLost => stats.lease_lost_count += 1,
                    }
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        error = %summary,
                        "notification push failed"
                    );
                }
                Err(DeliveryError::Progress(message)) => {
                    match self.mark_failed(&task, &message).await? {
                        MarkFailedOutcome::Applied => stats.failed_count += 1,
                        MarkFailedOutcome::LeaseLost => stats.lease_lost_count += 1,
                    }
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        "notification part progress update failed"
                    );
                }
                Err(DeliveryError::LeaseLost) => {
                    stats.lease_lost_count += 1;
                    warn!(
                        task_id = task.id,
                        source_type = %task.source_type,
                        kind = %task.kind,
                        "notification worker lease lost during multipart delivery"
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
                lease_lost = stats.lease_lost_count,
                "notification worker cycle finished"
            );
        }
        Ok(stats)
    }

    async fn deliver(&self, task: &NotificationTask) -> Result<u32, DeliveryError> {
        let payload: NotificationPushPayload = serde_json::from_value(task.payload.clone())
            .map_err(|err| DeliveryError::InvalidPayload(format!("invalid push payload: {err}")))?;
        let parts = payload.validated_parts()?;
        let part_count = u32::try_from(parts.len()).map_err(|_| {
            DeliveryError::InvalidPayload(
                "push payload part count exceeds supported range".to_owned(),
            )
        })?;
        let delivered_parts = usize::try_from(task.delivered_parts).unwrap_or(usize::MAX);
        if delivered_parts > parts.len() {
            return Err(DeliveryError::InvalidPayload(
                "delivered_parts exceeds push payload part count".to_owned(),
            ));
        }
        for (index, part) in parts.into_iter().enumerate().skip(delivered_parts) {
            self.push_sink
                .push(PushIntent {
                    target: task.target.clone(),
                    message_type: part.message_type,
                    text: part.text,
                    fallback_text: part.fallback_text,
                    visible_entity_snapshot: payload.visible_entity_snapshot.clone(),
                })
                .await
                .map_err(DeliveryError::Push)?;
            let outcome = self
                .store
                .mark_part_delivered(task.id, &self.worker_id, index as u32)
                .map_err(|err| DeliveryError::Progress(err.message().to_owned()))?;
            if outcome == NotificationWriteOutcome::LeaseLost {
                return Err(DeliveryError::LeaseLost);
            }
        }
        Ok(part_count)
    }

    async fn mark_failed(
        &self,
        task: &NotificationTask,
        message: &str,
    ) -> Result<MarkFailedOutcome, String> {
        match self
            .store
            .mark_failed(
                task.id,
                &self.worker_id,
                message,
                retry_delay_seconds(self.config.retry_delay),
            )
            .map_err(|err| err.message().to_owned())?
        {
            NotificationWriteOutcome::Applied => Ok(MarkFailedOutcome::Applied),
            NotificationWriteOutcome::LeaseLost => {
                warn!(
                    task_id = task.id,
                    "notification worker lease lost before mark_failed"
                );
                Ok(MarkFailedOutcome::LeaseLost)
            }
        }
    }

    async fn after_sent(&self, task: &NotificationTask) {
        if let Some(hook) = &self.after_sent_hook {
            hook.after_sent(task).await;
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
    Progress(String),
    LeaseLost,
}

enum MarkFailedOutcome {
    Applied,
    LeaseLost,
}

impl NotificationPushPayload {
    fn validated_parts(&self) -> Result<Vec<NotificationPushPart>, DeliveryError> {
        let parts = if self.parts.is_empty() {
            vec![NotificationPushPart {
                message_type: self.message_type.clone().unwrap_or_default(),
                text: self.text.clone().unwrap_or_default(),
                fallback_text: self.fallback_text.clone(),
            }]
        } else {
            if self.message_type.is_some() || self.text.is_some() || self.fallback_text.is_some() {
                return Err(DeliveryError::InvalidPayload(
                    "push payload cannot mix legacy fields with parts".to_owned(),
                ));
            }
            self.parts
                .iter()
                .map(|part| NotificationPushPart {
                    message_type: part.message_type.clone(),
                    text: part.text.clone(),
                    fallback_text: part.fallback_text.clone(),
                })
                .collect()
        };
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| part.message_type.trim().is_empty() || part.text.trim().is_empty())
        {
            return Err(DeliveryError::InvalidPayload(
                "push payload requires non-empty message_type and text for every part".to_owned(),
            ));
        }
        Ok(parts)
    }
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
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use serde_json::json;

    use crate::{
        runtime::push::{PushResult, PushTarget, PushTargetType},
        storage::{
            database::SqliteDatabase,
            notification::{NOTIFICATION_MIGRATIONS, NotificationStatus, NotificationUpsert},
        },
    };

    use super::*;

    #[derive(Default)]
    struct TestPushSink {
        requests: Mutex<Vec<PushIntent>>,
        fail: bool,
    }

    #[derive(Default)]
    struct FailSecondPartOnceSink {
        attempts: Mutex<Vec<String>>,
        failed_second: Mutex<bool>,
    }

    struct LeaseStealingSink {
        store: NotificationOutboxStore,
        requests: Mutex<Vec<PushIntent>>,
    }

    #[derive(Default)]
    struct CountingSentHook {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl PushSink for FailSecondPartOnceSink {
        async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
            self.attempts.lock().unwrap().push(intent.text.clone());
            if intent.text == "part-2" {
                let mut failed = self.failed_second.lock().unwrap();
                if !*failed {
                    *failed = true;
                    return Err(PushError::Failed {
                        summary: "temporary second part failure".to_owned(),
                    });
                }
            }
            Ok(PushResult { message_id: None })
        }
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

    #[async_trait]
    impl PushSink for LeaseStealingSink {
        async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
            self.requests.lock().unwrap().push(intent);
            self.store
                .claim_due("worker-b", 10, "9999-01-01T00:00:00+08:00")
                .unwrap();
            Ok(PushResult { message_id: None })
        }
    }

    #[async_trait]
    impl NotificationSentHook for CountingSentHook {
        async fn after_sent(&self, _task: &NotificationTask) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn test_store() -> NotificationOutboxStore {
        let database =
            SqliteDatabase::open_temp("notification-worker-tests", NOTIFICATION_MIGRATIONS)
                .unwrap();
        NotificationOutboxStore::new(database)
    }

    async fn run_until_status(
        worker: &NotificationWorker,
        store: &NotificationOutboxStore,
        dedupe_key: &str,
        expected: NotificationStatus,
    ) -> NotificationWorkerStats {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let stats = worker.run_once().await.unwrap();
            let task = store.get_by_dedupe_key(dedupe_key).unwrap().unwrap();
            if task.status == expected {
                return stats;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "notification did not reach {expected:?} before timeout; actual={:?}",
                task.status
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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

    fn upsert_multipart(store: &NotificationOutboxStore, dedupe_key: &str) -> NotificationTask {
        store
            .upsert(NotificationUpsert {
                source_type: "ops".to_owned(),
                source_id: "ops-1".to_owned(),
                dedupe_key: dedupe_key.to_owned(),
                target: PushTarget::qq_official(PushTargetType::Private, "u1"),
                channel: "push".to_owned(),
                kind: "ops_result".to_owned(),
                payload: json!({
                    "parts": [
                        {"message_type":"markdown", "text":"part-1", "fallback_text":"part-1"},
                        {"message_type":"markdown", "text":"part-2", "fallback_text":"part-2"}
                    ]
                }),
                scheduled_at: "2020-01-01T09:00:00+08:00".to_owned(),
                max_attempts: 3,
                reactivate_cancelled: false,
            })
            .unwrap()
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

    #[tokio::test]
    async fn multipart_retry_resumes_after_persisted_successful_prefix() {
        let store = test_store();
        store
            .upsert(NotificationUpsert {
                source_type: "ops".to_owned(),
                source_id: "ops-1".to_owned(),
                dedupe_key: "ops:ops-1:result".to_owned(),
                target: PushTarget::qq_official(PushTargetType::Private, "u1"),
                channel: "push".to_owned(),
                kind: "ops_result".to_owned(),
                payload: json!({
                    "parts": [
                        {"message_type":"markdown", "text":"part-1", "fallback_text":"part-1"},
                        {"message_type":"markdown", "text":"part-2", "fallback_text":"part-2"},
                        {"message_type":"markdown", "text":"part-3", "fallback_text":"part-3"}
                    ]
                }),
                scheduled_at: "2020-01-01T09:00:00+08:00".to_owned(),
                max_attempts: 3,
                reactivate_cancelled: false,
            })
            .unwrap();
        let sink = Arc::new(FailSecondPartOnceSink::default());
        let worker = NotificationWorker::new(
            store.clone(),
            sink.clone(),
            NotificationWorkerConfig {
                retry_delay: Duration::ZERO,
                ..NotificationWorkerConfig::default()
            },
        );

        let first = worker.run_once().await.unwrap();
        let retry = store
            .get_by_dedupe_key("ops:ops-1:result")
            .unwrap()
            .unwrap();
        assert_eq!(first.failed_count, 1);
        assert_eq!(retry.status, NotificationStatus::Retry);
        assert_eq!(retry.delivered_parts, 1);

        let second = run_until_status(
            &worker,
            &store,
            "ops:ops-1:result",
            NotificationStatus::Sent,
        )
        .await;
        let sent = store
            .get_by_dedupe_key("ops:ops-1:result")
            .unwrap()
            .unwrap();

        assert_eq!(second.sent_count, 1);
        assert_eq!(sent.status, NotificationStatus::Sent);
        assert_eq!(sent.delivered_parts, 3);
        assert_eq!(
            *sink.attempts.lock().unwrap(),
            vec!["part-1", "part-2", "part-2", "part-3"]
        );
    }

    #[tokio::test]
    async fn changed_payload_and_target_restart_from_first_new_part() {
        let store = test_store();
        let task = upsert_multipart(&store, "ops:changed:result");
        store
            .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
            .unwrap();
        store.mark_part_delivered(task.id, "worker-a", 0).unwrap();
        store
            .mark_failed(task.id, "worker-a", "temporary", 0)
            .unwrap();
        let changed_target = PushTarget::onebot11("bot-b", PushTargetType::Group, "group-b");
        store
            .upsert(NotificationUpsert {
                source_type: "ops".to_owned(),
                source_id: "ops-1".to_owned(),
                dedupe_key: "ops:changed:result".to_owned(),
                target: changed_target.clone(),
                channel: "push".to_owned(),
                kind: "ops_result".to_owned(),
                payload: json!({
                    "parts": [
                        {"message_type":"text", "text":"new-part-1"},
                        {"message_type":"text", "text":"new-part-2"}
                    ]
                }),
                scheduled_at: "2020-01-01T09:00:00+08:00".to_owned(),
                max_attempts: 3,
                reactivate_cancelled: false,
            })
            .unwrap();
        let sink = Arc::new(TestPushSink::default());
        let worker = NotificationWorker::new(
            store.clone(),
            sink.clone(),
            NotificationWorkerConfig::default(),
        );

        let stats = worker.run_once().await.unwrap();
        let requests = sink.requests.lock().unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].text, "new-part-1");
        assert_eq!(requests[1].text, "new-part-2");
        assert!(
            requests
                .iter()
                .all(|intent| intent.target == changed_target)
        );
    }

    #[tokio::test]
    async fn lost_lease_stops_worker_without_retry_or_after_sent_hook() {
        let store = test_store();
        upsert_multipart(&store, "ops:lease-lost:result");
        let sink = Arc::new(LeaseStealingSink {
            store: store.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let hook = Arc::new(CountingSentHook::default());
        let worker = NotificationWorker::new(
            store.clone(),
            sink.clone(),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(hook.clone());

        let stats = worker.run_once().await.unwrap();
        let task = store
            .get_by_dedupe_key("ops:lease-lost:result")
            .unwrap()
            .unwrap();

        assert_eq!(stats.lease_lost_count, 1);
        assert_eq!(stats.sent_count, 0);
        assert_eq!(stats.failed_count, 0);
        assert_eq!(hook.calls.load(Ordering::SeqCst), 0);
        assert_eq!(sink.requests.lock().unwrap().len(), 1);
        assert_eq!(task.status, NotificationStatus::Sending);
        assert_eq!(task.locked_by.as_deref(), Some("worker-b"));
        assert_eq!(task.delivered_parts, 0);
    }

    #[tokio::test]
    async fn retry_after_uncommitted_mark_sent_does_not_resend_confirmed_body() {
        let store = test_store();
        let task = upsert_multipart(&store, "ops:mark-sent-retry:result");
        store
            .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
            .unwrap();
        store.mark_part_delivered(task.id, "worker-a", 0).unwrap();
        store.mark_part_delivered(task.id, "worker-a", 1).unwrap();
        // 模拟正文分段均已确认，但首次 mark_sent 没有成功提交，任务仍留在 sending。
        store
            .database()
            .connection()
            .unwrap()
            .execute(
                "UPDATE notification_outbox SET locked_at = '2000-01-01T00:00:00+08:00' WHERE id = ?1",
                [task.id],
            )
            .unwrap();
        let sink = Arc::new(TestPushSink::default());
        let worker = NotificationWorker::new(
            store.clone(),
            sink.clone(),
            NotificationWorkerConfig::default(),
        );

        let stats = worker.run_once().await.unwrap();
        let sent = store
            .get_by_dedupe_key("ops:mark-sent-retry:result")
            .unwrap()
            .unwrap();

        assert_eq!(stats.sent_count, 1);
        assert!(sink.requests.lock().unwrap().is_empty());
        assert_eq!(sent.status, NotificationStatus::Sent);
        assert_eq!(sent.delivered_parts, 2);
    }
}
