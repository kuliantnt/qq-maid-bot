use super::*;

pub(crate) fn message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "group:g1".to_owned(),
        user_id: Some("u1".to_owned()),
        group_member_role: Some("owner".to_owned()),
        group_id: Some("g1".to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

pub(crate) fn private_message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "private:u1".to_owned(),
        conversation_kind: qq_maid_common::identity_context::ConversationKind::Private,
        conversation_id: Some("u1".to_owned()),
        user_id: Some("u1".to_owned()),
        platform: "qq_official".to_owned(),
        ..empty_respond_request()
    }
}

pub(crate) fn private_todo_owner() -> TodoOwner {
    TodoStore::owner(Some("u1"), "private:u1")
}

pub(crate) fn todo_pending(pending: Option<&PreparedAction>) -> Option<TodoPendingPayload> {
    pending.and_then(|pending| TodoPendingPayload::try_from_pending(pending).ok().flatten())
}

pub(crate) fn raw_tool_result(
    name: &str,
    output: serde_json::Value,
    succeeded: bool,
) -> ToolExecutionResult {
    ToolExecutionResult {
        name: name.to_owned(),
        output,
        succeeded,
    }
}

pub(crate) fn private_test_meta() -> SessionMeta {
    SessionMeta::new(
        "private:u1",
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    )
}

pub(crate) fn todo_draft(title: impl Into<String>) -> TodoItemDraft {
    TodoItemDraft {
        title: title.into(),
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

pub(crate) fn create_private_todo(
    service: &RustRespondService,
    title: impl Into<String>,
) -> TodoItem {
    service
        .task_store
        .create(&private_todo_owner(), todo_draft(title))
        .unwrap()
}

pub(crate) fn create_private_todo_due_date(
    service: &RustRespondService,
    title: impl Into<String>,
    due_date: impl Into<String>,
) -> TodoItem {
    service
        .task_store
        .create(
            &private_todo_owner(),
            TodoItemDraft {
                due_date: Some(due_date.into()),
                time_precision: TodoTimePrecision::Date,
                ..todo_draft(title)
            },
        )
        .unwrap()
}

pub(crate) fn create_numbered_private_todos(
    service: &RustRespondService,
    prefix: &str,
    range: std::ops::RangeInclusive<u32>,
) -> Vec<TodoItem> {
    range
        .map(|index| create_private_todo(service, format!("{prefix} {index}")))
        .collect()
}

pub(crate) fn active_private_session(
    service: &RustRespondService,
) -> crate::runtime::session::SessionRecord {
    service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap()
}

pub(crate) fn last_todo_snapshot(service: &RustRespondService, context: &str) -> LastTodoQuery {
    active_private_session(service)
        .last_todo_query
        .unwrap_or_else(|| panic!("missing {context} snapshot"))
}

pub(crate) fn first_snapshot_item(
    service: &RustRespondService,
    owner: &TodoOwner,
    snapshot: &LastTodoQuery,
    context: &str,
) -> (String, String) {
    let item_id = snapshot
        .result_ids
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("{context} snapshot should contain first item"));
    let title = service
        .task_store
        .get_by_id(owner, &item_id)
        .unwrap()
        .unwrap_or_else(|| panic!("{context} snapshot first item should still exist"))
        .title;
    (item_id, title)
}

pub(crate) fn assert_refreshed_pending_snapshot(
    service: &RustRespondService,
    owner: &TodoOwner,
    visible_count: usize,
) {
    let remaining = service.task_store.list_pending(owner).unwrap();
    let snapshot = last_todo_snapshot(service, "refreshed");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(
        snapshot.result_ids,
        remaining
            .iter()
            .take(visible_count)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.result_ids.len(), visible_count);
}

pub(crate) async fn execute_tool_json(
    tool_request: &ToolChatRequest,
    tool_name: &str,
    arguments: &str,
) -> Value {
    let output = tool_request
        .tools
        .execute_json(&tool_request.tool_context, tool_name, arguments)
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

pub(crate) async fn complete_first_visible_todo(tool_request: &ToolChatRequest) -> Value {
    execute_tool_json(
        tool_request,
        "complete_todos",
        r#"{"numbers":[1],"reference":null}"#,
    )
    .await
}

pub(crate) fn newest_tool_request(inspector: &MockProvider, context: &str) -> ToolChatRequest {
    inspector
        .tool_requests()
        .pop()
        .unwrap_or_else(|| panic!("missing tool request {context}"))
}

pub(crate) fn seed_scoped_memory(
    service: &RustRespondService,
    scope_type: MemoryScopeType,
    scope_id: &str,
    creator: &str,
    group_id: Option<&str>,
    content: &str,
) {
    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type,
            scope_id: scope_id.to_owned(),
            created_by_user_id: creator.to_owned(),
            user_id: Some(creator.to_owned()),
            group_id: group_id.map(str::to_owned),
            content: content.to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();
}

pub(crate) fn message_in_scope(
    text: &str,
    scope_key: &str,
    user_id: &str,
    group_id: &str,
) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: scope_key.to_owned(),
        user_id: Some(user_id.to_owned()),
        group_member_role: Some("owner".to_owned()),
        group_id: Some(group_id.to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

pub(crate) fn test_meta() -> SessionMeta {
    SessionMeta::new(
        "group:g1",
        Some("u1".to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
    )
}

#[derive(Debug, Clone)]
pub(crate) struct SeededCompletedTodos {
    pub(crate) old_id: String,
    pub(crate) yesterday_id: String,
}

fn completed_at_for(date: NaiveDate, hour: u32) -> String {
    format!("{}T{hour:02}:00:00+08:00", date.format("%Y-%m-%d"))
}

pub(crate) fn seed_completed_time_todos(store: &TodoStore) -> SeededCompletedTodos {
    let today = request_time_context().local_date();
    let yesterday = today - Duration::days(1);
    let before_yesterday = today - Duration::days(2);
    let old_completed_at = completed_at_for(before_yesterday, 8);
    let yesterday_completed_at = completed_at_for(yesterday, 9);
    let today_completed_at = completed_at_for(today, 10);
    let old_created_at = completed_at_for(before_yesterday, 6);
    let yesterday_created_at = completed_at_for(before_yesterday, 7);
    let today_created_at = completed_at_for(before_yesterday, 8);
    let missing_created_at = completed_at_for(before_yesterday, 9);
    let pending_created_at = completed_at_for(today, 11);

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    store
        .set_items_for_test(
            &owner,
            &[
                TodoItem {
                    id: "1".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "前天完成".to_owned(),
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
                    created_at: old_created_at,
                    updated_at: old_completed_at.clone(),
                    completed_at: Some(old_completed_at),
                },
                TodoItem {
                    id: "2".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "昨天完成".to_owned(),
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
                    created_at: yesterday_created_at,
                    updated_at: yesterday_completed_at.clone(),
                    completed_at: Some(yesterday_completed_at),
                },
                TodoItem {
                    id: "3".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "今天完成".to_owned(),
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
                    created_at: today_created_at,
                    updated_at: today_completed_at.clone(),
                    completed_at: Some(today_completed_at),
                },
                TodoItem {
                    id: "4".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "没有完成时间".to_owned(),
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
                    created_at: missing_created_at.clone(),
                    updated_at: missing_created_at,
                    completed_at: None,
                },
                TodoItem {
                    id: "6".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "未完成旧截止".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-01-01".to_owned()),
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::Date,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                    status: TodoStatus::Pending,
                    created_at: pending_created_at.clone(),
                    updated_at: pending_created_at,
                    completed_at: None,
                },
            ],
        )
        .unwrap();

    SeededCompletedTodos {
        old_id: "1".to_owned(),
        yesterday_id: "2".to_owned(),
    }
}
