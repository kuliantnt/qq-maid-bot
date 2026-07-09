//! Todo 每日提醒后台调度。
//!
//! 当前提醒只面向可验证 private target 的个人待办：群内 Todo 仍保留现有查询/操作语义，
//! 但不会主动推回群里，避免暴露按个人 owner 归属的待办内容。
//! 调度器只生产统一通知任务，实际投递、重试和失败终态由 Notification Worker 处理。

use std::{collections::HashSet, time::Duration};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use qq_maid_common::time_context::{
    format_todo_time_for_display, local_date_from_timestamp, shanghai_offset,
};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::{
    config::DailyReminderTime,
    runtime::{
        push::{PushTarget, PushTargetType},
        tools::todo::{
            TodoItem, TodoReminderOwnerQueryResult, TodoReminderOwnerSkipReason, TodoStore,
        },
    },
    storage::notification::{NotificationOutboxStore, NotificationStatus, NotificationUpsert},
};

const MAX_ITEMS_PER_SECTION: usize = 10;
// 每日提醒默认只在固定时点触发一次；若这一轮存在临时失败，需要在当天补跑，
// 避免把本应今天入队的提醒直接拖到下一次日常调度。投递失败由 Notification Worker 重试。
const FAILED_RUN_RETRY_DELAY: Duration = Duration::from_secs(300);
// 调度层只做一次当天补跑：已入队 owner 通过稳定 dedupe_key 跳过，入队失败 owner 可被重试，
// 同时避免数据库长时间不可用时在同一天内无限循环占用后台任务。
const MAX_SCHEDULED_ATTEMPTS_PER_DAY: usize = 2;

#[derive(Debug, Clone, Copy)]
pub struct TodoReminderSchedulerConfig {
    pub enabled: bool,
    pub reminder_time: DailyReminderTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoReminderRunStats {
    pub candidate_owner_count: usize,
    pub skipped_owner_count: usize,
    pub queued_owner_count: usize,
    pub enqueue_failed_owner_count: usize,
    pub empty_owner_count: usize,
    pub already_queued_owner_count: usize,
    pub duplicate_owner_count: usize,
}

#[derive(Clone)]
pub struct TodoReminderScheduler {
    store: TodoStore,
    notification_store: NotificationOutboxStore,
    config: TodoReminderSchedulerConfig,
    retry_delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormattedReminder {
    markdown: String,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReminderDisplayItem {
    title: String,
    due_label: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ReminderBuckets {
    today: Vec<ReminderDisplayItem>,
    overdue: Vec<ReminderDisplayItem>,
    no_date: Vec<ReminderDisplayItem>,
}

enum ReminderClassification {
    Today(ReminderDisplayItem),
    Overdue(ReminderDisplayItem),
    NoDate(ReminderDisplayItem),
    Future,
}

impl TodoReminderScheduler {
    pub fn new(
        store: TodoStore,
        notification_store: NotificationOutboxStore,
        config: TodoReminderSchedulerConfig,
    ) -> Self {
        Self {
            store,
            notification_store,
            config,
            retry_delay: FAILED_RUN_RETRY_DELAY,
        }
    }

    pub fn spawn(self) {
        if !self.config.enabled {
            info!("todo daily reminder disabled");
            return;
        }
        tokio::spawn(async move {
            info!(
                reminder_time = %self.config.reminder_time,
                "todo daily reminder scheduler enabled"
            );
            self.run_loop().await;
        });
    }

    async fn run_loop(self) {
        loop {
            let now = Utc::now().with_timezone(&shanghai_offset());
            let next_run = next_run_after(now, self.config.reminder_time);
            let wait_duration = (next_run - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            debug!(
                next_run_at = %next_run.to_rfc3339(),
                reminder_time = %self.config.reminder_time,
                "todo daily reminder waiting for next run"
            );
            tokio::time::sleep(wait_duration).await;
            self.run_scheduled_cycle_for_date(next_run.date_naive())
                .await;
        }
    }

    async fn run_scheduled_cycle_for_date(&self, scheduled_date: NaiveDate) {
        let mut attempt = 1usize;
        loop {
            match self.run_once_for_date(scheduled_date).await {
                Ok(stats) if stats.enqueue_failed_owner_count == 0 => {
                    if attempt > 1 {
                        info!(
                            scheduled_date = %scheduled_date,
                            attempt,
                            "todo daily reminder retry finished successfully"
                        );
                    }
                    return;
                }
                Ok(stats) => {
                    warn!(
                        scheduled_date = %scheduled_date,
                        attempt,
                        enqueue_failed_owner_count = stats.enqueue_failed_owner_count,
                        "todo daily reminder cycle had enqueue failures; scheduling same-day retry"
                    );
                }
                Err(err) => {
                    warn!(
                        scheduled_date = %scheduled_date,
                        attempt,
                        error = %err,
                        "todo daily reminder cycle failed; scheduling same-day retry"
                    );
                }
            }

            if attempt >= MAX_SCHEDULED_ATTEMPTS_PER_DAY {
                warn!(
                    scheduled_date = %scheduled_date,
                    attempt,
                    "todo daily reminder retry attempts exhausted for today"
                );
                return;
            }

            let now = Utc::now().with_timezone(&shanghai_offset());
            let Some(retry_at) = next_retry_after(now, scheduled_date, self.retry_delay) else {
                warn!(
                    scheduled_date = %scheduled_date,
                    attempt,
                    "todo daily reminder retry window closed for today"
                );
                return;
            };
            let wait_duration = (retry_at - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            debug!(
                scheduled_date = %scheduled_date,
                attempt,
                retry_at = %retry_at.to_rfc3339(),
                "todo daily reminder waiting to retry failed cycle"
            );
            tokio::time::sleep(wait_duration).await;
            attempt += 1;
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub async fn run_once(&self) -> Result<TodoReminderRunStats, String> {
        self.run_once_for_date(Utc::now().with_timezone(&shanghai_offset()).date_naive())
            .await
    }

    async fn run_once_for_date(&self, today: NaiveDate) -> Result<TodoReminderRunStats, String> {
        let owner_result = self
            .store
            .list_private_reminder_owners()
            .map_err(|err| err.message().to_owned())?;
        log_skipped_owners(&owner_result);

        let mut stats = TodoReminderRunStats {
            candidate_owner_count: owner_result.candidates.len(),
            skipped_owner_count: owner_result.skipped.len(),
            ..TodoReminderRunStats::default()
        };
        let mut seen_owners = HashSet::new();
        for owner in owner_result.candidates {
            if !seen_owners.insert(owner.owner_key.clone()) {
                stats.duplicate_owner_count += 1;
                continue;
            }
            let dedupe_key = daily_reminder_dedupe_key(&owner.owner_key, today);
            if self
                .notification_store
                .get_by_dedupe_key(&dedupe_key)
                .map_err(|err| err.message().to_owned())?
                .is_some()
            {
                stats.already_queued_owner_count += 1;
                continue;
            }

            let items = self
                .store
                .list_pending_for_private_scopes(&owner.owner_key, &owner.private_scope_keys)
                .map_err(|err| err.message().to_owned())?;
            let Some(message) = format_reminder_message(&items, today) else {
                stats.empty_owner_count += 1;
                continue;
            };

            let target = PushTarget::from_scope_key_or_qq_official(
                &owner.primary_private_scope_key,
                PushTargetType::Private,
                owner.private_target_id.clone(),
            );
            match self.notification_store.upsert(NotificationUpsert {
                source_type: "todo".to_owned(),
                source_id: daily_reminder_source_id(&owner.owner_key, today),
                dedupe_key,
                target,
                channel: "push".to_owned(),
                kind: "todo_daily_reminder".to_owned(),
                payload: serde_json::json!({
                    "message_type": "markdown",
                    "text": message.markdown,
                    "fallback_text": message.text,
                }),
                scheduled_at: scheduled_at_for_date(today, self.config.reminder_time),
                max_attempts: 5,
                reactivate_cancelled: false,
            }) {
                Ok(task) => {
                    if task.status == NotificationStatus::Cancelled {
                        stats.already_queued_owner_count += 1;
                        info!(
                            owner = %short_hash(&owner.owner_key),
                            target = %short_hash(&owner.private_target_id),
                            "todo daily reminder task remains cancelled"
                        );
                        continue;
                    }
                    stats.queued_owner_count += 1;
                    info!(
                        owner = %short_hash(&owner.owner_key),
                        target = %short_hash(&owner.private_target_id),
                        "todo daily reminder notification queued"
                    );
                }
                Err(err) => {
                    stats.enqueue_failed_owner_count += 1;
                    warn!(
                        owner = %short_hash(&owner.owner_key),
                        target = %short_hash(&owner.private_target_id),
                        error = %err.message(),
                        "todo daily reminder notification enqueue failed"
                    );
                }
            }
        }
        Ok(stats)
    }
}

fn next_run_after(
    now: DateTime<FixedOffset>,
    reminder_time: DailyReminderTime,
) -> DateTime<FixedOffset> {
    let offset = shanghai_offset();
    let today = now.date_naive();
    let today_run = offset
        .with_ymd_and_hms(
            today.year(),
            today.month(),
            today.day(),
            reminder_time.hour.into(),
            reminder_time.minute.into(),
            0,
        )
        .single()
        .expect("Asia/Shanghai uses a stable fixed offset");
    if now <= today_run {
        today_run
    } else {
        let tomorrow = today.succ_opt().expect("valid next date");
        offset
            .with_ymd_and_hms(
                tomorrow.year(),
                tomorrow.month(),
                tomorrow.day(),
                reminder_time.hour.into(),
                reminder_time.minute.into(),
                0,
            )
            .single()
            .expect("Asia/Shanghai uses a stable fixed offset")
    }
}

fn next_retry_after(
    now: DateTime<FixedOffset>,
    scheduled_date: NaiveDate,
    retry_delay: Duration,
) -> Option<DateTime<FixedOffset>> {
    if now.date_naive() != scheduled_date {
        return None;
    }
    let retry_at = now + chrono::Duration::from_std(retry_delay).ok()?;
    (retry_at.date_naive() == scheduled_date).then_some(retry_at)
}

fn format_reminder_message(items: &[TodoItem], today: NaiveDate) -> Option<FormattedReminder> {
    let mut buckets = ReminderBuckets::default();
    for item in items {
        match classify_item(item, today) {
            ReminderClassification::Today(display) => buckets.today.push(display),
            ReminderClassification::Overdue(display) => buckets.overdue.push(display),
            ReminderClassification::NoDate(display) => buckets.no_date.push(display),
            ReminderClassification::Future => {}
        }
    }
    if buckets.today.is_empty() && buckets.overdue.is_empty() && buckets.no_date.is_empty() {
        return None;
    }

    let markdown = render_reminder("## 今日待办提醒", &buckets, true);
    let text = render_reminder("【今日待办提醒】", &buckets, false);
    Some(FormattedReminder { markdown, text })
}

fn render_reminder(header: &str, buckets: &ReminderBuckets, markdown: bool) -> String {
    let mut output = String::from(header);
    append_section(&mut output, "今日任务", &buckets.today, markdown);
    append_section(&mut output, "逾期任务", &buckets.overdue, markdown);
    append_section(&mut output, "无日期任务", &buckets.no_date, markdown);
    output.push_str("\n\n查看更多 /todo");
    output
}

fn append_section(output: &mut String, title: &str, items: &[ReminderDisplayItem], markdown: bool) {
    if items.is_empty() {
        return;
    }
    output.push_str(if markdown { "\n\n### " } else { "\n\n" });
    output.push_str(title);
    for item in items.iter().take(MAX_ITEMS_PER_SECTION) {
        output.push_str("\n- ");
        output.push_str(&render_item_line(item, markdown));
    }
    let omitted = items.len().saturating_sub(MAX_ITEMS_PER_SECTION);
    if omitted > 0 {
        output.push_str(&format!("\n- 另有 {omitted} 项未展示"));
    }
}

fn render_item_line(item: &ReminderDisplayItem, markdown: bool) -> String {
    let title = if markdown {
        escape_markdown(&item.title)
    } else {
        item.title.clone()
    };
    match &item.due_label {
        Some(due_label) => format!("{due_label} {title}"),
        None => title,
    }
}

fn classify_item(item: &TodoItem, today: NaiveDate) -> ReminderClassification {
    if let Some(due_at) = non_empty(item.due_at.as_deref()) {
        return local_date_from_timestamp(due_at)
            .map(|date| {
                classify_due_date(
                    date,
                    today,
                    item,
                    Some(format_todo_time_for_display(due_at)),
                )
            })
            .unwrap_or_else(|| ReminderClassification::NoDate(display_item(item, None)));
    }
    if let Some(due_date) = non_empty(item.due_date.as_deref()) {
        return NaiveDate::parse_from_str(due_date, "%Y-%m-%d")
            .map(|date| {
                classify_due_date(
                    date,
                    today,
                    item,
                    Some(format_todo_time_for_display(due_date)),
                )
            })
            .unwrap_or_else(|_| ReminderClassification::NoDate(display_item(item, None)));
    }
    ReminderClassification::NoDate(display_item(item, None))
}

fn classify_due_date(
    due_date: NaiveDate,
    today: NaiveDate,
    item: &TodoItem,
    due_label: Option<String>,
) -> ReminderClassification {
    let display = display_item(item, due_label);
    if due_date == today {
        ReminderClassification::Today(display)
    } else if due_date < today {
        ReminderClassification::Overdue(display)
    } else {
        ReminderClassification::Future
    }
}

fn display_item(item: &TodoItem, due_label: Option<String>) -> ReminderDisplayItem {
    ReminderDisplayItem {
        title: sanitize_title(&item.title),
        due_label,
    }
}

fn sanitize_title(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "未命名待办".to_owned()
    } else {
        collapsed
    }
}

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '(' | ')' | '`' | '#') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn daily_reminder_source_id(owner_key: &str, date: NaiveDate) -> String {
    format!("daily-reminder:{date}:{}", stable_hash(owner_key))
}

fn daily_reminder_dedupe_key(owner_key: &str, date: NaiveDate) -> String {
    format!("todo:daily-reminder:{date}:{}", stable_hash(owner_key))
}

fn scheduled_at_for_date(date: NaiveDate, reminder_time: DailyReminderTime) -> String {
    shanghai_offset()
        .with_ymd_and_hms(
            date.year(),
            date.month(),
            date.day(),
            reminder_time.hour.into(),
            reminder_time.minute.into(),
            0,
        )
        .single()
        .expect("Asia/Shanghai uses a stable fixed offset")
        .to_rfc3339()
}

fn log_skipped_owners(result: &TodoReminderOwnerQueryResult) {
    for skipped in &result.skipped {
        let reason = match skipped.reason {
            TodoReminderOwnerSkipReason::InvalidPrivateScope => "invalid_private_scope",
            TodoReminderOwnerSkipReason::ConflictingPrivateTargets => "conflicting_private_targets",
        };
        warn!(
            owner = %short_hash(&skipped.owner_key),
            reason,
            scope_count = skipped.private_scope_keys.len(),
            scope_hashes = ?hash_values(&skipped.private_scope_keys),
            parsed_target_hashes = ?hash_values(&skipped.parsed_target_ids),
            "todo reminder skipped owner candidate"
        );
    }
}

fn hash_values(values: &[String]) -> Vec<String> {
    values.iter().map(|value| short_hash(value)).collect()
}

fn short_hash(value: &str) -> String {
    stable_hash(value).chars().take(10).collect()
}

fn stable_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        runtime::tools::todo::{TodoItemDraft, TodoTimePrecision},
        storage::{APP_MIGRATIONS, database::SqliteDatabase, notification::NotificationStatus},
    };

    fn test_stores() -> (TodoStore, NotificationOutboxStore) {
        let database = SqliteDatabase::open_temp("qq-maid-todo-reminder", APP_MIGRATIONS).unwrap();
        (
            TodoStore::new(database.clone()),
            NotificationOutboxStore::new(database),
        )
    }

    fn reminder_scheduler(
        store: TodoStore,
        notification_store: NotificationOutboxStore,
    ) -> TodoReminderScheduler {
        TodoReminderScheduler::new(
            store,
            notification_store,
            TodoReminderSchedulerConfig {
                enabled: true,
                reminder_time: DailyReminderTime { hour: 9, minute: 0 },
            },
        )
    }

    fn create_todo(
        store: &TodoStore,
        owner: &crate::runtime::tools::todo::TodoOwner,
        title: &str,
        due_date: Option<&str>,
        due_at: Option<&str>,
    ) {
        store
            .create(
                owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: due_date.map(str::to_owned),
                    due_at: due_at.map(str::to_owned),
                    reminder_at: None,
                    time_precision: if due_at.is_some() {
                        TodoTimePrecision::DateTime
                    } else if due_date.is_some() {
                        TodoTimePrecision::Date
                    } else {
                        TodoTimePrecision::None
                    },
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }

    fn enable_daily_reminder(store: &TodoStore, owner: &crate::runtime::tools::todo::TodoOwner) {
        store.set_daily_reminder_enabled(owner, true).unwrap();
    }

    #[tokio::test]
    async fn run_once_queues_one_private_reminder_per_owner_per_day() {
        let (store, notification_store) = test_stores();
        let owner_same_scope = TodoStore::owner(Some("u1"), "private:u1");
        let owner_dirty_scope = TodoStore::owner(Some("u1"), "private: u1");
        let future_owner = TodoStore::owner(Some("u2"), "private:u2");
        create_todo(
            &store,
            &owner_same_scope,
            "今天检查日志",
            Some("2026-06-24"),
            None,
        );
        create_todo(
            &store,
            &owner_dirty_scope,
            "昨天补充说明",
            Some("2026-06-23"),
            None,
        );
        create_todo(&store, &future_owner, "明天再做", Some("2026-06-25"), None);
        enable_daily_reminder(&store, &owner_same_scope);
        enable_daily_reminder(&store, &owner_dirty_scope);
        enable_daily_reminder(&store, &future_owner);

        let scheduler = reminder_scheduler(store, notification_store.clone());
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.queued_owner_count, 1);
        assert_eq!(first.empty_owner_count, 1);
        let tasks = notification_store.list_all_for_test().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].source_type, "todo");
        assert_eq!(tasks[0].kind, "todo_daily_reminder");
        assert_eq!(tasks[0].channel, "push");
        assert_eq!(tasks[0].target.target_id, "u1");
        assert_eq!(tasks[0].scheduled_at, "2026-06-24T09:00:00+08:00");
        assert_eq!(tasks[0].payload["message_type"], "markdown");
        assert!(
            tasks[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("今日任务")
        );
        assert!(
            tasks[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("逾期任务")
        );
        assert!(
            tasks[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("今天检查日志")
        );
        assert!(
            tasks[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("昨天补充说明")
        );
        assert!(
            tasks[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("查看更多 /todo")
        );
        assert!(!tasks[0].payload["text"].as_str().unwrap().contains("[1]"));
        assert!(
            tasks[0].payload["fallback_text"]
                .as_str()
                .unwrap()
                .contains("查看更多 /todo")
        );

        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.already_queued_owner_count, 1);
        assert_eq!(notification_store.list_all_for_test().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_once_future_only_is_silent_and_does_not_mark_queued() {
        let (store, notification_store) = test_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(&store, &owner, "未来任务", Some("2026-06-25"), None);
        enable_daily_reminder(&store, &owner);

        let scheduler = reminder_scheduler(store.clone(), notification_store.clone());
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.queued_owner_count, 0);
        assert_eq!(first.empty_owner_count, 1);
        assert!(notification_store.list_all_for_test().unwrap().is_empty());

        create_todo(&store, &owner, "今天补记", Some("2026-06-24"), None);
        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.queued_owner_count, 1);
        assert_eq!(notification_store.list_all_for_test().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_once_includes_due_at_today_without_reminder_at() {
        let (store, notification_store) = test_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(
            &store,
            &owner,
            "今天带时间但不单独提醒",
            None,
            Some("2026-06-24 18:30:00"),
        );
        enable_daily_reminder(&store, &owner);

        let scheduler = reminder_scheduler(store, notification_store.clone());
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let stats = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(stats.queued_owner_count, 1);
        let tasks = notification_store.list_all_for_test().unwrap();
        assert_eq!(tasks.len(), 1);
        let payload = tasks[0].payload["text"].as_str().unwrap();
        assert!(payload.contains("今日任务"));
        assert!(payload.contains("今天带时间但不单独提醒"));
        assert_eq!(tasks[0].scheduled_at, "2026-06-24T09:00:00+08:00");
    }

    #[tokio::test]
    async fn disabled_scheduler_spawn_does_not_enqueue_daily_summary() {
        let (store, notification_store) = test_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(&store, &owner, "今天不应摘要推送", Some("2026-06-24"), None);
        enable_daily_reminder(&store, &owner);
        let scheduler = TodoReminderScheduler::new(
            store,
            notification_store.clone(),
            TodoReminderSchedulerConfig {
                enabled: false,
                reminder_time: DailyReminderTime { hour: 9, minute: 0 },
            },
        );

        scheduler.spawn();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(notification_store.list_all_for_test().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_once_skips_owner_when_daily_summary_preference_is_disabled() {
        let (store, notification_store) = test_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(&store, &owner, "今天不应发送", Some("2026-06-24"), None);
        store.set_daily_reminder_enabled(&owner, true).unwrap();
        store.set_daily_reminder_enabled(&owner, false).unwrap();

        let scheduler = reminder_scheduler(store, notification_store.clone());
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let stats = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(stats.candidate_owner_count, 0);
        assert!(notification_store.list_all_for_test().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_once_skips_existing_daily_reminder_task_regardless_of_delivery_status() {
        let (store, notification_store) = test_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        create_todo(&store, &owner, "今天已入队", Some("2026-06-24"), None);
        enable_daily_reminder(&store, &owner);

        let scheduler = reminder_scheduler(store, notification_store.clone());
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();

        let first = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(first.queued_owner_count, 1);
        let task = notification_store.list_all_for_test().unwrap()[0].clone();
        notification_store
            .mark_failed(task.id, "temporary", 60)
            .unwrap();

        let second = scheduler.run_once_for_date(today).await.unwrap();
        assert_eq!(second.queued_owner_count, 0);
        assert_eq!(second.already_queued_owner_count, 1);
        let tasks = notification_store.list_all_for_test().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, NotificationStatus::Retry);
    }

    #[test]
    fn formatter_uses_due_at_precedence_and_hides_future_items() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 24).unwrap();
        let items = vec![
            TodoItem {
                id: "1".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "due-at 优先".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("2026-06-23T16:30:00+00:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                status: crate::runtime::tools::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
            },
            TodoItem {
                id: "2".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "未来 due_at 覆盖过期 due_date".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("2026-06-25 09:00:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                status: crate::runtime::tools::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
            },
            TodoItem {
                id: "3".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "坏 due-at 归无日期".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-20".to_owned()),
                due_at: Some("bad data".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                status: crate::runtime::tools::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
            },
            TodoItem {
                id: "4".to_owned(),
                user_id: Some("u1".to_owned()),
                scope_key: "private:u1".to_owned(),
                title: "无日期任务".to_owned(),
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
                status: crate::runtime::tools::todo::TodoStatus::Pending,
                created_at: "2026-06-20T00:00:00+08:00".to_owned(),
                updated_at: "2026-06-20T00:00:00+08:00".to_owned(),
                completed_at: None,
            },
        ];

        let formatted = format_reminder_message(&items, today).unwrap();

        assert!(formatted.markdown.contains("今日任务"));
        assert!(formatted.markdown.contains("due-at 优先"));
        assert!(!formatted.markdown.contains("未来 due_at 覆盖过期 due_date"));
        assert!(formatted.markdown.contains("无日期任务"));
        assert!(formatted.markdown.contains("坏 due-at 归无日期"));
        assert!(formatted.markdown.contains("查看更多 /todo"));
        assert!(formatted.text.contains("due-at 优先"));
        assert!(formatted.text.contains("无日期任务"));
    }
}
