// 拆分后这些不再随 `super::*` 自动进入命名空间，测试体里仍直接引用完整类型/宏。
use std::sync::Arc;

use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext};

use crate::runtime::pending::PendingOperation;
use crate::runtime::session::{SessionMeta, SessionStore};
use crate::runtime::todo::{
    TodoItem, TodoItemDraft, TodoOwner, TodoStatus, TodoStore, TodoTimePrecision,
};

use super::scope::{SelectionScope, TodoToolScope};
use super::{
    CancelTodoTool, CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool,
    ListTodoTool, MergeTodoTool,
};
use crate::storage::{APP_MIGRATIONS, database::SqliteDatabase};

use super::common::{
    TODO_TOOL_MAX_BATCH_CREATE_ITEMS, TODO_TOOL_MAX_NUMBERS, TodoReference, TodoSelectionRequest,
};

fn test_context() -> ToolContext {
    ToolContext {
        task_id: "msg-1".to_owned(),
        user_id: Some("u1".to_owned()),
        scope_id: "private:u1".to_owned(),
        tool_call_id: Some("call-1".to_owned()),
    }
}

fn test_stores() -> (
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

#[test]
fn todo_tool_scope_loads_legacy_and_stable_private_group_scopes() {
    let (_todo_store, session_store, _notification_store, _owner) = test_stores();
    for (scope_id, expected_group_id) in [
        ("private:u1", None),
        ("group:g1", Some("g1")),
        ("platform:qq_official:account:app-1:private:u1", None),
        ("platform:qq_official:account:app-1:group:g1", Some("g1")),
    ] {
        let context = ToolContext {
            scope_id: scope_id.to_owned(),
            ..test_context()
        };

        let scope = TodoToolScope::load(&session_store, &context, None)
            .unwrap_or_else(|err| panic!("{scope_id} should load, got {err}"));

        assert_eq!(scope.session.group_id.as_deref(), expected_group_id);
        assert_eq!(scope.owner.scope_key, scope_id);
    }
}

#[test]
fn todo_tool_scope_keeps_stable_private_and_group_distinct() {
    let (_todo_store, session_store, _notification_store, _owner) = test_stores();
    let private_context = ToolContext {
        scope_id: "platform:qq_official:account:app-1:private:u1".to_owned(),
        ..test_context()
    };
    let group_context = ToolContext {
        scope_id: "platform:qq_official:account:app-1:group:g1".to_owned(),
        ..test_context()
    };

    let private_scope = TodoToolScope::load(&session_store, &private_context, None).unwrap();
    let group_scope = TodoToolScope::load(&session_store, &group_context, None).unwrap();

    assert_eq!(private_scope.session.group_id, None);
    assert_eq!(group_scope.session.group_id.as_deref(), Some("g1"));
}

#[tokio::test]
async fn create_tool_accepts_stable_private_scope_context() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let stable_scope = "platform:qq_official:account:app-1:private:u1";
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store,
    );
    let context = ToolContext {
        scope_id: stable_scope.to_owned(),
        ..test_context()
    };
    let arguments = json!({
        "content":"今晚检查机器人日志",
        "title":null,
        "detail":null,
        "due_date":null,
        "due_at":null,
        "reminder_at": null,
        "time_precision":null
    });

    let output = create_tool.execute(context, arguments).await.unwrap();

    assert_ne!(
        output.value.get("error_code").and_then(Value::as_str),
        Some("permission_denied")
    );
    let owner = TodoStore::owner(Some("u1"), stable_scope);
    let todos = todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].scope_key, stable_scope);
}

fn create_item_value(index: usize) -> Value {
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

fn tool_test_draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        reminder_at: None,
        time_precision: TodoTimePrecision::None,
        recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
    }
}

fn batch_create_arguments(count: usize) -> Value {
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

fn json_type_contains(value: &Value, expected: &str) -> bool {
    match value.get("type") {
        Some(Value::String(actual)) => actual == expected,
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

fn tool_order_items() -> Vec<TodoItem> {
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T12:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T12:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T11:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T11:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T10:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T10:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T09:00:00+08:00".to_owned(),
            completed_at: Some("2026-06-30T18:00:00+08:00".to_owned()),
            cancelled_at: None,
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
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T08:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T08:00:00+08:00".to_owned(),
            completed_at: Some("2026-07-01T18:00:00+08:00".to_owned()),
            cancelled_at: None,
        },
        TodoItem {
            id: "6".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
            title: "最近放弃".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-04".to_owned()),
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T13:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T13:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T13:10:00+08:00".to_owned()),
        },
    ]
}

#[test]
fn todo_selector_schemas_allow_null_for_unused_strict_fields() {
    let (todo_store, session_store, notification_store, _) = test_stores();
    let schemas = vec![
        (
            "get_todo",
            GetTodoTool::new(todo_store.clone(), session_store.clone())
                .metadata()
                .parameters,
        ),
        (
            "complete_todos",
            CompleteTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .metadata()
            .parameters,
        ),
        (
            "cancel_todo",
            CancelTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .metadata()
            .parameters,
        ),
        (
            "restore_todos",
            super::RestoreTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .metadata()
            .parameters,
        ),
        (
            "delete_todos",
            DeleteTodoTool::new(
                todo_store.clone(),
                session_store.clone(),
                notification_store.clone(),
            )
            .metadata()
            .parameters,
        ),
    ];

    for (tool_name, schema) in schemas {
        let properties = schema["properties"].as_object().unwrap();
        assert!(
            json_type_contains(&properties["numbers"], "array")
                && json_type_contains(&properties["numbers"], "null"),
            "{tool_name} numbers must accept array|null"
        );
        assert_eq!(
            properties["numbers"]["maxItems"],
            json!(TODO_TOOL_MAX_NUMBERS),
            "{tool_name} numbers maxItems must use the shared selector limit"
        );
        assert!(
            json_type_contains(&properties["selection_text"], "string")
                && json_type_contains(&properties["selection_text"], "null"),
            "{tool_name} selection_text must accept string|null"
        );
        assert!(
            json_type_contains(&properties["reference"], "string")
                && json_type_contains(&properties["reference"], "null"),
            "{tool_name} reference must accept string|null"
        );
    }

    let edit_schema = EditTodoTool::new(todo_store, session_store, notification_store.clone())
        .metadata()
        .parameters;
    assert!(json_type_contains(
        &edit_schema["properties"]["number"],
        "integer"
    ));
    assert!(json_type_contains(
        &edit_schema["properties"]["number"],
        "null"
    ));
    assert!(json_type_contains(
        &edit_schema["properties"]["reference"],
        "string"
    ));
    assert!(json_type_contains(
        &edit_schema["properties"]["reference"],
        "null"
    ));
}

#[test]
fn list_todos_schema_requires_nullable_due_date_for_strict_tools() {
    let (todo_store, session_store, _, _) = test_stores();
    let schema = ListTodoTool::new(todo_store, session_store)
        .metadata()
        .parameters;
    let required = schema["required"].as_array().unwrap();

    assert!(required.contains(&json!("status")));
    assert!(required.contains(&json!("due_date")));
    assert!(required.contains(&json!("date_range_text")));
    assert!(json_type_contains(
        &schema["properties"]["due_date"],
        "string"
    ));
    assert!(json_type_contains(
        &schema["properties"]["due_date"],
        "null"
    ));
    assert!(json_type_contains(
        &schema["properties"]["date_range_text"],
        "string"
    ));
    assert!(json_type_contains(
        &schema["properties"]["date_range_text"],
        "null"
    ));
}

#[test]
fn todo_selection_request_counts_only_effective_selectors() {
    assert_eq!(
        super::common::todo_selection_request(
            &json!({"numbers": [1, 2, 3], "selection_text": null, "reference": null}),
            true,
        )
        .unwrap(),
        TodoSelectionRequest::Numbers(vec![1, 2, 3])
    );
    assert_eq!(
        super::common::todo_selection_request(
            &json!({"numbers": null, "selection_text": "1-3", "reference": null}),
            true,
        )
        .unwrap(),
        TodoSelectionRequest::Numbers(vec![1, 2, 3])
    );
    assert_eq!(
        super::common::todo_selection_request(
            &json!({"numbers": null, "selection_text": null, "reference": "last"}),
            true,
        )
        .unwrap(),
        TodoSelectionRequest::Reference(TodoReference::Last)
    );
    assert_eq!(
        super::common::todo_selection_request(
            &json!({"numbers": [], "selection_text": "1-2", "reference": null}),
            true,
        )
        .unwrap(),
        TodoSelectionRequest::Numbers(vec![1, 2])
    );
    assert_eq!(
        super::common::todo_selection_request(
            &json!({"numbers": [1], "selection_text": "   ", "reference": "   "}),
            true,
        )
        .unwrap(),
        TodoSelectionRequest::Numbers(vec![1])
    );

    let multiple = super::common::todo_selection_request(
        &json!({"numbers": [1], "selection_text": "1-3", "reference": null}),
        true,
    )
    .unwrap_err();
    assert_eq!(multiple.code, "bad_tool_arguments");
    assert!(multiple.message.contains("exactly one"));

    let missing = super::common::todo_selection_request(
        &json!({"numbers": null, "selection_text": "   ", "reference": null}),
        true,
    )
    .unwrap_err();
    assert_eq!(missing.code, "bad_tool_arguments");
    assert!(missing.message.contains("exactly one"));
}

#[test]
fn create_todo_schema_uses_shared_batch_limit() {
    let (todo_store, session_store, notification_store, _) = test_stores();
    let schema = CreateTodoTool::new(todo_store, session_store, notification_store.clone())
        .metadata()
        .parameters;
    assert_eq!(
        schema["properties"]["items"]["maxItems"],
        json!(TODO_TOOL_MAX_BATCH_CREATE_ITEMS)
    );
}

#[tokio::test]
async fn list_tool_all_uses_board_order_for_task_local_numbers_without_user_snapshot_pollution() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    todo_store
        .set_items_for_test(&owner, &tool_order_items())
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = list_tool
        .execute(test_context(), json!({"status":"all"}))
        .await
        .unwrap()
        .value;

    let titles = output["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            "明天事项",
            "后天事项",
            "无时间事项",
            "较新归档",
            "较早归档",
            "最近放弃"
        ]
    );
    assert_eq!(output["items"][0]["visible_number"], 1);

    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(
        session.last_todo_query.is_none(),
        "list_todos 是 Agent 内部查询，不应污染用户可见编号快照"
    );

    let completed = complete_tool
        .execute(test_context(), json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;
    assert_eq!(completed["completed"][0]["title"], "明天事项");
}

#[tokio::test]
async fn list_tool_due_date_filters_items_and_keeps_task_local_numbers() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let no_time = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "无时间".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let today = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "今天事项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-07-03".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "明天事项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-07-04".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    assert_ne!(no_time.id, today.id);

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    let output = list_tool
        .execute(
            context.clone(),
            json!({"status":"pending", "due_date":"2026-07-03"}),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(output["due_date"], "2026-07-03");
    assert_eq!(output["count"], 1);
    assert_eq!(output["items"][0]["title"], "今天事项");

    let completed = complete_tool
        .execute(context, json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;
    assert_eq!(completed["completed"][0]["title"], "今天事项");
}

#[tokio::test]
async fn list_tool_date_range_text_is_normalized_by_rust_context() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let ctx = crate::util::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    for (title, date) in [
        ("范围外事项", before_range),
        ("昨天事项", yesterday),
        ("今天事项", today),
    ] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some(date.format("%Y-%m-%d").to_string()),
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::Date,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let output = list_tool
        .execute(
            test_context(),
            json!({"status":"pending", "due_date": null, "date_range_text":"这两天"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["date_range_text"], "这两天");
    assert_eq!(output["date_range_field"], "planned");
    assert_eq!(
        output["due_start"],
        yesterday.format("%Y-%m-%d").to_string()
    );
    assert_eq!(output["due_end"], today.format("%Y-%m-%d").to_string());
    assert_eq!(output["count"], 2);
    assert_eq!(output["items"][0]["title"], "昨天事项");
    assert_eq!(output["items"][1]["title"], "今天事项");
}

#[tokio::test]
async fn list_tool_completed_date_range_uses_completed_at_not_planned_date() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let ctx = crate::util::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    let completed_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "昨天完成但计划较早".to_owned(),
                due_date: Some(before_range.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("昨天完成但计划较早")
            },
        )
        .unwrap();
    let planned_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "计划昨天但完成较早".to_owned(),
                due_date: Some(yesterday.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("计划昨天但完成较早")
            },
        )
        .unwrap();
    todo_store.complete(&owner, &completed_in_range.id).unwrap();
    todo_store.complete(&owner, &planned_in_range.id).unwrap();
    let mut items = todo_store.list_all(&owner).unwrap();
    for item in &mut items {
        if item.id == completed_in_range.id {
            item.completed_at = Some(format!("{}T10:00:00+08:00", yesterday.format("%Y-%m-%d")));
        } else if item.id == planned_in_range.id {
            item.completed_at = Some(format!(
                "{}T10:00:00+08:00",
                before_range.format("%Y-%m-%d")
            ));
        }
    }
    todo_store.set_items_for_test(&owner, &items).unwrap();

    let output = ListTodoTool::new(todo_store.clone(), session_store.clone())
        .execute(
            test_context(),
            json!({"status":"completed", "due_date": null, "date_range_text":"这两天"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["date_range_field"], "completed_at");
    assert_eq!(output["count"], 1);
    assert_eq!(output["items"][0]["title"], "昨天完成但计划较早");
}

#[tokio::test]
async fn list_tool_cancelled_date_range_uses_cancelled_at_not_planned_date() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let ctx = crate::util::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    let cancelled_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "昨天取消但计划较早".to_owned(),
                due_date: Some(before_range.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("昨天取消但计划较早")
            },
        )
        .unwrap();
    let planned_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "计划昨天但取消较早".to_owned(),
                due_date: Some(yesterday.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("计划昨天但取消较早")
            },
        )
        .unwrap();
    todo_store.cancel(&owner, &cancelled_in_range.id).unwrap();
    todo_store.cancel(&owner, &planned_in_range.id).unwrap();
    let mut items = todo_store.list_all(&owner).unwrap();
    for item in &mut items {
        if item.id == cancelled_in_range.id {
            item.cancelled_at = Some(format!("{}T10:00:00+08:00", yesterday.format("%Y-%m-%d")));
        } else if item.id == planned_in_range.id {
            item.cancelled_at = Some(format!(
                "{}T10:00:00+08:00",
                before_range.format("%Y-%m-%d")
            ));
        }
    }
    todo_store.set_items_for_test(&owner, &items).unwrap();

    let output = ListTodoTool::new(todo_store.clone(), session_store.clone())
        .execute(
            test_context(),
            json!({"status":"cancelled", "due_date": null, "date_range_text":"这两天"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["date_range_field"], "cancelled_at");
    assert_eq!(output["count"], 1);
    assert_eq!(output["items"][0]["title"], "昨天取消但计划较早");
}

#[tokio::test]
async fn list_tool_rejects_due_date_and_date_range_text_together() {
    let (todo_store, session_store, _notification_store, _owner) = test_stores();

    let err = ListTodoTool::new(todo_store.clone(), session_store.clone())
        .execute(
            test_context(),
            json!({"status":"pending", "due_date": "2026-07-01", "date_range_text":"本周"}),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_request");
    assert!(err.message.contains("不能同时传入"));
}

#[tokio::test]
async fn get_tool_uses_task_local_number_without_user_snapshot_pollution() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    todo_store
        .set_items_for_test(&owner, &tool_order_items())
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());
    let context = test_context();

    list_tool
        .execute(context.clone(), json!({"status":"all"}))
        .await
        .unwrap();
    let output = get_tool
        .execute(
            context,
            json!({"number": 1, "numbers": null, "selection_text": null, "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "明天事项");
    assert_eq!(output["item"]["visible_number"], 1);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(
        session.last_todo_query.is_none(),
        "get_todo 不应把 Agent 内部查询编号写成用户可见编号快照"
    );
}

#[tokio::test]
async fn get_tool_selection_text_reuses_single_selector() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    for title in ["第一条", "第二条"] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let output = get_tool
        .execute(
            context,
            json!({"number": null, "numbers": null, "selection_text": "第2条", "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "第二条");
    assert_eq!(output["item"]["visible_number"], 2);
}

#[tokio::test]
async fn get_tool_reference_last_uses_last_todo_action_without_writes() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "刚创建的事项".to_owned(),
                detail: Some("需要查详情".to_owned()),
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_action(&owner.key, &item, "created");
    session_store.save(&mut session).unwrap();
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());

    let output = get_tool
        .execute(
            test_context(),
            json!({"number": null, "numbers": null, "selection_text": null, "reference": "last"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "刚创建的事项");
    assert_eq!(output["item"]["reference"], "last");
    let saved = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(saved.pending_operation.is_none());
    assert!(saved.last_todo_query.is_none());
    assert_eq!(
        saved.last_todo_action.expect("missing last action").item_id,
        item.id
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn prepared_number_binding_survives_previous_completion() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "搬家".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let second = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "宽带迁移".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();

    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let complete_prepared = complete_tool
        .prepare(&context, json!({"numbers":[1], "reference": null}))
        .unwrap();
    let mut edit_context = context.clone();
    edit_context.tool_call_id = Some("call-2".to_owned());
    let edit_prepared = edit_tool
        .prepare(
            &edit_context,
            json!({
                "number": 2,
                "reference": null,
                "raw_text": "改为除了搬家还有宽带要迁移",
                "title": null,
                "detail": "除了搬家还有宽带要迁移",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap();

    complete_tool
        .execute(context.clone(), complete_prepared.arguments)
        .await
        .unwrap();
    let edited = edit_tool
        .execute(edit_context.clone(), edit_prepared.arguments)
        .await
        .unwrap();

    let edited_value = edited.value;
    assert_eq!(edited_value["ok"], true);
    assert_eq!(
        todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    let second_item = todo_store.get_by_id(&owner, &second.id).unwrap().unwrap();
    assert_eq!(
        second_item.detail.as_deref(),
        Some("除了搬家还有宽带要迁移")
    );
}

#[tokio::test]
async fn create_tool_replay_with_same_call_id_does_not_duplicate_created_todo() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "旧列表", vec!["999".to_owned()]);
    session_store.save(&mut session).unwrap();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    let arguments = json!({
        "content":"今晚检查机器人日志",
        "title":null,
        "detail":null,
        "due_date":null,
        "due_at":null,
        "reminder_at": null,
        "time_precision":null
    });

    let first = create_tool
        .execute(context.clone(), arguments.clone())
        .await
        .unwrap();
    let second = create_tool.execute(context, arguments).await.unwrap();

    assert_eq!(first.value, second.value);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert!(session.last_todo_query.is_none());
    let todos = todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].raw_text.as_deref(), Some("今晚检查机器人日志"));
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.item_id, todos[0].id);
    assert_eq!(last_action.action, "created");
}

#[tokio::test]
async fn create_tool_accepts_batch_at_contract_limit() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let output = create_tool
        .execute(
            test_context(),
            batch_create_arguments(TODO_TOOL_MAX_BATCH_CREATE_ITEMS),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true, "{output}");
    assert_eq!(
        output["created_items"].as_array().unwrap().len(),
        TODO_TOOL_MAX_BATCH_CREATE_ITEMS
    );
    assert_eq!(
        todo_store.list_pending(&owner).unwrap().len(),
        TODO_TOOL_MAX_BATCH_CREATE_ITEMS
    );
}

#[tokio::test]
async fn create_tool_rejects_empty_batch_without_writes() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let err = create_tool
        .execute(test_context(), batch_create_arguments(0))
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert!(err.message.contains("at least one"));
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn create_tool_rejects_batch_over_contract_limit_without_partial_writes() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let err = create_tool
        .execute(
            test_context(),
            batch_create_arguments(TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 1),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert!(err.message.contains("单次最多创建"));
    assert!(
        err.message
            .contains(&TODO_TOOL_MAX_BATCH_CREATE_ITEMS.to_string())
    );
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn create_tool_batch_limit_does_not_cap_existing_todo_total() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    for index in 0..(TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 3) {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: format!("已有事项 {index}"),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    assert!(todo_store.list_pending(&owner).unwrap().len() > TODO_TOOL_MAX_BATCH_CREATE_ITEMS);

    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );
    let output = create_tool
        .execute(test_context(), batch_create_arguments(2))
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["created_items"].as_array().unwrap().len(), 2);
    assert_eq!(
        todo_store.list_pending(&owner).unwrap().len(),
        TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 5
    );
}

#[tokio::test]
async fn same_task_query_numbers_prefer_current_list_over_stale_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let stale_visible = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "旧可见列表第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let current_completed = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "当前已完成第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.complete(&owner, &current_completed.id).unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(
        &owner.key,
        "pending",
        "旧列表",
        vec![stale_visible.id.clone()],
    );
    session_store.save(&mut session).unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let restore_tool = super::RestoreTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );
    let context = test_context();

    let listed = list_tool
        .execute(context.clone(), json!({"status":"completed"}))
        .await
        .unwrap()
        .value;
    assert_eq!(listed["items"][0]["visible_number"], 1);
    assert_eq!(listed["items"][0]["title"], "当前已完成第一条");

    let restored = restore_tool
        .execute(context, json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;

    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["title"], "当前已完成第一条");
    assert_eq!(
        todo_store
            .get_by_id(&owner, &current_completed.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &stale_visible.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn edit_tool_reuses_user_visible_snapshot_across_same_task_rounds() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut visible_ids = Vec::new();
    for title in ["第一条", "第二条", "第三条旧内容", "第四条旧内容"] {
        let item = todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
        visible_ids.push(item.id);
    }
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "进行中列表", visible_ids.clone());
    session_store.save(&mut session).unwrap();
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let mut first_context = test_context();
    first_context.tool_call_id = Some("edit-third".to_owned());
    let first_prepared = edit_tool
        .prepare(
            &first_context,
            json!({
                "number": 3,
                "reference": null,
                "raw_text": "第三条改成验收前检查",
                "title": "第三条新内容",
                "detail": "验收前检查",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap()
        .arguments;
    let first_output = edit_tool
        .execute(first_context, first_prepared)
        .await
        .unwrap()
        .value;
    assert_eq!(first_output["ok"], true);
    assert!(
        session_store
            .get_or_create_active(&SessionMeta::new(
                "private:u1",
                Some("u1".to_owned()),
                None,
                None,
                None,
                "qq_official",
            ))
            .unwrap()
            .last_todo_query
            .is_none()
    );

    let mut second_context = test_context();
    second_context.tool_call_id = Some("edit-fourth".to_owned());
    let second_prepared = edit_tool
        .prepare(
            &second_context,
            json!({
                "number": 4,
                "reference": null,
                "raw_text": "第四条改成计划验收",
                "title": "第四条新内容",
                "detail": "计划验收",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap()
        .arguments;
    let second_output = edit_tool
        .execute(second_context, second_prepared)
        .await
        .unwrap()
        .value;

    assert_eq!(second_output["ok"], true);
    let third = todo_store
        .get_by_id(&owner, &visible_ids[2])
        .unwrap()
        .expect("missing third todo");
    let fourth = todo_store
        .get_by_id(&owner, &visible_ids[3])
        .unwrap()
        .expect("missing fourth todo");
    assert_eq!(third.title, "第三条新内容");
    assert_eq!(third.detail.as_deref(), Some("验收前检查"));
    assert_eq!(fourth.title, "第四条新内容");
    assert_eq!(fourth.detail.as_deref(), Some("计划验收"));
}

#[tokio::test]
async fn create_then_edit_reference_last_updates_same_todo_without_pending() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-call".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "content":"明天搬家",
                "title":null,
                "detail":null,
                "due_date":null,
                "due_at":null,
                "reminder_at": null,
                "time_precision":null
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-call".to_owned());
    let updated = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "修改一下时间，中午搬家",
                "title": null,
                "detail": null,
                "due_date": "2026-07-03",
                "due_at": "2026-07-03 12:00:00",
                "time_precision": "date_time"
            }),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(updated["ok"], true);
    let todos = todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].due_at.as_deref(), Some("2026-07-03 12:00:00"));
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
    let last_action = session.last_todo_action.expect("missing last action");
    assert_eq!(last_action.item_id, todos[0].id);
    assert_eq!(last_action.action, "edited");
}

#[tokio::test]
async fn unresolved_last_reference_creates_todo_clarification_pending() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let complete_tool = CompleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = complete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": "last"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
    assert_eq!(output["pending_action"], "clarify");
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "complete_todos");
            assert_eq!(
                request.arguments,
                json!({"numbers": null, "reference": "last"})
            );
        }
        other => panic!("unexpected pending operation: {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_number_clarification_includes_pending_candidates_without_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中也能永久删除".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "delete_todos");
            assert_eq!(request.candidates.len(), 1);
            assert_eq!(request.candidates[0].id, item.id);
            assert_eq!(request.candidates[0].status, TodoStatus::Pending);
        }
        other => panic!("expected delete clarification pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_all_cancelled_creates_bulk_pending_without_deleting() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "和老公出门".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let second = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "买6号飞机票".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.cancel(&owner, &first.id).unwrap();
    todo_store.cancel(&owner, &second.id).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": null, "all_status": "cancelled"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["requires_confirmation"], true);
    assert_eq!(output["pending_action"], "delete");
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("准备永久删除 2 条已取消待办")
    );
    assert_eq!(todo_store.list_cancelled(&owner).unwrap().len(), 2);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete {
            item_ids,
            source_condition,
            status,
            ..
        }) => {
            assert_eq!(item_ids.len(), 2);
            assert_eq!(status, TodoStatus::Cancelled);
            assert_eq!(source_condition, "全部已取消待办");
        }
        other => panic!("expected bulk delete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_all_completed_zero_match_does_not_create_pending() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": null, "all_status": "completed"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn cancel_tool_selection_text_range_executes_batch_without_confirmation() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    for title in ["第一条", "第二条", "第三条"] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let cancel_tool = CancelTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let output = cancel_tool
        .execute(
            context,
            json!({"numbers": null, "selection_text": "1-3", "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["cancelled"].as_array().unwrap().len(), 3);
    assert!(output.get("requires_confirmation").is_none());
    assert!(output["missing_numbers"].as_array().unwrap().is_empty());
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
    assert_eq!(todo_store.list_cancelled(&owner).unwrap().len(), 3);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn cancel_tool_returns_failure_when_prepared_selection_no_longer_writes() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "会被并发删除的待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let cancel_tool = CancelTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();
    let prepared = cancel_tool
        .prepare(
            &context,
            json!({"numbers": [1], "selection_text": null, "reference": null}),
        )
        .unwrap();
    todo_store
        .delete_pending_by_ids(&owner, std::slice::from_ref(&item.id))
        .unwrap();

    let output = cancel_tool
        .execute(context, prepared.arguments)
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["error_code"], "todo_selection_not_found");
    assert!(output["cancelled"].as_array().unwrap().is_empty());
    assert_eq!(output["missing_numbers"], json!([1]));
    assert!(todo_store.list_cancelled(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn complete_tool_selection_text_discrete_deduplicates_numbers() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    for title in ["第一条", "第二条", "第三条"] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let output = complete_tool
        .execute(
            context,
            json!({"numbers": null, "selection_text": "1,3,3", "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["completed"].as_array().unwrap().len(), 2);
    assert_eq!(todo_store.list_completed(&owner).unwrap().len(), 2);
    assert_eq!(todo_store.list_pending(&owner).unwrap().len(), 1);
}

#[tokio::test]
async fn delete_tool_query_unique_creates_single_delete_pending() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "和老公出门".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.cancel(&owner, &item.id).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "和老公出门", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert!(
        output["message"]
            .as_str()
            .unwrap()
            .contains("准备永久删除待办：和老公出门")
    );
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.title, "和老公出门");
            assert_eq!(item.status, TodoStatus::Cancelled);
        }
        other => panic!("expected single delete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_query_multiple_creates_clarification_without_snapshot_pollution() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let visible = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "用户可见第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "飞机票 6号".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let second = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "买6号飞机票".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.complete(&owner, &first.id).unwrap();
    todo_store.cancel(&owner, &second.id).unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "all", "全部待办", vec![visible.id.clone()]);
    session_store.save(&mut session).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "飞机票", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert_eq!(
        session.last_todo_query.unwrap().result_ids,
        vec![visible.id]
    );
    match session.pending_operation {
        Some(PendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "delete_todos");
            assert_eq!(request.candidates.len(), 2);
        }
        other => panic!("expected clarification pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_query_pending_match_creates_confirmation() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "还没做不能永久删".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "不能永久删", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["requires_confirmation"], true);
    assert_eq!(output["pending_action"], "delete");
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(item_ids.len(), 1);
            assert_eq!(status, TodoStatus::Pending);
        }
        other => panic!("expected pending bulk delete operation, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_numbers_prefer_current_task_query_over_stale_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let pending = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let cancelled_a = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "内部已取消第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let cancelled_b = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "和老公出门".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.cancel(&owner, &cancelled_a.id).unwrap();
    todo_store.cancel(&owner, &cancelled_b.id).unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(
        &owner.key,
        "all",
        "全部待办",
        vec![
            pending.id.clone(),
            cancelled_a.id.clone(),
            cancelled_b.id.clone(),
        ],
    );
    session_store.save(&mut session).unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    list_tool
        .execute(test_context(), json!({"status":"cancelled"}))
        .await
        .unwrap();
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.status, TodoStatus::Cancelled)
        }
        other => panic!("expected single delete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_numbers_prefer_quoted_snapshot_over_latest_last_todo_query() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut list_a_ids = Vec::new();
    let mut list_b_ids = Vec::new();
    for index in 1..=7 {
        let item = todo_store
            .create(&owner, tool_test_draft(&format!("列表 A 第 {index} 条")))
            .unwrap();
        list_a_ids.push(item.id);
    }
    for index in 1..=7 {
        let item = todo_store
            .create(&owner, tool_test_draft(&format!("列表 B 第 {index} 条")))
            .unwrap();
        list_b_ids.push(item.id);
    }
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "列表 B", list_b_ids.clone());
    session_store.save(&mut session).unwrap();

    let delete_tool = DeleteTodoTool::new(todo_store, session_store.clone(), notification_store)
        .with_selection_scope(SelectionScope::Scoped(Arc::from(list_a_ids.clone())));
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [7], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete { item_ids, .. }) => {
            assert_eq!(item_ids, vec![list_a_ids[6].clone()]);
            assert_ne!(item_ids, vec![list_b_ids[6].clone()]);
        }
        other => panic!("expected bulk delete pending from quoted snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn blocked_quoted_snapshot_does_not_fallback_to_last_todo_query() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let fallback = todo_store
        .create(&owner, tool_test_draft("不应被 fallback 删除"))
        .unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "列表 B", vec![fallback.id]);
    session_store.save(&mut session).unwrap();

    let delete_tool = DeleteTodoTool::new(todo_store, session_store.clone(), notification_store)
        .with_selection_scope(SelectionScope::Blocked);
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn cancel_numbers_use_quoted_snapshot_and_do_not_delete() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut list_a_ids = Vec::new();
    let mut list_b_ids = Vec::new();
    for index in 1..=7 {
        list_a_ids.push(
            todo_store
                .create(
                    &owner,
                    tool_test_draft(&format!("取消列表 A 第 {index} 条")),
                )
                .unwrap()
                .id,
        );
        list_b_ids.push(
            todo_store
                .create(
                    &owner,
                    tool_test_draft(&format!("取消列表 B 第 {index} 条")),
                )
                .unwrap()
                .id,
        );
    }
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "列表 B", list_b_ids.clone());
    session_store.save(&mut session).unwrap();

    let cancel_tool = CancelTodoTool::new(todo_store.clone(), session_store, notification_store)
        .with_selection_scope(SelectionScope::Scoped(Arc::from(list_a_ids.clone())));
    let output = cancel_tool
        .execute(
            test_context(),
            json!({"numbers": [7], "selection_text": null, "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["cancelled"].as_array().unwrap().len(), 1);
    let cancelled = todo_store.list_cancelled(&owner).unwrap();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(cancelled[0].id, list_a_ids[6]);
    assert_eq!(
        todo_store
            .get_by_id(&owner, &list_b_ids[6])
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn merge_numbers_use_quoted_snapshot_and_physically_delete_source() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut list_a_ids = Vec::new();
    let mut list_b_ids = Vec::new();
    for index in 1..=7 {
        let mut draft = tool_test_draft(&format!("合并列表 A 第 {index} 条"));
        draft.detail = Some(format!("A detail {index}"));
        list_a_ids.push(todo_store.create(&owner, draft).unwrap().id);
        list_b_ids.push(
            todo_store
                .create(
                    &owner,
                    tool_test_draft(&format!("合并列表 B 第 {index} 条")),
                )
                .unwrap()
                .id,
        );
    }
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "列表 B", list_b_ids.clone());
    session_store.save(&mut session).unwrap();

    let merge_tool = MergeTodoTool::new(todo_store.clone(), session_store, notification_store)
        .with_selection_scope(SelectionScope::Scoped(Arc::from(list_a_ids.clone())));
    let output = merge_tool
        .execute(
            test_context(),
            json!({"source_number": 7, "target_number": 6}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    let target = todo_store
        .get_by_id(&owner, &list_a_ids[5])
        .unwrap()
        .unwrap();
    assert!(target.detail.unwrap_or_default().contains("A detail 7"));
    assert!(
        todo_store
            .get_by_id(&owner, &list_a_ids[6])
            .unwrap()
            .is_none()
    );
    assert!(
        todo_store
            .list_cancelled(&owner)
            .unwrap()
            .iter()
            .all(|item| item.id != list_a_ids[6])
    );
    assert!(
        todo_store
            .get_by_id(&owner, &list_b_ids[6])
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn delete_tool_rejects_mixed_status_bulk_selection_without_pending() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let pending = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中目标".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let completed = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成目标".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store.complete(&owner, &completed.id).unwrap();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(
        &owner.key,
        "all",
        "全部待办",
        vec![pending.id.clone(), completed.id.clone()],
    );
    session_store.save(&mut session).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1, 2], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["error_code"], "todo_delete_mixed_status");
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(
        todo_store
            .get_by_id(&owner, &pending.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &completed.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}

#[tokio::test]
async fn create_tool_with_reminder_writes_notification_outbox() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let output = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "明天提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(
        output["created"]["due_at"].as_str(),
        Some("2099-01-01 09:30")
    );
    assert_eq!(
        output["created"]["reminder_at"].as_str(),
        Some("2099-01-01 09:30")
    );
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_type, "todo");
    assert_eq!(tasks[0].kind, "todo_reminder");
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Pending
    );
    assert_eq!(tasks[0].scheduled_at, "2099-01-01T09:30:00+08:00");
    assert!(
        tasks[0].payload["text"]
            .as_str()
            .unwrap()
            .contains("待办提醒")
    );
    assert!(
        tasks[0].payload["fallback_text"]
            .as_str()
            .unwrap()
            .starts_with("⏰ 待办提醒")
    );
    assert!(
        tasks[0].payload["text"]
            .as_str()
            .unwrap()
            .contains("检查日志")
    );
}

#[tokio::test]
async fn create_tool_rejects_ambiguous_recurrence_phrase() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    let err = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每隔几天提醒我复盘",
                "title": "复盘",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code.as_str(), "bad_request");
    assert!(err.message.contains("每隔 3 天"));
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn create_tool_explicit_none_skips_recurrence_inference_from_content() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "明天提醒我：不要每天喝奶茶",
                "title": "不要每天喝奶茶",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "none",
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();

    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::todo::TodoRecurrenceKind::None
    );
    assert_eq!(todo.recurrence_interval_days, 0);
    assert_eq!(todo.recurrence_interval, 0);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::todo::TodoRecurrenceUnit::Day
    );
}

#[tokio::test]
async fn edit_tool_explicit_none_skips_recurrence_inference_from_raw_text() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-recurring".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("clear-recurrence".to_owned());
    edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "不要每天提醒了，保留这次提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": "none",
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();

    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::todo::TodoRecurrenceKind::None
    );
    assert_eq!(todo.recurrence_interval_days, 0);
    assert_eq!(todo.recurrence_interval, 0);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::todo::TodoRecurrenceUnit::Day
    );
    assert_eq!(todo.reminder_at.as_deref(), Some("2099-01-01 09:30"));
}

#[tokio::test]
async fn edit_tool_sets_weekly_monthly_yearly_unit_when_only_kind_is_provided() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    for (index, kind, expected_unit) in [
        (
            "weekly",
            "weekly",
            crate::runtime::todo::TodoRecurrenceUnit::Week,
        ),
        (
            "monthly",
            "monthly",
            crate::runtime::todo::TodoRecurrenceUnit::Month,
        ),
        (
            "yearly",
            "yearly",
            crate::runtime::todo::TodoRecurrenceUnit::Year,
        ),
    ] {
        let mut create_context = test_context();
        create_context.tool_call_id = Some(format!("create-{index}"));
        create_tool
            .execute(
                create_context,
                json!({
                    "items": null,
                    "content": format!("提醒我做 {index} 检查"),
                    "title": format!("{index} 检查"),
                    "detail": null,
                    "due_date": null,
                    "due_at": null,
                    "reminder_at": "2099-01-01 09:30",
                    "time_precision": null,
                    "recurrence_kind": "none",
                    "recurrence_interval": null,
                    "recurrence_unit": null,
                    "recurrence_interval_days": null
                }),
            )
            .await
            .unwrap();

        let mut edit_context = test_context();
        edit_context.tool_call_id = Some(format!("edit-{index}"));
        edit_tool
            .execute(
                edit_context,
                json!({
                    "number": null,
                    "reference": "last",
                    "raw_text": format!("改成 {kind} 重复"),
                    "title": null,
                    "detail": null,
                    "due_date": null,
                    "due_at": null,
                    "reminder_at": null,
                    "time_precision": null,
                    "recurrence_kind": kind,
                    "recurrence_interval": null,
                    "recurrence_unit": null,
                    "recurrence_interval_days": null
                }),
            )
            .await
            .unwrap();

        let updated = todo_store
            .list_pending(&owner)
            .unwrap()
            .into_iter()
            .find(|item| item.title == format!("{index} 检查"))
            .unwrap();
        assert_eq!(updated.recurrence_interval, 1, "{kind}");
        assert_eq!(updated.recurrence_unit, expected_unit, "{kind}");
        assert_eq!(updated.recurrence_interval_days, 0, "{kind}");
    }
}

#[tokio::test]
async fn edit_tool_reschedules_pending_reminder_cancels_old_outbox_task() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-pending-reminder".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-pending-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "改到后天上午九点半提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-02 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();
    let old_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-01T09:30:00+08:00")
        .unwrap();
    let new_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-02T09:30:00+08:00")
        .unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        old_task.status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
    assert_eq!(
        new_task.status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn edit_tool_reschedules_sent_reminder_with_new_outbox_task() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-reminder".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();
    let first_task = notification_store.list_all_for_test().unwrap()[0].clone();
    notification_store.mark_sent(first_task.id).unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "改到后天上午九点半提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-02 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();
    let new_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-02T09:30:00+08:00")
        .unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Sent
    );
    assert_eq!(
        new_task.status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn edit_tool_allows_unrelated_edit_when_existing_reminder_is_past() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: Some("2020-01-01 09:30:00".to_owned()),
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    list_tool
        .execute(test_context(), json!({"status": "pending"}))
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-title-with-past-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": 1,
                "reference": null,
                "raw_text": "标题改成检查网关日志",
                "title": "检查网关日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(updated.title, "检查网关日志");
    assert_eq!(notification_store.list_all_for_test().unwrap().len(), 0);
}

#[tokio::test]
async fn complete_tool_cancels_pending_reminder_task() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "待办列表", vec![todo.id.clone()]);
    session_store.save(&mut session).unwrap();
    let complete_tool =
        CompleteTodoTool::new(todo_store, session_store, notification_store.clone());

    complete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "selection_text": null, "reference": null}),
        )
        .await
        .unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
}

#[tokio::test]
async fn complete_tool_advances_recurring_todo_and_reschedules_reminder() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
            }),
        )
        .await
        .unwrap();
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "待办列表", vec![todo.id.clone()]);
    session_store.save(&mut session).unwrap();

    let output = complete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "selection_text": null, "reference": null}),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["completed"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["advanced"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        output["advanced"][0]["reminder_at"].as_str(),
        Some("2099-01-02 09:30")
    );
    assert_eq!(
        output["advanced"][0]["next_reminder_at"].as_str(),
        Some("2099-01-03 09:30")
    );
    assert_eq!(updated.status, TodoStatus::Pending);
    assert_eq!(updated.reminder_at.as_deref(), Some("2099-01-02 09:30"));
    assert_eq!(updated.due_at.as_deref(), Some("2099-01-02 09:30"));
    assert_eq!(
        updated.recurrence_kind,
        crate::runtime::todo::TodoRecurrenceKind::Daily
    );
    assert_eq!(updated.recurrence_interval_days, 1);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
    assert_eq!(
        tasks[1].status,
        crate::storage::notification::NotificationStatus::Pending
    );
    assert_eq!(tasks[1].scheduled_at, "2099-01-02T09:30:00+08:00");
}
