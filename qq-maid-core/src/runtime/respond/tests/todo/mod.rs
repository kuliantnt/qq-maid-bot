use super::support::*;
use crate::runtime::{
    pending::PreparedAction,
    session::{SessionMeta, now_iso_cn},
    tools::todo::{
        ClarificationCandidate, PendingTodoClarification, TodoItem, TodoItemDraft,
        TodoPendingPayload, TodoStatus, TodoStore, TodoTimePrecision,
    },
};
use chrono::Duration;
use serde_json::{Value, json};

fn draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
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
    }
}

fn todo_pending(pending: Option<&PreparedAction>) -> Option<TodoPendingPayload> {
    pending.and_then(|pending| TodoPendingPayload::try_from_pending(pending).ok().flatten())
}

fn draft_due_date(title: &str, due_date: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: Some(due_date.to_owned()),
        due_at: None,
        reminder_at: None,
        time_precision: TodoTimePrecision::Date,
        recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
    }
}

fn draft_due_at(title: &str, due_at: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: Some(due_at.to_owned()),
        reminder_at: None,
        time_precision: TodoTimePrecision::DateTime,
        recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
    }
}

fn assert_in_order(text: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let offset = text[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered text: {needle}"));
        cursor += offset + needle.len();
    }
}

fn status_list_items() -> Vec<TodoItem> {
    vec![
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "无时间事项".to_owned(),
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
            status: TodoStatus::Pending,
            created_at: "2026-07-01T12:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T12:00:00+08:00".to_owned(),
            completed_at: None,
        },
        TodoItem {
            id: "2".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "后天事项".to_owned(),
            detail: Some("需要保留详情".to_owned()),
            raw_text: None,
            due_date: Some("2026-07-03".to_owned()),
            due_at: None,
            reminder_at: Some("2026-07-03 09:30:00".to_owned()),
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T11:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T11:00:00+08:00".to_owned(),
            completed_at: None,
        },
        TodoItem {
            id: "3".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "明天事项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-02".to_owned()),
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T10:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T10:00:00+08:00".to_owned(),
            completed_at: None,
        },
        TodoItem {
            id: "4".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较早归档".to_owned(),
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
            status: TodoStatus::Completed,
            created_at: "2026-07-01T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T09:00:00+08:00".to_owned(),
            completed_at: Some("2026-06-30T18:00:00+08:00".to_owned()),
        },
        TodoItem {
            id: "5".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较新归档".to_owned(),
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
            status: TodoStatus::Completed,
            created_at: "2026-07-01T08:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T08:00:00+08:00".to_owned(),
            completed_at: Some("2026-07-01T18:00:00+08:00".to_owned()),
        },
    ]
}

fn last_todo_result_ids(service: &crate::runtime::respond::RustRespondService) -> Vec<String> {
    service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .last_todo_query
        .expect("missing todo query snapshot")
        .result_ids
}

fn private_todo_meta() -> SessionMeta {
    SessionMeta::new(
        "private:u1",
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    )
}

fn private_todo_message(text: &str) -> crate::runtime::respond::RespondRequest {
    message_in_scope(text, "private:u1", "u1", "")
}

fn clarification_candidates(items: &[TodoItem]) -> Vec<ClarificationCandidate> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| ClarificationCandidate {
            id: item.id.clone(),
            display_number: index + 1,
            title: item.title.clone(),
            status: item.status.clone(),
        })
        .collect()
}

fn install_todo_clarification(
    service: &crate::runtime::respond::RustRespondService,
    tool_name: &str,
    arguments: Value,
    allow_many: bool,
    created_at: String,
    candidates: Vec<ClarificationCandidate>,
) {
    let mut session = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap();
    session.pending_operation = Some(
        TodoPendingPayload::TodoClarify {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: "u1".to_owned(),
            request: PendingTodoClarification {
                tool_name: tool_name.to_owned(),
                arguments,
                allow_many,
                error_code: "todo_reference_unavailable".to_owned(),
                question: "请补充要操作哪条待办。".to_owned(),
                candidates,
                created_at: created_at.clone(),
            },
            created_at,
        }
        .into_prepared_action(&session.scope_key),
    );
    service.session_store.save(&mut session).unwrap();
}

mod clarification;
mod commands;
mod query;
mod rendering;
mod write;
