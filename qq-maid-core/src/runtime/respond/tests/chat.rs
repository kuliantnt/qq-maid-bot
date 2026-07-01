use std::fs;

use serde_json::Value;

use crate::provider::{ToolCallingProtocol, types::ChatRole};

use super::{
    super::{
        RespondRequest,
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT,
            SESSION_STATE_SHORT_TEXT_LIMIT, empty_respond_request,
        },
    },
    support::*,
};
use crate::runtime::session::SessionMeta;
use crate::runtime::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};
use crate::runtime::{
    memory::{CreateScopedMemoryRequest, MemoryScopeType},
    pending::PendingOperation,
};

#[tokio::test]
async fn chat_writes_history_and_uses_prompt_files() {
    let service = test_service();

    let response = service
        .respond(private_message("我是407，继续"))
        .await
        .unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    assert_eq!(response.markdown.as_deref(), Some("回复：我是407，继续"));
    assert_eq!(response.diagnostics.unwrap()["backend"], "rust");
}

#[tokio::test]
async fn chat_returns_markdown_and_plaintext_fallback_for_structured_reply() {
    let response = test_service().respond(message("给 codex")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("标题\n· hello"));
    assert_eq!(response.markdown.as_deref(), Some("# 标题\n- hello"));
}

#[tokio::test]
async fn private_chat_with_openai_responses_capability_enters_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("工具回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    let mut tool_requests = inspector.tool_requests();
    let tool_request = tool_requests.remove(0);
    assert_eq!(tool_request.tool_context.user_id.as_deref(), Some("u1"));
    assert_eq!(tool_request.tool_context.scope_id, "private:u1");
    assert!(!tool_request.tool_context.task_id.trim().is_empty());
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(true)
    );
}

#[tokio::test]
async fn private_general_chat_with_tool_capability_uses_tool_loop_without_tool_call() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("聊聊 Rust 的所有权"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("工具回复：聊聊 Rust 的所有权")
    );
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_calling_enabled"], serde_json::json!(true));
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
}

#[tokio::test]
async fn private_tool_loop_registers_todo_tools_and_keeps_internal_ids_hidden() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查机器人日志".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let list_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    let listed: Value = serde_json::from_str(&list_output).unwrap();
    assert_eq!(listed["items"][0]["visible_number"], 1);
    assert!(listed["items"][0].get("id").is_none());

    let complete_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&complete_output).unwrap();
    assert_eq!(completed["completed"][0]["title"], "检查机器人日志");
    assert!(completed["completed"][0].get("id").is_none());
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Completed
    );
    let list_completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"completed"}"#,
        )
        .await
        .unwrap();
    let listed_completed: Value = serde_json::from_str(&list_completed_output).unwrap();
    assert_eq!(listed_completed["items"][0]["visible_number"], 1);
    let restore_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let restored: Value = serde_json::from_str(&restore_output).unwrap();
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["visible_number"], 1);
    assert!(restored["missing_numbers"].as_array().unwrap().is_empty());

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.last_todo_query.is_none());
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.owner_key, owner.key);
    assert_eq!(last_action.title, "检查机器人日志");
    assert_eq!(last_action.action, "restored");
    assert_eq!(last_action.resulting_status, TodoStatus::Pending);
}

#[tokio::test]
async fn todo_tools_create_cancel_restore_and_delete_use_existing_pending_boundaries() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    service
        .respond(private_message("帮我记待办"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let create_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "create_todo",
            r#"{"content":"今晚检查机器人日志","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
        )
        .await
        .unwrap();
    let created: Value = serde_json::from_str(&create_output).unwrap();
    assert_eq!(created["requires_confirmation"], true);
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
    let duplicate_pending = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "create_todo",
            r#"{"content":"明天交话费","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate_pending.code, "pending_operation_exists");

    service.respond(private_message("确认")).await.unwrap();
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);

    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    let cancel_output = tool_request
        .tools
        .execute_json(&tool_request.tool_context, "cancel_todo", r#"{"number":1}"#)
        .await
        .unwrap();
    let cancel: Value = serde_json::from_str(&cancel_output).unwrap();
    assert_eq!(cancel["requires_confirmation"], true);
    assert_eq!(cancel["pending_action"], "cancel");
    service.respond(private_message("确认")).await.unwrap();
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Cancelled
    );

    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"cancelled"}"#,
        )
        .await
        .unwrap();
    let restore_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let restore: Value = serde_json::from_str(&restore_output).unwrap();
    assert_eq!(restore["restored"][0]["visible_number"], 1);
    assert!(restore["missing_numbers"].as_array().unwrap().is_empty());
    let restored = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].cancelled_at.is_none());

    service
        .todo_store
        .complete(&owner, &restored[0].id)
        .unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"completed"}"#,
        )
        .await
        .unwrap();
    let delete_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "delete_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let delete: Value = serde_json::from_str(&delete_output).unwrap();
    assert_eq!(delete["requires_confirmation"], true);
    assert_eq!(delete["pending_action"], "delete");
    service.respond(private_message("确认")).await.unwrap();
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn restore_then_cancel_last_reference_creates_pending_without_relisting() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "恢复后继续取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();

    service
        .respond(private_message("看看已完成"))
        .await
        .unwrap();
    assert_eq!(inspector.tool_call_count(), 0);
    let _ = service.respond(private_message("恢复第 1 个")).await;
    let mut tool_requests = inspector.tool_requests();
    let tool_request = tool_requests.pop().unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let cancel_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "cancel_todo",
            r#"{"number":null,"reference":"last"}"#,
        )
        .await
        .unwrap();
    let cancel: Value = serde_json::from_str(&cancel_output).unwrap();

    assert_eq!(cancel["ok"], true);
    assert_eq!(cancel["requires_confirmation"], true);
    assert_eq!(cancel["pending_action"], "cancel");
    assert_eq!(cancel["item"]["reference"], "last");

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.title, "恢复后继续取消");
            assert_eq!(item.status, TodoStatus::Pending);
        }
        other => panic!("expected TodoDelete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn restore_then_reuse_stale_number_keeps_visible_number_error() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "旧编号不能偷偷映射最近对象".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();

    service
        .respond(private_message("看看已完成"))
        .await
        .unwrap();
    assert_eq!(inspector.tool_call_count(), 0);
    let _ = service.respond(private_message("恢复第 1 个")).await;
    let mut tool_requests = inspector.tool_requests();
    let tool_request = tool_requests.pop().unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let cancel_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "cancel_todo",
            r#"{"number":1,"reference":null}"#,
        )
        .await
        .unwrap();
    let cancel: Value = serde_json::from_str(&cancel_output).unwrap();

    assert_eq!(cancel["ok"], false);
    assert_eq!(cancel["error_code"], "todo_visible_numbers_unavailable");
}

#[tokio::test]
async fn complete_multiple_items_clears_last_todo_action() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for title in ["批量一", "批量二"] {
        service
            .todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
    }

    service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1,2],"reference":null}"#,
        )
        .await
        .unwrap();

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.last_todo_action.is_none());
}

#[tokio::test]
async fn last_reference_rejects_owner_mismatch_and_missing_todo() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "最近对象失效".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();

    let mut session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    session.last_todo_action.as_mut().unwrap().owner_key = "other-user".to_owned();
    service.session_store.save(&mut session).unwrap();

    let owner_mismatch = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "cancel_todo",
            r#"{"number":null,"reference":"last"}"#,
        )
        .await
        .unwrap();
    let owner_mismatch: Value = serde_json::from_str(&owner_mismatch).unwrap();
    assert_eq!(owner_mismatch["ok"], false);
    assert_eq!(owner_mismatch["error_code"], "todo_reference_unavailable");
    assert_eq!(owner_mismatch["requires_clarification"], true);

    let mut session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    session.pending_operation = None;
    session.last_todo_action.as_mut().unwrap().owner_key = owner.key.clone();
    service.session_store.save(&mut session).unwrap();
    service
        .todo_store
        .delete_completed_by_ids(&owner, std::slice::from_ref(&todo.id))
        .unwrap();

    let missing = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "cancel_todo",
            r#"{"number":null,"reference":"last"}"#,
        )
        .await
        .unwrap();
    let missing: Value = serde_json::from_str(&missing).unwrap();
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["error_code"], "todo_reference_unavailable");
    assert_eq!(missing["requires_clarification"], true);

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(matches!(
        session.pending_operation,
        Some(crate::runtime::pending::PendingOperation::TodoClarify { .. })
    ));
}

#[tokio::test]
async fn tool_loop_created_todo_pending_survives_chat_history_save_and_confirm_skips_llm() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_create_todo_tool_call("今晚检查机器人日志");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let first = service
        .respond(private_message("帮我记一个待办，今晚检查机器人日志"))
        .await
        .unwrap();
    assert!(first.text.unwrap().contains("工具回复"));
    let first_diagnostics = first.diagnostics.unwrap();
    assert_eq!(first_diagnostics["todo_success_claimed"], false);
    assert_eq!(first_diagnostics["todo_success_verified"], true);
    assert_eq!(first_diagnostics["tool_retry_count"], 0);
    assert_eq!(
        first_diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["create_todo"])
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoAdd { draft, .. }) => {
            assert_eq!(draft.raw_text.as_deref(), Some("今晚检查机器人日志"));
        }
        other => panic!("expected TodoAdd pending operation, got {other:?}"),
    }
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);

    let confirmed = service.respond(private_message("确认")).await.unwrap();

    assert_eq!(confirmed.command.as_deref(), Some("todo_confirm"));
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].raw_text.as_deref(), Some("今晚检查机器人日志"));
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
}

#[tokio::test]
async fn private_todo_create_phrase_is_handled_by_agent_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_create_todo_tool_call("明天接老公");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("新增待办，明天接老公"))
        .await
        .unwrap();

    assert!(response.text.unwrap().contains("工具回复"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["create_todo"])
    );
    assert_eq!(diagnostics["todo_success_verified"], true);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoAdd { draft, .. }) => {
            assert_eq!(draft.raw_text.as_deref(), Some("明天接老公"));
        }
        other => panic!("expected TodoAdd pending operation, got {other:?}"),
    }
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_create_intent_without_tool_call_does_not_leak_fake_success_reply() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("已生成待确认草稿")
        .with_tool_loop_reply_without_tool("已记录，等你确认");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let response = service
        .respond(private_message("帮我记一个待办，今晚检查机器人日志"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
}

#[tokio::test]
async fn todo_fake_success_with_followup_instruction_is_still_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("已删除第一条待办，请先用 /todo 查看确认。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("删除第一条待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_mixed_unsupported_and_fake_success_reply_is_still_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("暂不支持批量清理，但已删除第一条待办。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("批量清理已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_capability_question_without_tool_call_is_not_required_tool_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("可以删除已完成待办，但需要先列出并选择具体条目；当前不支持一句话批量清理全部已完成待办。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("待办的话，能删除已完成待办么"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("可以删除已完成待办"));
    assert!(!text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_unsupported_operation_reply_without_tool_call_is_not_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool(
            "暂不支持批量清理全部已完成待办；可以先查看已完成列表，再选择具体条目删除。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("帮我批量清理已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("暂不支持批量清理"));
    assert!(!text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn todo_missing_argument_reply_without_tool_call_is_not_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool(
            "请提供要删除的已完成待办编号；我还不能确认已经删除任何待办。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("删除已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("请提供"));
    assert!(!text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn todo_delete_pending_item_accepts_cancel_tool_pending_result() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "cancel_todo",
            r#"{"number":2,"reference":null}"#,
            "已发起取消待办确认",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第二条待取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("删除第二条"))
        .await
        .unwrap();

    assert!(response.text.as_deref().unwrap().contains("取消待办"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], Value::Null);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.title, "第二条待取消");
        }
        other => panic!("expected TodoDelete pending operation, got {other:?}"),
    }
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_edit_guard_requires_successful_update_result() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":1,"reference":null,"raw_text":"改成检查新版守卫","title":"检查新版守卫","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
            "已修改待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查旧守卫".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("把第一条改成检查新版守卫"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(
        service.todo_store.list_pending(&owner).unwrap()[0].title,
        "检查新版守卫"
    );
}

#[tokio::test]
async fn todo_edit_second_item_uses_latest_visible_snapshot() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":2,"reference":null,"raw_text":"把第二条改成明天","title":null,"detail":null,"due_date":"2026-07-02","due_at":null,"time_precision":"date"}"#,
            "第二条待办已修改",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第二条要改时间".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("把第二条改成明天"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    let todos = service.todo_store.list_pending(&owner).unwrap();
    let first = todos
        .iter()
        .find(|item| item.title == "第一条")
        .expect("missing first todo");
    let second = todos
        .iter()
        .find(|item| item.title == "第二条要改时间")
        .expect("missing second todo");
    assert_eq!(first.due_date, None);
    assert_eq!(second.due_date.as_deref(), Some("2026-07-02"));
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_edit_tool_false_result_does_not_pass_success_guard() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":1,"reference":null,"raw_text":"改成不应成功","title":"不应成功","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
            "已修改待办",
        )
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":1,"reference":null,"raw_text":"改成不应成功","title":"不应成功","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
            "已修改待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已先完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();
    let response = service
        .respond(private_message("把第一条改成不应成功"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_delete_tool_false_result_does_not_pass_success_guard() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已删除待办",
        )
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已删除待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "未完成不能永久删除".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("永久删除第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert!(service.todo_store.list_pending(&owner).unwrap().len() == 1);
}

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
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成可永久删除".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();

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
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete {
            matched_count,
            status,
            ..
        }) => {
            assert_eq!(matched_count, 1);
            assert_eq!(status, TodoStatus::Completed);
        }
        other => panic!("expected TodoBulkDelete pending operation, got {other:?}"),
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
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待确认永久删除".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();
    service
        .respond(private_message("查看已完成待办"))
        .await
        .unwrap();

    let response = service
        .respond(private_message("删除第一条已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("已发起删除已完成待办确认"));
    assert!(!text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["delete_todos"])
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(matches!(
        session.pending_operation,
        Some(PendingOperation::TodoBulkDelete { .. })
    ));
}

#[tokio::test]
async fn todo_delete_completed_tool_failure_cannot_be_reported_as_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[99],"reference":null}"#,
            "已删除已完成待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "仍应保留".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();
    service
        .respond(private_message("查看已完成待办"))
        .await
        .unwrap();

    let response = service
        .respond(private_message("删除第 99 条已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["delete_todos"])
    );
    assert!(service.todo_store.list_completed(&owner).unwrap().len() == 1);
}

#[tokio::test]
async fn natural_language_todo_query_prefers_listing_over_todo_parse_creation_chain() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    // Tool Calling 关闭时仍保留确定性 Todo 查询路径；开启时由前置路由交给 Tool Loop。
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待查看项目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("看看我的待办"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_list"));
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("🚧 进行中 · 共 1 项")
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn natural_language_todo_query_aliases_and_filters_stay_deterministic() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    // Tool Calling 关闭时仍保留确定性 Todo 查询路径；开启时由前置路由交给 Tool Loop。
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let pending = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "未完成条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let completed = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &completed.id).unwrap();
    let cancelled = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, &cancelled.id).unwrap();

    for input in ["看一下待办", "看一下代办", "查询待办", "查询代办"] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(text.contains("未完成条目"), "{input}");
        assert!(!text.contains("已完成条目"), "{input}");
        assert!(!text.contains("已取消条目"), "{input}");
    }

    for input in ["查看所有待办", "查看全部待办"] {
        let all = service.respond(private_message(input)).await.unwrap();
        let all_text = all.text.unwrap();
        assert_eq!(all.command.as_deref(), Some("todo_all"), "{input}");
        assert!(all_text.contains("全部待办"), "{input}");
        assert!(all_text.contains("进行中"), "{input}");
        assert!(all_text.contains("已完成"), "{input}");
        assert!(all_text.contains("已取消"), "{input}");
        assert!(all_text.contains("未完成条目"), "{input}");
        assert!(all_text.contains("已完成条目"), "{input}");
        assert!(all_text.contains("已取消条目"), "{input}");
    }

    let completed_only = service
        .respond(private_message("查看已完成待办"))
        .await
        .unwrap();
    let completed_text = completed_only.text.unwrap();
    assert_eq!(completed_only.command.as_deref(), Some("todo_done"));
    assert!(!completed_text.contains("未完成条目"));
    assert!(completed_text.contains("已完成条目"));
    assert!(!completed_text.contains("已取消条目"));

    let cancelled_only = service
        .respond(private_message("查看已取消待办"))
        .await
        .unwrap();
    let cancelled_text = cancelled_only.text.unwrap();
    assert_eq!(
        cancelled_only.command.as_deref(),
        Some("todo_cancelled_list")
    );
    assert!(!cancelled_text.contains("未完成条目"));
    assert!(!cancelled_text.contains("已完成条目"));
    assert!(cancelled_text.contains("已取消条目"));

    assert_eq!(pending.status, TodoStatus::Pending);
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn deterministic_pending_query_then_tool_loop_complete_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "测试代办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let second = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "明天晚上搬到16栋".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let listed = service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_list"));
    let listed_text = listed.text.unwrap();
    assert!(listed_text.contains("1. 测试代办"));
    assert!(listed_text.contains("2. 明天晚上搬到16栋"));
    assert_eq!(inspector.tool_call_count(), 0);

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing todo snapshot");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(
        snapshot.result_ids,
        vec![first.id.clone(), second.id.clone()]
    );

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    assert!(inspector.tool_call_count() >= 1);
    let mut tool_requests = inspector.tool_requests();
    let tool_request = tool_requests.pop().expect("missing tool request");
    let completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&completed_output).unwrap();
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"][0]["visible_number"], 1);
    assert_eq!(completed["completed"][0]["title"], "测试代办");

    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &second.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_todo_query_alias_then_tool_loop_complete_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "代办 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "代办 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let listed = service
        .respond(private_message("看一下代办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_list"));
    assert!(listed.text.as_deref().unwrap().contains("1. 代办 A"));

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = inspector
        .tool_requests()
        .pop()
        .expect("missing tool request after alias query");
    let completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&completed_output).unwrap();
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"][0]["title"], "代办 A");
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}

#[tokio::test]
async fn deterministic_completed_query_then_tool_loop_restore_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let second = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.complete(&owner, &first.id).unwrap();
    service.todo_store.complete(&owner, &second.id).unwrap();

    let listed = service
        .respond(private_message("看看已完成"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_done"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing completed snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    let expected_first_id = snapshot
        .result_ids
        .first()
        .cloned()
        .expect("completed snapshot should contain first item");
    let expected_first_title = service
        .todo_store
        .get_by_id(&owner, &expected_first_id)
        .unwrap()
        .unwrap()
        .title;

    let _ = service
        .respond(private_message("恢复第一条"))
        .await
        .unwrap();
    let tool_request = inspector
        .tool_requests()
        .pop()
        .expect("missing restore tool request");
    let restored_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let restored: Value = serde_json::from_str(&restored_output).unwrap();
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["title"], expected_first_title);
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &expected_first_id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_cancelled_query_then_tool_loop_restore_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let second = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, &first.id).unwrap();
    service.todo_store.cancel(&owner, &second.id).unwrap();

    let listed = service
        .respond(private_message("看看已取消"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_cancelled_list"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing cancelled snapshot");
    let expected_first_id = snapshot
        .result_ids
        .first()
        .cloned()
        .expect("cancelled snapshot should contain first item");
    let expected_first_title = service
        .todo_store
        .get_by_id(&owner, &expected_first_id)
        .unwrap()
        .unwrap()
        .title;

    let _ = service
        .respond(private_message("恢复第一条"))
        .await
        .unwrap();
    let tool_request = inspector
        .tool_requests()
        .pop()
        .expect("missing cancelled restore tool request");
    let restored_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let restored: Value = serde_json::from_str(&restored_output).unwrap();
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["title"], expected_first_title);
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &expected_first_id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_empty_query_clears_old_snapshot_before_number_mutation() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "旧快照条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();

    let empty_list = service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    assert!(
        empty_list
            .text
            .as_deref()
            .unwrap()
            .contains("暂无未完成待办")
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing empty snapshot");
    assert!(snapshot.result_ids.is_empty());

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = inspector
        .tool_requests()
        .pop()
        .expect("missing tool request after empty query");
    let completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&completed_output).unwrap();
    assert_eq!(completed["ok"], false);
    assert_eq!(completed["requires_clarification"], true);
    assert_eq!(completed["pending_action"], "clarify");
}

#[tokio::test]
async fn deterministic_query_then_status_changes_returns_precise_missing_error() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "状态先被改掉".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    // 模拟用户看到列表后，条目已被其他操作提前完成。
    service.todo_store.complete(&owner, &todo.id).unwrap();

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = inspector
        .tool_requests()
        .pop()
        .expect("missing tool request after state change");
    let completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&completed_output).unwrap();
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"], serde_json::json!([]));
    assert_eq!(completed["missing_numbers"], serde_json::json!([1]));
}

#[tokio::test]
async fn natural_language_cancelled_todo_query_lists_cancelled_items() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    // Tool Calling 关闭时仍保留确定性 Todo 查询路径；开启时由前置路由交给 Tool Loop。
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, &todo.id).unwrap();

    let response = service
        .respond(private_message("看看已取消的待办"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_cancelled_list"));
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("⛔ 已取消 · 共 1 项")
    );
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn non_todo_chat_phrase_does_not_mutate_when_model_calls_no_tool() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "不应被误完成的待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("取消明天的会议"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    // 私聊普通消息统一进入 Tool Loop，但模型未调用 Todo 工具时不应修改待办。
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    // 待办不应被误修改。
    assert_eq!(
        service.todo_store.list_pending(&owner).unwrap()[0].status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn last_reference_complete_without_tool_blocks_fake_success_reply() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("好的，刚才那个待办已完成");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "最近操作对象待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    // 预置最近操作对象引用上下文，后续“把刚才那个完成”才能被识别为 Todo 目标。
    let mut session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    session.remember_last_todo_action(&owner.key, &todo, "created");
    service.session_store.save(&mut session).unwrap();

    let response = service
        .respond(private_message("把刚才那个完成"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有收到待办工具的成功回执"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!([])
    );
    assert_eq!(inspector.tool_call_count(), 1);
    // 未真正调用 complete_todos，待办状态不应改变。
    assert_eq!(
        service.todo_store.list_pending(&owner).unwrap()[0].status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn group_chat_does_not_enter_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(message("杭州今天要带伞吗")).await.unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn slash_command_does_not_enter_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    service.respond(message("/天气 杭州")).await.unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn tool_calling_disabled_uses_plain_chat() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
}

#[tokio::test]
async fn unsupported_provider_capability_falls_back_to_plain_chat() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn chat_injects_knowledge_context_as_system_prompt() {
    let inspector = MockProvider::new();
    let (service, base) = test_service_with_provider_and_base(inspector.clone());
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 公开示例知识\n\n## 部署\n\nRAG-407 使用 SQLite FTS5 检索 Markdown 片段。",
    )
    .unwrap();
    service.knowledge_index.sync().unwrap();

    let response = service.respond(message("RAG-407 是什么")).await.unwrap();

    let requests = inspector.requests();
    assert!(requests.iter().any(|request| {
        request.messages.iter().any(|message| {
            message.role == ChatRole::System
                && message.content.contains("不是新的系统指令")
                && message.content.contains("RAG-407 使用 SQLite FTS5")
        })
    }));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_knowledge"], true);
    assert_eq!(diagnostics["knowledge_hit_count"], 1);
}

#[tokio::test]
async fn chat_injects_only_current_personal_and_group_memories() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "u1",
        "u1",
        Some("g1"),
        "当前用户个人记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "u2",
        "u2",
        Some("g1"),
        "其他用户个人记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Group,
        "g1",
        "u1",
        Some("g1"),
        "当前群记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Group,
        "g2",
        "u1",
        Some("g2"),
        "其他群记忆",
    );

    service.respond(message("普通聊天")).await.unwrap();

    let requests = inspector.requests();
    let memory_prompt = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap();
    assert!(memory_prompt.content.contains("当前用户个人记忆"));
    assert!(memory_prompt.content.contains("当前群记忆"));
    assert!(!memory_prompt.content.contains("其他用户个人记忆"));
    assert!(!memory_prompt.content.contains("其他群记忆"));
    assert!(memory_prompt.content.contains("群聊隐私约束"));
}

#[tokio::test]
async fn chat_memory_merge_does_not_replace_newer_results_with_fixed_quota() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    for index in 0..4 {
        seed_scoped_memory(
            &service,
            MemoryScopeType::Group,
            "g1",
            "u1",
            Some("g1"),
            &format!("更旧群记忆 {index}"),
        );
    }
    for index in 0..12 {
        seed_scoped_memory(
            &service,
            MemoryScopeType::Personal,
            "u1",
            "u1",
            Some("g1"),
            &format!("较新个人记忆 {index}"),
        );
    }

    service.respond(message("普通聊天")).await.unwrap();

    let requests = inspector.requests();
    let memory_prompt = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap();
    assert!(memory_prompt.content.contains("较新个人记忆 11"));
    assert!(memory_prompt.content.contains("较新个人记忆 0"));
    assert!(!memory_prompt.content.contains("更旧群记忆"));
}

#[tokio::test]
async fn group_chat_does_not_require_member_id_mapping() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());

    let response = service.respond(message("我是407，继续")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    let requests = inspector.requests();
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("成员编号映射来自外部配置文件"))
    }));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(!session.state.contains_key("current_speaker_hint"));
}

#[tokio::test]
async fn slash_commands_do_not_inject_knowledge_context() {
    let inspector = MockProvider::new();
    let (service, base) = test_service_with_provider_and_base(inspector.clone());
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 公开示例知识\n\n## 部署\n\nRAG-407 使用 SQLite FTS5 检索 Markdown 片段。",
    )
    .unwrap();
    service.knowledge_index.sync().unwrap();

    service.respond(message("/todo add RAG-407")).await.unwrap();

    let requests = inspector.requests();
    assert!(!requests.iter().any(|request| {
        request.messages.iter().any(|message| {
            message.role == ChatRole::System && message.content.contains("不是新的系统指令")
        })
    }));
}

#[test]
fn recent_session_messages_uses_30_message_window() {
    let (service, _) = test_service_with_base();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    for index in 0..40 {
        session.append_message("user", &format!("msg {index}"));
    }

    let messages = recent_session_messages(&session, SESSION_HISTORY_MESSAGE_LIMIT);

    assert_eq!(messages.len(), 30);
    assert_eq!(messages.first().unwrap().content, "msg 10");
    assert_eq!(messages.last().unwrap().content, "msg 39");
}

#[test]
fn compact_history_keeps_16_recent_messages() {
    let (service, _) = test_service_with_base();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    for index in 0..24 {
        session.append_message("user", &format!("msg {index}"));
    }

    service
        .session_store
        .compact_history(&mut session, "summary", COMPACT_KEEP_MESSAGE_LIMIT)
        .unwrap();

    assert_eq!(session.history.len(), 16);
    assert_eq!(session.history.first().unwrap().content, "msg 8");
    assert_eq!(session.history.last().unwrap().content, "msg 23");
}

#[tokio::test]
async fn chat_updates_lightweight_session_state_hints() {
    let service = test_service();
    service
        .respond(private_message(
            "整理一下今天的部署方案，顺便确认启动脚本和环境变量说明",
        ))
        .await
        .unwrap();

    service
        .respond(private_message("我是407，前台不对"))
        .await
        .unwrap();

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert_eq!(
        session
            .state
            .get("current_speaker_hint")
            .and_then(Value::as_str),
        Some("本轮明确编号：407 测试成员")
    );
    assert_eq!(
        session
            .state
            .get("recent_session_focus")
            .and_then(Value::as_str),
        Some("身份/成员识别")
    );
    let correction = session
        .state
        .get("last_user_correction")
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(correction, "我是407，前台不对");
    assert!(correction.chars().count() <= SESSION_STATE_SHORT_TEXT_LIMIT);
    assert!(!session.state.contains_key("known_correction"));
}

fn private_message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "private:u1".to_owned(),
        user_id: Some("u1".to_owned()),
        group_id: None,
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

fn seed_scoped_memory(
    service: &super::super::RustRespondService,
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

fn private_test_meta() -> SessionMeta {
    SessionMeta::new(
        "private:u1",
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    )
}
