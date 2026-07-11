//! Todo Agent 在 Respond 边界的集成测试。
//!
//! 保留真实工具结果控制成功状态、可见快照、会话保存和 Pending 交接等高层语义；
//! Tool schema、参数解析、内部编号和持久化细节由 `runtime::tools::todo` 测试负责。

use qq_maid_llm::provider::ToolCallingProtocol;
use serde_json::Value;

use crate::runtime::{
    session::SessionMeta,
    tools::todo::{TodoItemDraft, TodoPendingOperation, TodoStatus, TodoStore, TodoTimePrecision},
};

use super::support::*;

#[tokio::test]
async fn ordinary_chat_response_does_not_inherit_old_todo_visible_snapshot() {
    let service = test_service();
    create_private_todo(&service, "旧列表第一条");

    let list_response = service.respond(private_message("/todo")).await.unwrap();
    assert!(
        list_response.visible_entity_snapshot.is_some(),
        "deterministic todo list should bind its own snapshot"
    );

    let chat_response = service
        .respond(private_message("普通聊一句，不展示待办编号"))
        .await
        .unwrap();

    assert!(
        chat_response.visible_entity_snapshot.is_none(),
        "ordinary chat response must not bind stale last_todo_query"
    );
}

#[tokio::test]
async fn list_todos_due_date_receipt_preserves_filtered_visible_snapshot() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "list_todos",
            r#"{"status":"pending","due_date":"2026-07-03"}"#,
            "今天待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
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
            },
        )
        .unwrap();
    let today = service
        .task_store
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    service
        .task_store
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("检查今天待办状态"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("今天事项"));
    assert!(!text.contains("明天事项"));
    assert!(!text.contains("无时间事项"));

    let session = service
        .session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing filtered snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, "2026-07-03");
    assert_eq!(snapshot.result_ids, vec![today.id]);
}

#[tokio::test]
async fn list_todos_completed_date_range_receipt_uses_completed_at_snapshot() {
    let ctx = qq_maid_common::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![raw_tool_result(
                "list_todos",
                serde_json::json!({
                    "status": "completed",
                    "due_date": null,
                    "due_start": yesterday.format("%Y-%m-%d").to_string(),
                    "due_end": today.format("%Y-%m-%d").to_string(),
                    "date_range_text": "这两天",
                    "date_range_field": "completed_at",
                    "items": [],
                    "count": 1
                }),
                true,
            )],
            "昨天完成的待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let completed_in_range = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "昨天完成但计划较早".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some(before_range.format("%Y-%m-%d").to_string()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let planned_in_range = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "计划昨天但完成较早".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some(yesterday.format("%Y-%m-%d").to_string()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    service
        .task_store
        .complete(&owner, &completed_in_range.id)
        .unwrap();
    service
        .task_store
        .complete(&owner, &planned_in_range.id)
        .unwrap();
    let mut items = service.task_store.list_all(&owner).unwrap();
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
    service
        .task_store
        .set_items_for_test(&owner, &items)
        .unwrap();

    let response = service
        .respond(private_message("检查待办状态"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("昨天完成但计划较早"));
    assert!(!text.contains("计划昨天但完成较早"), "{text}");
    let diagnostics = response.diagnostics.as_ref().unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["list_todos"])
    );

    let session = service
        .session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing filtered snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    assert_eq!(snapshot.condition, "这两天");
    assert_eq!(snapshot.result_ids, vec![completed_in_range.id]);
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
async fn todo_create_receipt_shows_full_user_visible_card() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "create_todo",
            r#"{"items":null,"content":"装宽带","title":"装宽带","detail":"提前确认地址并携带身份证","due_date":"2099-01-01","due_at":"2099-01-01 10:00:00","reminder_at":"2099-01-01 09:30","time_precision":"date_time"}"#,
            "已新增待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("帮我新增待办：装宽带"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("装宽带 · 时间：99-01-01 10:00（四）"));
    assert!(text.contains("提醒："));
    assert!(text.contains("99-01-01 9:30（四）"));
    assert!(text.contains("详情：\n提前确认地址并携带身份证"));
    assert!(!text.contains("created_at"));
    assert!(!text.contains("scope"));
    let markdown = response.markdown.unwrap();
    assert!(markdown.contains("**时间**"));
    assert!(markdown.contains("**提醒**"));
    assert!(markdown.contains("详情：\n提前确认地址并携带身份证"));
}

#[tokio::test]
async fn todo_edit_receipt_shows_final_detail_card() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":1,"reference":null,"raw_text":"把第一条详情改成提前确认地址","title":null,"detail":"提前确认地址","due_date":null,"due_at":null,"reminder_at":null,"time_precision":null}"#,
            "已修改待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "装宽带".to_owned(),
                detail: Some("旧详情".to_owned()),
                raw_text: None,
                due_date: Some("2099-01-01".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    service.respond(private_message("/todo")).await.unwrap();

    let response = service
        .respond(private_message("把第一条详情改成提前确认地址"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✏️ 已修改待办"));
    assert!(text.contains("装宽带 · 时间：99-01-01（四）"));
    assert!(text.contains("详情：\n提前确认地址"));
    // 写操作默认不再刷新完整列表；详情只需在修改回执本身展示。
    assert!(!text.contains("🚧 当前进行中"));
    assert!(!text.contains("旧详情"));
    assert!(!text.contains("created_at"));
    assert_eq!(
        service.task_store.list_pending(&owner).unwrap()[0]
            .detail
            .as_deref(),
        Some("提前确认地址")
    );
}

#[tokio::test]
async fn todo_edit_receipt_clears_detail_after_successful_tool_result() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "edit_todo",
            r#"{"number":1,"reference":null,"raw_text":"清除第一条详情","title":null,"detail":"","due_date":null,"due_at":null,"reminder_at":null,"time_precision":null,"recurrence_kind":null,"recurrence_interval":null,"recurrence_unit":null,"recurrence_interval_days":null}"#,
            "第一条详情已清除",
        );
    let service = test_service_with_provider_and_tool_calling(inspector, true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "装宽带".to_owned(),
                detail: Some("旧详情不能再显示".to_owned()),
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
        .respond(private_message("清除第一条详情"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✏️ 已修改待办"));
    assert!(!text.contains("旧详情不能再显示"));
    assert!(!text.contains("详情："));
    assert_eq!(
        service.task_store.list_pending(&owner).unwrap()[0].detail,
        None
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
}

#[tokio::test]
async fn todo_tool_loop_clears_third_and_fourth_details() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "edit_todo",
                    r#"{"number":3,"reference":null,"raw_text":"第三条和第四条详情都不需要","title":null,"detail":"","due_date":null,"due_at":null,"reminder_at":null,"time_precision":null,"recurrence_kind":null,"recurrence_interval":null,"recurrence_unit":null,"recurrence_interval_days":null}"#,
                ),
                (
                    "edit_todo",
                    r#"{"number":4,"reference":null,"raw_text":"第三条和第四条详情都不需要","title":null,"detail":"","due_date":null,"due_at":null,"reminder_at":null,"time_precision":null,"recurrence_kind":null,"recurrence_interval":null,"recurrence_unit":null,"recurrence_interval_days":null}"#,
                ),
            ],
            "第三条和第四条详情已清除",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let mut ids = Vec::new();
    for number in 1..=4 {
        ids.push(
            service
                .task_store
                .create(
                    &owner,
                    TodoItemDraft {
                        title: format!("第{number}条"),
                        detail: Some(format!("第{number}条旧详情")),
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
                .unwrap()
                .id,
        );
    }
    service.respond(private_message("/todo")).await.unwrap();

    let response = service
        .respond(private_message("第三条和第四条详情都不需要"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert_eq!(text.matches("✏️ 已修改待办").count(), 2);
    assert!(!text.contains("第3条旧详情"));
    assert!(!text.contains("第4条旧详情"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &ids[2])
            .unwrap()
            .unwrap()
            .detail,
        None
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &ids[3])
            .unwrap()
            .unwrap()
            .detail,
        None
    );
    let listed = service.respond(private_message("/todo")).await.unwrap();
    let listed_text = listed.text.unwrap();
    assert!(!listed_text.contains("第3条旧详情"));
    assert!(!listed_text.contains("第4条旧详情"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["edit_todo", "edit_todo"])
    );
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(inspector.tool_call_count(), 1);
}

#[tokio::test]
async fn todo_complete_receipt_reuses_full_user_visible_card() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"selection_text":null,"reference":null}"#,
            "已完成待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "装宽带".to_owned(),
                detail: Some("提前确认地址并携带身份证".to_owned()),
                raw_text: None,
                due_date: Some("2099-01-01".to_owned()),
                due_at: Some("2099-01-01 10:00:00".to_owned()),
                reminder_at: Some("2099-01-01 09:30:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    service.respond(private_message("/todo")).await.unwrap();

    let response = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已完成待办"));
    assert!(text.contains("状态：已完成"));
    assert!(text.contains("装宽带 · 时间：99-01-01 10:00（四）"));
    assert!(text.contains("提醒："));
    assert!(text.contains("99-01-01 9:30（四）"));
    assert!(text.contains("详情：\n提前确认地址并携带身份证"));
    assert!(text.contains("完成时间："));
    assert!(!text.contains("created_at"));
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
        service.task_store.list_pending(&owner).unwrap()[0].title,
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第一条".to_owned(),
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
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第二条要改时间".to_owned(),
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
        .respond(private_message("把第二条改成明天"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    let todos = service.task_store.list_pending(&owner).unwrap();
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
async fn todo_internal_list_before_write_is_not_user_visible_query() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("list_todos", r#"{"status":"pending"}"#),
                (
                    "complete_todos",
                    r#"{"numbers":[1],"selection_text":null,"reference":null}"#,
                ),
            ],
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "先完成".to_owned(),
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
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "仍进行中".to_owned(),
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
        .respond(private_message("完成第一项待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已完成待办"));
    assert!(!text.contains("🚧 当前进行中 · 共 1 项"));
    assert!(!text.contains("先完成\n状态：未完成"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["list_todos", "complete_todos"])
    );
    let outcomes = diagnostics["tool_outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["tool"], "complete_todos");
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing background refreshed snapshot");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(snapshot.result_ids.len(), 1);
    assert_eq!(inspector.tool_call_count(), 1);
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
async fn todo_write_with_explicit_list_does_not_append_auto_related_list() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "complete_todos",
                    r#"{"numbers":[1],"selection_text":null,"reference":null}"#,
                ),
                ("list_todos", r#"{"status":"completed"}"#),
            ],
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "先完成".to_owned(),
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
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "仍进行中".to_owned(),
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
        .respond(private_message("处理第一项，然后列出已完成项目"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已完成待办"));
    assert!(text.contains("✅ 当前已完成 · 共 1 项"));
    assert!(!text.contains("🚧 当前进行中 · 共 1 项"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["complete_todos", "list_todos"])
    );
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 2);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing visible snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    assert_eq!(snapshot.result_ids, vec![first.id]);
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

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
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

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
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
        Some(TodoPendingOperation::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(item_ids.len(), 1);
            assert_eq!(status, TodoStatus::Pending);
        }
        other => panic!("expected pending bulk delete operation, got {other:?}"),
    }
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
        .respond(private_message("查看已完成待办"))
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
async fn natural_language_todo_query_prefers_listing_over_todo_parse_creation_chain() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    // Tool Calling 关闭时仍保留确定性 Todo 查询路径；开启时由前置路由交给 Tool Loop。
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待查看项目".to_owned(),
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
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "未完成条目".to_owned(),
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
    let completed = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成条目".to_owned(),
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
    service.task_store.complete(&owner, &completed.id).unwrap();
    for input in ["看一下待办", "看一下代办", "查询待办", "查询代办"] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(text.contains("未完成条目"), "{input}");
        assert!(!text.contains("已完成条目"), "{input}");
        assert!(!text.contains("已取消条目"), "{input}");
    }

    for input in [
        "查看未完成的待办",
        "看看没做完的任务",
        "查看还没做完的任务",
        "查看未结束的待办",
    ] {
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
        assert!(all_text.contains("未完成条目"), "{input}");
        assert!(all_text.contains("已完成条目"), "{input}");
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

    for input in ["查看完成的待办", "看看做完的任务"] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_done"), "{input}");
        assert!(!text.contains("未完成条目"), "{input}");
        assert!(text.contains("已完成条目"), "{input}");
        assert!(!text.contains("已取消条目"), "{input}");
    }

    assert_eq!(pending.status, TodoStatus::Pending);
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn todo_completed_lists_use_dynamic_collapse_hints() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=9 {
        let completed = service
            .task_store
            .create(&owner, todo_draft(format!("已完成 {index}")))
            .unwrap();
        service.task_store.complete(&owner, &completed.id).unwrap();
    }

    let completed = service
        .respond(private_message("查看已完成待办"))
        .await
        .unwrap();
    let completed_text = completed.text.unwrap();
    assert!(completed_text.contains("✅ 已完成 · 共 9 项"));
    assert!(completed_text.contains("还有 4 项已完成待办，可说“查看全部已完成待办”。"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing completed snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    assert_eq!(snapshot.result_ids.len(), 5);

    let completed_full = service
        .respond(private_message("查看全部已完成待办"))
        .await
        .unwrap();
    let completed_full_text = completed_full.text.unwrap();
    assert!(!completed_full_text.contains("还有 4 项已完成待办"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing full completed snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    assert_eq!(snapshot.result_ids.len(), 9);
}

#[tokio::test]
async fn todo_date_filter_collapse_hint_restores_full_result_scope() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=9 {
        let item = service
            .task_store
            .create(&owner, todo_draft(format!("今天完成 {index}")))
            .unwrap();
        service.task_store.complete(&owner, &item.id).unwrap();
    }

    let response = service
        .respond(private_message("/todo 截至今天完成"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("已完成待办：截至今天完成"));
    assert!(text.contains("还有 4 项截至今天完成的已完成待办，可说“查看完整结果”。"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing date snapshot");
    assert_eq!(snapshot.query_type, "completed-time");
    assert_eq!(snapshot.condition, "截至今天完成");
    assert_eq!(snapshot.result_ids.len(), 5);

    let full = service
        .respond(private_message("查看完整结果"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();
    assert!(full_text.contains("已完成待办：截至今天完成"));
    assert!(!full_text.contains("还有 4 项截至今天完成的已完成待办"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing full date snapshot");
    assert_eq!(snapshot.query_type, "completed-time");
    assert_eq!(snapshot.condition, "截至今天完成");
    assert_eq!(snapshot.result_ids.len(), 9);
}

#[tokio::test]
async fn todo_all_collapse_hint_restores_full_result_with_tool_loop_enabled() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=10 {
        service
            .task_store
            .create(&owner, todo_draft(format!("全部待办 {index}")))
            .unwrap();
    }

    let collapsed = service.respond(private_message("全部待办")).await.unwrap();
    let collapsed_text = collapsed.text.unwrap();
    assert_eq!(collapsed.command.as_deref(), Some("todo_all"));
    assert!(collapsed_text.contains("📋 全部待办 · 共 10 项"));
    assert!(collapsed_text.contains("还有 5 项待办，可说“查看完整结果”。"));

    let full = service
        .respond(private_message("查看完整结果"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();

    assert_eq!(full.command.as_deref(), Some("todo_all"));
    assert!(full_text.contains("📋 全部待办 · 共 10 项"));
    assert!(full_text.contains("全部待办 10"));
    assert!(!full_text.contains("还有 5 项待办"));
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn complete_todo_phrase_lists_all_statuses_fully_with_tool_loop_enabled() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=6 {
        service
            .task_store
            .create(&owner, todo_draft(format!("进行中待办 {index}")))
            .unwrap();
    }
    for index in 1..=2 {
        let item = service
            .task_store
            .create(&owner, todo_draft(format!("已完成待办 {index}")))
            .unwrap();
        service.task_store.complete(&owner, &item.id).unwrap();
    }
    let pending = service.respond(private_message("查看待办")).await.unwrap();
    let pending_text = pending.text.unwrap();
    assert_eq!(pending.command.as_deref(), Some("todo_list"));
    assert!(pending_text.contains("🚧 进行中 · 共 6 项"));
    assert!(!pending_text.contains("已完成待办 1"));

    let full = service
        .respond(private_message("查看完整待办"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();

    assert_eq!(full.command.as_deref(), Some("todo_all"));
    assert!(full_text.contains("📋 全部待办 · 共 8 项"));
    assert!(full_text.contains("进行中待办 6"));
    assert!(full_text.contains("已完成待办 1"));
    assert!(!full_text.contains("还有 5 项待办"));
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn todo_write_or_question_phrases_do_not_enter_natural_query_path() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);

    for input in ["取消这个待办", "怎么取消待办", "帮我取消第一条", "不做了"]
    {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_ne!(response.command.as_deref(), Some("todo_list"), "{input}");
    }
    assert_eq!(inspector.tool_call_count(), 0);
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
