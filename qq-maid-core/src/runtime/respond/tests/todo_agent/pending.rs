//! Todo 删除 Pending、用户确认和会话交接的 Respond 集成测试。

use qq_maid_llm::provider::ToolCallingProtocol;

use crate::runtime::tools::todo::{
    TodoItemDraft, TodoPendingOperation, TodoStatus, TodoStore, TodoTimePrecision,
};

use super::super::support::*;

#[tokio::test]
async fn todo_delete_completed_item_accepts_delete_tool_pending_result() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已发起永久删除确认",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成可永久删除".to_owned(),
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
    service.task_store.complete(&owner, &todo.id).unwrap();

    service
        .respond(private_message("看看已完成"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("删除第一条"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.title, "已完成可永久删除");
            assert_eq!(item.status, TodoStatus::Completed);
        }
        other => panic!("expected TodoDelete pending operation, got {other:?}"),
    }
}

#[tokio::test]
async fn todo_delete_completed_pending_confirmation_is_verified_by_real_tool_result() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已发起删除已完成待办确认",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待确认永久删除".to_owned(),
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
    service.task_store.complete(&owner, &todo.id).unwrap();
    service
        .respond(private_message("查看已完成待办"))
        .await
        .unwrap();

    let response = service
        .respond(private_message("删除第一条已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("确认删除以下 1 项待办吗"));
    assert!(text.contains("删除后不可恢复"));
    assert!(!text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["delete_todos"])
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(matches!(
        todo_pending(session.pending_operation.as_ref()),
        Some(TodoPendingOperation::TodoDelete { .. })
    ));
}
