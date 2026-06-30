use super::*;
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
