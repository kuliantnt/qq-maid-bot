//! Todo Tool outcome 的 Respond 编排测试。

use super::*;

#[tokio::test]
async fn todo_tool_ok_false_without_error_code_is_failed_outcome() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "edit_todo",
                serde_json::json!({
                    "ok": false,
                    "message": "没有成功修改待办"
                }),
                false,
            )],
            "已修改待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("把第一条待办改成新标题"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_tool_error"));
    let text = response.text.unwrap();
    assert!(text.contains("没有成功修改待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["error_code"], Value::Null);
    assert_eq!(diagnostics["todo_success_verified"], false);
}

#[tokio::test]
async fn todo_clarification_is_not_marked_as_write_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "complete_todos",
                serde_json::json!({
                    "ok": false,
                    "requires_clarification": true,
                    "question": "请说明要完成哪条待办。"
                }),
                false,
            )],
            "已完成待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(private_message("完成待办")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    let text = response.text.unwrap();
    assert!(text.contains("请说明要完成哪条待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "requires_clarification");
    assert_eq!(
        diagnostics["tool_outcomes"][0]["status"],
        "requires_clarification"
    );
    assert_eq!(diagnostics["todo_success_verified"], false);
}

#[tokio::test]
async fn todo_business_failure_keeps_root_error_before_dependency_skip() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "delete_todos",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "todo_selection_not_found",
                        "message": "没有找到符合条件的待办"
                    }),
                    false,
                ),
                raw_tool_result(
                    "complete_todos",
                    serde_json::json!({
                        "ok": false,
                        "skipped": true,
                        "reason": "dependency_previous_call_failed"
                    }),
                    false,
                ),
            ],
            "已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("删除第一条再完成第二条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_tool_error"));
    let text = response.text.unwrap();
    assert!(text.contains("没有找到符合条件的待办"));
    assert!(text.contains("前序工具没有成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["error_code"], "todo_selection_not_found");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][1]["status"], "skipped");
}

#[tokio::test]
async fn todo_success_then_failure_is_partial_success_and_keeps_database_change() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "create_todo",
                    r#"{"content":"新增后保留","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
                (
                    "edit_todo",
                    r#"{"number":99,"reference":null,"raw_text":"不应成功","title":"不应成功","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("新增一个待办再编辑不存在的待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("新增后保留"));
    assert!(text.contains("我现在没有可用的待办列表编号"));
    assert!(text.contains("可选待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(
        diagnostics["tool_outcomes"][1]["status"],
        "requires_clarification"
    );
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "新增后保留");
}

#[tokio::test]
async fn multiple_successful_todo_writes_share_one_background_snapshot() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "create_todo",
                    r#"{"content":"第一条新增","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
                (
                    "create_todo",
                    r#"{"content":"第二条新增","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "已新增最后一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("新增两条待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("✅ 已新增待办").count(), 2);
    assert_eq!(text.matches("🚧 当前进行中").count(), 0);
    assert!(text.contains("第一条新增"));
    assert!(text.contains("第二条新增"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 2);
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 2);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing background refreshed snapshot");
    assert_eq!(snapshot.result_ids.len(), 2);
}

#[tokio::test]
async fn only_list_todos_success_does_not_claim_todo_write_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", r#"{"status":"pending"}"#, "当前待办列表");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "只读查询不算写入".to_owned(),
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
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("检查待办状态"))
        .await
        .unwrap();

    let visible_snapshot = response
        .visible_entity_snapshot
        .as_ref()
        .expect("visible list response should carry snapshot");
    assert_eq!(visible_snapshot.items.len(), 1);
    assert_eq!(visible_snapshot.items[0].visible_number, 1);
    assert_eq!(visible_snapshot.items[0].domain, "todo");

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][0]["effect"], "read_only");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
}
