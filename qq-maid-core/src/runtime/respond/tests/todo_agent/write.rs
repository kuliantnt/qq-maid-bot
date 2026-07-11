//! Todo 真实写操作、持久化状态和用户回执的 Respond 集成测试。

use qq_maid_llm::provider::ToolCallingProtocol;

use crate::runtime::tools::todo::{TodoItemDraft, TodoStore, TodoTimePrecision};

use super::super::support::*;

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
