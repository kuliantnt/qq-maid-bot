use super::support::*;
use super::*;

#[tokio::test]
async fn create_tool_accepts_stable_private_scope_context() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let stable_scope = "platform:qq_official:account:app-1:private:u1";
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store,
    );
    let mut context = test_context();
    context.conversation.scope_id = stable_scope.to_owned();
    context.conversation.interaction_scope_id = stable_scope.to_owned();
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

#[tokio::test]
async fn create_tool_places_daypart_in_time_fields_when_model_keeps_raw_content() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store,
    );
    let arguments = json!({
        "content":"下午检查发布清单",
        "title":"检查发布清单",
        "detail":null,
        "due_date":null,
        "due_at":null,
        "reminder_at": null,
        "time_precision":null
    });

    create_tool
        .execute(test_context(), arguments)
        .await
        .unwrap();

    let expected_due_at = format!(
        "{} 15:00:00",
        qq_maid_common::time_context::request_time_context().current_date()
    );
    let todos = todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "检查发布清单");
    assert_eq!(todos[0].due_at.as_deref(), Some(expected_due_at.as_str()));
    assert_eq!(todos[0].time_precision, TodoTimePrecision::DateTime);
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
    let create_tool = create_batch_tool(todo_store.clone(), session_store, notification_store);

    let output = execute_batch_create(&create_tool, TODO_TOOL_MAX_BATCH_CREATE_ITEMS)
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true, "{output}");
    assert_eq!(
        output["created_items"].as_array().unwrap().len(),
        TODO_TOOL_MAX_BATCH_CREATE_ITEMS
    );
    assert_pending_todo_count(&todo_store, &owner, TODO_TOOL_MAX_BATCH_CREATE_ITEMS);
}

#[tokio::test]
async fn create_tool_rejects_empty_batch_without_writes() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = create_batch_tool(todo_store.clone(), session_store, notification_store);

    let err = execute_batch_create(&create_tool, 0).await.unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert!(err.message.contains("at least one"));
    assert_pending_todo_count(&todo_store, &owner, 0);
}

#[tokio::test]
async fn create_tool_rejects_batch_over_contract_limit_without_partial_writes() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = create_batch_tool(todo_store.clone(), session_store, notification_store);

    let err = execute_batch_create(&create_tool, TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 1)
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert!(err.message.contains("单次最多创建"));
    assert!(
        err.message
            .contains(&TODO_TOOL_MAX_BATCH_CREATE_ITEMS.to_string())
    );
    assert_pending_todo_count(&todo_store, &owner, 0);
}

#[tokio::test]
async fn create_tool_batch_limit_does_not_cap_existing_todo_total() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    for index in 0..(TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 3) {
        todo_store
            .create(&owner, tool_test_draft(&format!("已有事项 {index}")))
            .unwrap();
    }
    assert!(todo_store.list_pending(&owner).unwrap().len() > TODO_TOOL_MAX_BATCH_CREATE_ITEMS);

    let create_tool = create_batch_tool(todo_store.clone(), session_store, notification_store);
    let output = execute_batch_create(&create_tool, 2).await.unwrap().value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["created_items"].as_array().unwrap().len(), 2);
    assert_pending_todo_count(&todo_store, &owner, TODO_TOOL_MAX_BATCH_CREATE_ITEMS + 5);
}
