//! Todo 成功声明必须由真实工具结果验证的 Respond 安全边界测试。

use qq_maid_llm::provider::ToolCallingProtocol;
use serde_json::Value;

use crate::runtime::tools::todo::{
    TodoItemDraft, TodoPendingPayload, TodoStatus, TodoStore, TodoTimePrecision,
};

use super::super::support::*;

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
    assert!(text.contains("这次没有确认改动成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
    assert!(service.task_store.list_all(&owner).unwrap().is_empty());
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
}

#[tokio::test]
async fn todo_detail_clear_promise_without_tool_call_is_blocked() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("第三条详情以后不会显示了。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: Some("必须保留的原详情".to_owned()),
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
        .respond(private_message("第三条不要详情了"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("这次没有确认改动成功"), "{text}");
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .detail
            .as_deref(),
        Some("必须保留的原详情")
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(inspector.tool_call_count(), 1);
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
    assert!(text.contains("这次没有确认改动成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
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
    assert!(text.contains("这次没有确认改动成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
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
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
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
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
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
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查旧守卫".to_owned(),
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

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("把第一条改成检查新版守卫"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(
        service.task_store.list_pending(&owner).unwrap()[0].title,
        "检查新版守卫"
    );
}

#[tokio::test]
async fn todo_write_result_is_returned_when_final_agent_round_fails() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_then_error(
            vec![(
                "complete_todos",
                r#"{"numbers":[1],"selection_text":null,"reference":null}"#,
            )],
            crate::error::LlmError::new(
                "context_budget_exceeded",
                "tool loop context budget exceeded",
                "tool_loop",
            ),
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todo = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "确认线上回执".to_owned(),
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

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("完成第一条待办"))
        .await
        .unwrap();

    assert!(response.ok);
    assert!(
        response
            .text
            .as_deref()
            .is_some_and(|text| text.contains("✅ 已完成待办") && text.contains("确认线上回执"))
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_finalization_fallback_used"], true);
    assert_eq!(
        diagnostics["agent_finalization_error_code"],
        "context_budget_exceeded"
    );
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["complete_todos"])
    );
    assert_eq!(inspector.tool_call_count(), 1);
    assert!(service.task_store.list_pending(&owner).unwrap().is_empty());
    assert_eq!(
        service
            .task_store
            .list_completed(&owner)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![todo.id]
    );

    let exposed_tools = inspector.tool_requests()[0]
        .tools
        .metadata()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(exposed_tools.contains(&"complete_todos".to_owned()));
    assert!(!exposed_tools.contains(&"restore_todos".to_owned()));
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已先完成".to_owned(),
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

    service.respond(private_message("/todo")).await.unwrap();
    service.task_store.complete(&owner, &todo.id).unwrap();
    let response = service
        .respond(private_message("把第一条改成不应成功"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("目标待办当前状态不允许执行这次操作"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_reference_invalid_state");
    assert_eq!(
        diagnostics["todo_tool_results"][0]["error_code"],
        "todo_reference_invalid_state"
    );
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_delete_pending_item_false_deleted_text_does_not_pass_success_guard() {
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中可发起永久删除确认".to_owned(),
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

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("永久删除第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("确认删除以下 1 项待办吗"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert!(service.task_store.list_pending(&owner).unwrap().len() == 1);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingPayload::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(item_ids.len(), 1);
            assert_eq!(status, TodoStatus::Pending);
        }
        other => panic!("expected pending bulk delete operation, got {other:?}"),
    }
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "仍应保留".to_owned(),
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
        .respond(private_message("/todo done"))
        .await
        .unwrap();

    let response = service
        .respond(private_message("删除第 99 条已完成待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("这次选择的待办已经不可用或编号不存在"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["error_code"], "todo_selection_not_found");
    assert_eq!(
        diagnostics["todo_tool_results"][0]["error_code"],
        "todo_selection_not_found"
    );
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["delete_todos"])
    );
    assert!(service.task_store.list_completed(&owner).unwrap().len() == 1);
}

#[tokio::test]
async fn non_todo_chat_phrase_does_not_mutate_when_model_calls_no_tool() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "不应被误完成的待办".to_owned(),
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
        .respond(private_message("取消明天的会议"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
    // 模型可以看到工具，但本轮没有发出 Tool Call，因此不能产生 Todo 副作用。
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_calling_used"], false);
    assert_eq!(diagnostics["agent_result"], "direct_answer");
    // 待办不应被误修改。
    assert_eq!(
        service.task_store.list_pending(&owner).unwrap()[0].status,
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "最近操作对象待办".to_owned(),
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
    assert!(text.contains("这次没有确认改动成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], "todo_success_not_verified");
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
    assert_eq!(inspector.tool_call_count(), 1);
    // 未真正调用 complete_todos，待办状态不应改变。
    assert_eq!(
        service.task_store.list_pending(&owner).unwrap()[0].status,
        TodoStatus::Pending
    );
}
