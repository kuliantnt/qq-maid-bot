use super::*;

pub(super) fn test_context() -> ToolContext {
    ToolContext {
        task_id: "msg-1".to_owned(),
        actor: ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "qq_official".to_owned(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: Some("call-1".to_owned()),
        execution_deadline: None,
    }
}

pub(super) fn todo_pending(
    pending: Option<&crate::runtime::pending::PreparedAction>,
) -> Option<TodoPendingPayload> {
    pending.and_then(|pending| TodoPendingPayload::try_from_pending(pending).ok().flatten())
}

pub(super) fn test_stores() -> (
    TodoStore,
    SessionStore,
    crate::storage::notification::NotificationOutboxStore,
    TodoOwner,
) {
    let database = SqliteDatabase::open_temp("todo-tool-tests", APP_MIGRATIONS).unwrap();
    let todo_store = TodoStore::new(database.clone());
    let session_store = SessionStore::new(database.clone());
    let notification_store = crate::storage::notification::NotificationOutboxStore::new(database);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    (todo_store, session_store, notification_store, owner)
}

pub(super) fn create_item_value(index: usize) -> Value {
    json!({
        "content": format!("批量事项 {index}"),
        "title": null,
        "detail": null,
        "due_date": null,
        "due_at": null,
        "reminder_at": null,
        "time_precision": null
    })
}

pub(super) fn tool_test_draft(title: &str) -> TodoItemDraft {
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

pub(super) fn batch_create_arguments(count: usize) -> Value {
    json!({
        "items": (1..=count).map(create_item_value).collect::<Vec<_>>(),
        "content": null,
        "title": null,
        "detail": null,
        "due_date": null,
        "due_at": null,
        "reminder_at": null,
        "time_precision": null
    })
}

pub(super) fn json_type_contains(value: &Value, expected: &str) -> bool {
    match value.get("type") {
        Some(Value::String(actual)) => actual == expected,
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

pub(super) fn schema_property<'a>(schema: &'a Value, field: &str) -> &'a Value {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(field))
        .unwrap_or_else(|| panic!("missing schema property {field}"))
}

pub(super) fn assert_nullable_type(schema: &Value, field: &str, value_type: &str, label: &str) {
    let property = schema_property(schema, field);
    assert!(
        json_type_contains(property, value_type) && json_type_contains(property, "null"),
        "{label} {field} must accept {value_type}|null"
    );
}

pub(super) fn assert_schema_max_items(schema: &Value, field: &str, expected: usize, label: &str) {
    assert_eq!(
        schema_property(schema, field)["maxItems"],
        json!(expected),
        "{label} {field} maxItems must use the shared limit"
    );
}

pub(super) fn assert_pending_todo_count(
    todo_store: &TodoStore,
    owner: &TodoOwner,
    expected: usize,
) {
    assert_eq!(todo_store.list_pending(owner).unwrap().len(), expected);
}

pub(super) fn create_batch_tool(
    todo_store: TodoStore,
    session_store: SessionStore,
    notification_store: crate::storage::notification::NotificationOutboxStore,
) -> CreateTodoTool {
    CreateTodoTool::new(todo_store, session_store, notification_store)
}

pub(super) async fn execute_batch_create(
    create_tool: &CreateTodoTool,
    count: usize,
) -> Result<ToolOutput, LlmError> {
    create_tool
        .execute(test_context(), batch_create_arguments(count))
        .await
}

pub(super) fn tool_order_items() -> Vec<TodoItem> {
    vec![
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
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
            scope_key: "private:u1".to_owned(),
            title: "后天事项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-03".to_owned()),
            due_at: None,
            reminder_at: None,
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
            scope_key: "private:u1".to_owned(),
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
            scope_key: "private:u1".to_owned(),
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
            scope_key: "private:u1".to_owned(),
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
