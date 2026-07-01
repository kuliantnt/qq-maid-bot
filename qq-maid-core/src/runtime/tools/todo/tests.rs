// 拆分后这些不再随 `super::*` 自动进入命名空间，测试体里仍直接引用完整类型/宏。
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext};

use crate::runtime::pending::PendingOperation;
use crate::runtime::session::{SessionMeta, SessionStore};
use crate::runtime::todo::{
    TodoItem, TodoItemDraft, TodoOwner, TodoStatus, TodoStore, TodoTimePrecision,
};

use super::{CompleteTodoTool, CreateTodoTool, EditTodoTool, ListTodoTool};
use crate::storage::{APP_MIGRATIONS, database::SqliteDatabase};

fn test_context() -> ToolContext {
    ToolContext {
        task_id: "msg-1".to_owned(),
        user_id: Some("u1".to_owned()),
        scope_id: "private:u1".to_owned(),
        tool_call_id: Some("call-1".to_owned()),
    }
}

fn test_stores() -> (TodoStore, SessionStore, TodoOwner) {
    let database = SqliteDatabase::open_temp("todo-tool-tests", APP_MIGRATIONS).unwrap();
    let todo_store = TodoStore::new(database.clone());
    let session_store = SessionStore::new(database);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    (todo_store, session_store, owner)
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
            time_precision: TodoTimePrecision::None,
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
            time_precision: TodoTimePrecision::Date,
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
            time_precision: TodoTimePrecision::Date,
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
            time_precision: TodoTimePrecision::None,
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
            time_precision: TodoTimePrecision::None,
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
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T13:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T13:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T13:10:00+08:00".to_owned()),
        },
    ]
}

#[tokio::test]
async fn list_tool_all_uses_board_order_for_visible_numbers() {
    let (todo_store, session_store, owner) = test_stores();
    todo_store
        .set_items_for_test(&owner, &tool_order_items())
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store, session_store.clone());

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
    let snapshot = session.last_todo_query.expect("missing todo snapshot");
    assert_eq!(snapshot.query_type, "all");
    assert_eq!(
        snapshot.result_ids,
        vec!["3", "2", "1", "5", "4", "6"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn prepared_number_binding_survives_previous_completion() {
    let (todo_store, session_store, owner) = test_stores();
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "搬家".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
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
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(todo_store.clone(), session_store.clone());
    let edit_tool = EditTodoTool::new(todo_store.clone(), session_store.clone());
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
async fn create_tool_replay_with_same_call_id_does_not_duplicate_pending() {
    let (_todo_store, session_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(session_store.clone());
    let context = test_context();
    let arguments = json!({
        "content":"今晚检查机器人日志",
        "title":null,
        "detail":null,
        "due_date":null,
        "due_at":null,
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
    assert!(matches!(
        session.pending_operation,
        Some(PendingOperation::TodoAdd { .. })
    ));
}
