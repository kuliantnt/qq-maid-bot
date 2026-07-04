//! Todo 单次提醒与统一 Notification Outbox 的衔接。
//!
//! Todo 业务层负责解释提醒时间和渲染内容快照；通知层只负责按计划投递这个快照。

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde_json::json;

use crate::{
    identity::{group_raw_target_from_scope_key, private_raw_target_from_scope_key},
    runtime::{
        push::{PushTarget, PushTargetType},
        todo::{
            TodoItem, TodoItemDraft, TodoOwner,
            template::{TodoPushBody, format_todo_single_reminder_push},
        },
    },
    storage::notification::{NotificationOutboxStore, NotificationUpsert},
    util::time_context::shanghai_offset,
};

const TODO_REMINDER_SOURCE: &str = "todo";
const TODO_REMINDER_KIND: &str = "todo_reminder";

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
    use super::*;
    use crate::{
        runtime::todo::{TodoStatus, TodoStore, TodoTimePrecision},
        storage::{
            database::SqliteDatabase,
            notification::{NOTIFICATION_MIGRATIONS, NotificationOutboxStore, NotificationStatus},
        },
    };

    fn test_store() -> NotificationOutboxStore {
        let database =
            SqliteDatabase::open_temp("todo-reminder-tests", NOTIFICATION_MIGRATIONS).unwrap();
        NotificationOutboxStore::new(database)
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
            status: TodoStatus::Pending,
            created_at: "2026-07-03T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-03T09:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
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
}
