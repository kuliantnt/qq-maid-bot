use super::support::*;
use super::*;

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
        assert_nullable_type(&schema, "numbers", "array", tool_name);
        assert_schema_max_items(&schema, "numbers", TODO_TOOL_MAX_NUMBERS, tool_name);
        assert_nullable_type(&schema, "selection_text", "string", tool_name);
        assert_nullable_type(&schema, "reference", "string", tool_name);
    }

    let edit_schema = EditTodoTool::new(todo_store, session_store, notification_store.clone())
        .metadata()
        .parameters;
    assert_nullable_type(&edit_schema, "number", "integer", "edit_todo");
    assert_nullable_type(&edit_schema, "reference", "string", "edit_todo");
    assert!(
        edit_schema["properties"]["detail"]["description"]
            .as_str()
            .unwrap()
            .contains("清除详情")
    );
    assert!(
        edit_schema["properties"]["detail"]["description"]
            .as_str()
            .unwrap()
            .contains("空字符串")
    );
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
    assert_schema_max_items(
        &schema,
        "items",
        TODO_TOOL_MAX_BATCH_CREATE_ITEMS,
        "create_todo",
    );
}

#[test]
fn restore_todo_schema_describes_natural_language_undo_paths() {
    let (todo_store, session_store, notification_store, _) = test_stores();
    let metadata = RestoreTodoTool::new(todo_store, session_store, notification_store).metadata();

    for marker in [
        "撤销完成",
        "刚才那条还没做完",
        "第一条改回未完成",
        "list_todos",
        "无法唯一确定目标时必须追问",
    ] {
        assert!(metadata.description.contains(marker), "{marker}");
    }
    assert!(metadata.description.contains("不会接受数据库内部 ID"));
}
