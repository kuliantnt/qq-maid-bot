//! Todo 单次提醒与统一 Notification Outbox 的衔接。
//!
//! Todo 业务层负责解释提醒时间和渲染内容快照；通知层只负责按计划投递这个快照。

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use qq_maid_common::time_context::shanghai_offset;
use serde_json::json;
use tracing::{debug, warn};

use crate::{
    identity::{group_raw_target_from_scope_key, private_raw_target_from_scope_key},
    runtime::{
        notification::NotificationSentHook,
        push::{PushTarget, PushTargetType},
        tools::todo::{TodoItem, TodoItemDraft, TodoOwner, todo_item_visible_entity_snapshot},
    },
    storage::notification::{NotificationOutboxStore, NotificationTask, NotificationUpsert},
};

use super::template::{TodoPushBody, format_todo_single_reminder_push};

const TODO_REMINDER_SOURCE: &str = "todo";
const TODO_REMINDER_KIND: &str = "todo_reminder";

#[derive(Clone)]
pub struct TodoReminderSentHook {
    todo_store: crate::runtime::tools::todo::TodoStore,
    notification_store: NotificationOutboxStore,
}

impl TodoReminderSentHook {
    pub fn new(
        todo_store: crate::runtime::tools::todo::TodoStore,
        notification_store: NotificationOutboxStore,
    ) -> Self {
        Self {
            todo_store,
            notification_store,
        }
    }
}

#[async_trait]
impl NotificationSentHook for TodoReminderSentHook {
    async fn after_sent(&self, task: &NotificationTask) {
        if task.source_type != TODO_REMINDER_SOURCE || task.kind != TODO_REMINDER_KIND {
            return;
        }
        match self
            .todo_store
            .advance_recurring_reminder_by_id(&task.source_id, &task.scheduled_at)
        {
            Ok(Some((owner, item))) => {
                if let Err(err) = sync_reminder_task(&self.notification_store, &owner, &item) {
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

pub fn validate_draft_reminder(draft: &TodoItemDraft) -> Result<(), String> {
    let Some(reminder_at) = draft.reminder_at.as_deref() else {
        return Ok(());
    };
    let scheduled_at = parse_reminder_at(reminder_at)?;
    if scheduled_at <= Utc::now().with_timezone(&shanghai_offset()) {
        return Err("提醒时间必须晚于当前时间。".to_owned());
    }
    Ok(())
}

pub fn sync_reminder_task(
    store: &NotificationOutboxStore,
    owner: &TodoOwner,
    item: &TodoItem,
) -> Result<(), String> {
    let Some(reminder_at) = item.reminder_at.as_deref() else {
        cancel_reminder_task(store, item)?;
        return Ok(());
    };
    let scheduled_at = parse_reminder_at(reminder_at)?;
    if scheduled_at <= Utc::now().with_timezone(&shanghai_offset()) {
        cancel_reminder_task(store, item)?;
        return Ok(());
    }
    let target = push_target_from_scope(&owner.scope_key)
        .ok_or_else(|| "当前会话没有可用的提醒推送目标。".to_owned())?;
    let visible_entity_snapshot = todo_item_visible_entity_snapshot(
        &target.platform,
        target.account_id.as_deref(),
        &owner.scope_key,
        owner,
        item,
        Some("todo_reminder"),
    );
    // 单次提醒重排时，新时间会生成新 dedupe_key；先取消旧的未发送任务，
    // 避免未来时间 A 改到 B 后两个 pending/retry 提醒都发出。sent 记录由存储层保留。
    cancel_reminder_task(store, item)?;
    let message = render_todo_reminder(item);
    store
        .upsert(NotificationUpsert {
            source_type: TODO_REMINDER_SOURCE.to_owned(),
            source_id: item.id.clone(),
            dedupe_key: reminder_dedupe_key(item, &scheduled_at),
            target,
            channel: "qq".to_owned(),
            kind: TODO_REMINDER_KIND.to_owned(),
            payload: json!({
                "message_type": "markdown",
                "text": message.markdown,
                "fallback_text": message.text,
                "visible_entity_snapshot": visible_entity_snapshot,
            }),
            scheduled_at: scheduled_at.to_rfc3339(),
            max_attempts: 5,
            reactivate_cancelled: true,
        })
        .map(|_| ())
        .map_err(|err| err.message().to_owned())
}

pub fn cancel_reminder_task(
    store: &NotificationOutboxStore,
    item: &TodoItem,
) -> Result<(), String> {
    cancel_reminder_task_by_id(store, &item.id)
}

pub fn cancel_reminder_task_by_id(
    store: &NotificationOutboxStore,
    item_id: &str,
) -> Result<(), String> {
    store
        .cancel_by_source(TODO_REMINDER_SOURCE, item_id)
        .map(|_| ())
        .map_err(|err| err.message().to_owned())
}

pub fn parse_reminder_at(value: &str) -> Result<DateTime<FixedOffset>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("提醒时间不能为空。".to_owned());
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Ok(datetime.with_timezone(&shanghai_offset()));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return shanghai_offset()
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| "提醒时间无效。".to_owned());
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M") {
        return shanghai_offset()
            .from_local_datetime(&naive)
            .single()
            .ok_or_else(|| "提醒时间无效。".to_owned());
    }
    Err("提醒时间必须是 YYYY-MM-DD HH:MM 或 RFC3339。".to_owned())
}

fn push_target_from_scope(scope_key: &str) -> Option<PushTarget> {
    if let Some(target_id) = private_raw_target_from_scope_key(scope_key) {
        return Some(PushTarget::from_scope_key_or_qq_official(
            scope_key,
            PushTargetType::Private,
            target_id,
        ));
    }
    if let Some(target_id) = group_raw_target_from_scope_key(scope_key) {
        return Some(PushTarget::from_scope_key_or_qq_official(
            scope_key,
            PushTargetType::Group,
            target_id,
        ));
    }
    None
}

fn reminder_dedupe_key(item: &TodoItem, scheduled_at: &DateTime<FixedOffset>) -> String {
    // 单次提醒的业务事件由“待办 + 规范化提醒时间”共同确定。
    // 已发送任务不会被 outbox 重开；用户把已提醒过的待办改到新时间时，应生成新事件。
    format!("todo:{}:reminder:{}", item.id, scheduled_at.to_rfc3339())
}

fn render_todo_reminder(item: &TodoItem) -> TodoPushBody {
    format_todo_single_reminder_push(item)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Duration as ChronoDuration;

    use super::*;
    use crate::{
        runtime::{
            notification::{NotificationWorker, NotificationWorkerConfig},
            push::{PushError, PushIntent, PushResult, PushSink},
            tools::todo::{
                TodoRecurrenceKind, TodoRecurrenceUnit, TodoStatus, TodoStore, TodoTimePrecision,
            },
        },
        storage::{
            APP_MIGRATIONS,
            database::SqliteDatabase,
            notification::{
                NOTIFICATION_MIGRATIONS, NotificationOutboxStore, NotificationStatus,
                NotificationUpsert,
            },
        },
    };

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
            SqliteDatabase::open_temp("todo-reminder-tests", NOTIFICATION_MIGRATIONS).unwrap();
        NotificationOutboxStore::new(database)
    }

    fn test_app_stores() -> (TodoStore, NotificationOutboxStore) {
        let database =
            SqliteDatabase::open_temp("todo-reminder-app-tests", APP_MIGRATIONS).unwrap();
        (
            TodoStore::new(database.clone()),
            NotificationOutboxStore::new(database),
        )
    }

    fn create_recurring_todo_reminder(
        todo_store: &TodoStore,
        notification_store: &NotificationOutboxStore,
        owner: &TodoOwner,
        previous: DateTime<FixedOffset>,
        interval: u32,
        unit: TodoRecurrenceUnit,
    ) -> TodoItem {
        let recurrence_kind = match unit {
            TodoRecurrenceUnit::Minute => TodoRecurrenceKind::EveryNMinutes,
            TodoRecurrenceUnit::Hour => TodoRecurrenceKind::EveryNHours,
            TodoRecurrenceUnit::Day => {
                if interval == 1 {
                    TodoRecurrenceKind::Daily
                } else {
                    TodoRecurrenceKind::EveryNDays
                }
            }
            TodoRecurrenceUnit::Week => TodoRecurrenceKind::EveryNWeeks,
            TodoRecurrenceUnit::Month => TodoRecurrenceKind::EveryNMonths,
            TodoRecurrenceUnit::Year => TodoRecurrenceKind::EveryNYears,
        };
        let previous_local = previous.format("%Y-%m-%d %H:%M:%S").to_string();
        let todo = todo_store
            .create(
                owner,
                TodoItemDraft {
                    title: "报时".to_owned(),
                    detail: None,
                    raw_text: Some("重复提醒我报时".to_owned()),
                    due_date: None,
                    due_at: None,
                    reminder_at: Some(previous_local),
                    time_precision: TodoTimePrecision::DateTime,
                    recurrence_kind,
                    recurrence_interval_days: if matches!(unit, TodoRecurrenceUnit::Day) {
                        interval
                    } else {
                        0
                    },
                    recurrence_interval: interval,
                    recurrence_unit: unit,
                },
            )
            .unwrap();
        notification_store
            .upsert(NotificationUpsert {
                source_type: TODO_REMINDER_SOURCE.to_owned(),
                source_id: todo.id.clone(),
                dedupe_key: format!("todo:{}:reminder:{}", todo.id, previous.to_rfc3339()),
                target: PushTarget::qq_official(PushTargetType::Private, "u1"),
                channel: "qq".to_owned(),
                kind: TODO_REMINDER_KIND.to_owned(),
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
        todo
    }

    fn reminder_item() -> TodoItem {
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
            title: "检查日志".to_owned(),
            detail: Some("确认没有推送失败".to_owned()),
            raw_text: None,
            due_date: None,
            due_at: Some("2099-01-01 10:00:00".to_owned()),
            reminder_at: Some("2099-01-01 09:30:00".to_owned()),
            time_precision: TodoTimePrecision::DateTime,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-03T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-03T09:00:00+08:00".to_owned(),
            completed_at: None,
        }
    }

    #[test]
    fn parse_reminder_accepts_local_datetime() {
        let parsed = parse_reminder_at("2099-01-01 09:30").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2099-01-01T09:30:00+08:00");
    }

    #[test]
    fn render_reminder_keeps_content_snapshot() {
        let item = reminder_item();

        let rendered = render_todo_reminder(&item);

        assert!(rendered.text.contains("检查日志"));
        assert!(rendered.markdown.contains("⏰ 待办提醒"));
        assert!(rendered.text.contains("确认没有推送失败"));
    }

    #[test]
    fn sync_reminder_reschedule_cancels_old_pending_task() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let mut item = reminder_item();

        sync_reminder_task(&store, &owner, &item).unwrap();
        item.reminder_at = Some("2099-01-02 09:30:00".to_owned());
        sync_reminder_task(&store, &owner, &item).unwrap();
        let tasks = store.list_all_for_test().unwrap();
        let old_task = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-01T09:30:00+08:00")
            .unwrap()
            .unwrap();
        let new_task = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-02T09:30:00+08:00")
            .unwrap()
            .unwrap();

        assert_eq!(tasks.len(), 2);
        assert_eq!(old_task.status, NotificationStatus::Cancelled);
        assert_eq!(new_task.status, NotificationStatus::Pending);
    }

    #[test]
    fn sync_reminder_uses_raw_target_from_stable_scope() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "platform:qq_official:account:app-1:private:u1");
        let item = reminder_item();

        sync_reminder_task(&store, &owner, &item).unwrap();
        let task = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-01T09:30:00+08:00")
            .unwrap()
            .unwrap();

        assert_eq!(task.target.target_type, PushTargetType::Private);
        assert_eq!(task.target.target_id, "u1");
    }

    #[test]
    fn explicit_reminder_sync_is_independent_from_daily_summary_scheduler() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let item = todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: "周五早上提醒我写周报".to_owned(),
                    detail: None,
                    raw_text: Some("周五早上提醒我写周报".to_owned()),
                    due_date: None,
                    due_at: None,
                    reminder_at: Some("2099-01-01 09:30:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                    recurrence_kind: TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();

        // 单条提醒只看 Todo 自身的 reminder_at，不读取每日摘要开关。
        sync_reminder_task(&notification_store, &owner, &item).unwrap();

        let tasks = notification_store.list_all_for_test().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, TODO_REMINDER_KIND);
        assert_eq!(tasks[0].scheduled_at, "2099-01-01T09:30:00+08:00");
    }

    #[test]
    fn sync_reminder_reactivates_cancelled_task() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let item = reminder_item();

        sync_reminder_task(&store, &owner, &item).unwrap();
        cancel_reminder_task(&store, &item).unwrap();
        let cancelled = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-01T09:30:00+08:00")
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.status, NotificationStatus::Cancelled);

        sync_reminder_task(&store, &owner, &item).unwrap();
        let reactivated = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-01T09:30:00+08:00")
            .unwrap()
            .unwrap();

        assert_eq!(reactivated.status, NotificationStatus::Pending);
        assert_eq!(reactivated.attempts, 0);
        assert!(reactivated.cancelled_at.is_none());
    }

    #[test]
    fn sync_reminder_after_sent_with_new_time_creates_new_task() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let mut item = reminder_item();

        sync_reminder_task(&store, &owner, &item).unwrap();
        let sent = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-01T09:30:00+08:00")
            .unwrap()
            .unwrap();
        store.mark_sent(sent.id).unwrap();

        item.reminder_at = Some("2099-01-02 09:30:00".to_owned());
        sync_reminder_task(&store, &owner, &item).unwrap();
        let tasks = store.list_all_for_test().unwrap();
        let new_task = store
            .get_by_dedupe_key("todo:1:reminder:2099-01-02T09:30:00+08:00")
            .unwrap()
            .unwrap();

        assert_eq!(tasks.len(), 2);
        assert_eq!(new_task.status, NotificationStatus::Pending);
        assert_eq!(new_task.scheduled_at, "2099-01-02T09:30:00+08:00");
    }

    #[tokio::test]
    async fn sent_hook_reschedules_recurring_todo_reminder_after_worker_sent() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::days(1);
        let previous_local = previous.format("%Y-%m-%d %H:%M:%S").to_string();
        let todo = create_recurring_todo_reminder(
            &todo_store,
            &notification_store,
            &owner,
            previous,
            1,
            TodoRecurrenceUnit::Day,
        );
        let worker = NotificationWorker::new(
            notification_store.clone(),
            Arc::new(TestPushSink::default()),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(Arc::new(TodoReminderSentHook::new(
            todo_store.clone(),
            notification_store.clone(),
        )));

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
        assert_eq!(tasks[1].kind, TODO_REMINDER_KIND);
    }

    #[tokio::test]
    async fn sent_hook_is_not_called_after_push_failure() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::minutes(5);
        let previous_local = previous.format("%Y-%m-%d %H:%M:%S").to_string();
        let todo = create_recurring_todo_reminder(
            &todo_store,
            &notification_store,
            &owner,
            previous,
            5,
            TodoRecurrenceUnit::Minute,
        );
        let worker = NotificationWorker::new(
            notification_store.clone(),
            Arc::new(TestPushSink {
                requests: Mutex::new(Vec::new()),
                fail: true,
            }),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(Arc::new(TodoReminderSentHook::new(
            todo_store.clone(),
            notification_store.clone(),
        )));

        let stats = worker.run_once().await.unwrap();
        let tasks = notification_store.list_all_for_test().unwrap();
        let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();

        assert_eq!(stats.failed_count, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, NotificationStatus::Retry);
        assert_eq!(
            updated.reminder_at.as_deref(),
            Some(previous_local.as_str())
        );
    }

    #[tokio::test]
    async fn sent_hook_does_not_reschedule_completed_todo_reminder() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::minutes(5);
        let previous_local = previous.format("%Y-%m-%d %H:%M:%S").to_string();
        let todo = create_recurring_todo_reminder(
            &todo_store,
            &notification_store,
            &owner,
            previous,
            5,
            TodoRecurrenceUnit::Minute,
        );
        todo_store.complete(&owner, &todo.id).unwrap();
        let worker = NotificationWorker::new(
            notification_store.clone(),
            Arc::new(TestPushSink::default()),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(Arc::new(TodoReminderSentHook::new(
            todo_store.clone(),
            notification_store.clone(),
        )));

        let stats = worker.run_once().await.unwrap();
        let tasks = notification_store.list_all_for_test().unwrap();
        let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, NotificationStatus::Sent);
        assert_eq!(updated.status, TodoStatus::Completed);
        assert_eq!(
            updated.reminder_at.as_deref(),
            Some(previous_local.as_str())
        );
    }

    #[tokio::test]
    async fn sent_hook_is_idempotent_for_duplicate_sent_outbox() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::minutes(5);
        let todo = create_recurring_todo_reminder(
            &todo_store,
            &notification_store,
            &owner,
            previous,
            5,
            TodoRecurrenceUnit::Minute,
        );
        let hook = TodoReminderSentHook::new(todo_store.clone(), notification_store.clone());
        let worker = NotificationWorker::new(
            notification_store.clone(),
            Arc::new(TestPushSink::default()),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(Arc::new(hook.clone()));

        let stats = worker.run_once().await.unwrap();
        let sent_task = notification_store
            .get_by_dedupe_key(&format!(
                "todo:{}:reminder:{}",
                todo.id,
                previous.to_rfc3339()
            ))
            .unwrap()
            .unwrap();
        let after_first = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();

        hook.after_sent(&sent_task).await;
        let tasks = notification_store.list_all_for_test().unwrap();
        let after_duplicate = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(tasks.len(), 2);
        assert_eq!(after_duplicate.reminder_at, after_first.reminder_at);
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.status == NotificationStatus::Pending)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn sent_hook_reschedules_missed_minute_reminder_to_single_future_outbox() {
        let (todo_store, notification_store) = test_app_stores();
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let previous =
            chrono::Utc::now().with_timezone(&shanghai_offset()) - ChronoDuration::minutes(10);
        let todo = create_recurring_todo_reminder(
            &todo_store,
            &notification_store,
            &owner,
            previous,
            1,
            TodoRecurrenceUnit::Minute,
        );
        let worker = NotificationWorker::new(
            notification_store.clone(),
            Arc::new(TestPushSink::default()),
            NotificationWorkerConfig::default(),
        )
        .with_after_sent_hook(Arc::new(TodoReminderSentHook::new(
            todo_store.clone(),
            notification_store.clone(),
        )));

        let stats = worker.run_once().await.unwrap();
        let tasks = notification_store.list_all_for_test().unwrap();
        let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();
        let next = parse_reminder_at(updated.reminder_at.as_deref().unwrap()).unwrap();

        assert_eq!(stats.sent_count, 1);
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.status == NotificationStatus::Pending)
                .count(),
            1
        );
        assert!(next > chrono::Utc::now().with_timezone(&shanghai_offset()));
    }
}
