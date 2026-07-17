//! Todo 查询、过滤、折叠结果和用户可见快照的 Respond 集成测试。

use qq_maid_llm::provider::ToolCallingProtocol;

use crate::runtime::{
    session::SessionMeta,
    tools::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision},
};

use super::super::support::*;

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
async fn natural_language_tool_query_combines_tomorrow_status_and_keyword() {
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let tomorrow = (today + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let day_after = (today + chrono::Duration::days(2))
        .format("%Y-%m-%d")
        .to_string();
    let arguments = serde_json::json!({
        "status": "pending",
        "due_date": null,
        "date_range_text": "明天",
        "time_filter": null,
        "keyword": "项目 A"
    })
    .to_string();
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", arguments, "查询完成");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for (title, date) in [
        ("项目 A 报告", tomorrow.as_str()),
        ("项目 B 报告", tomorrow.as_str()),
        ("项目 A 后续", day_after.as_str()),
    ] {
        service
            .task_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some(date.to_owned()),
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
    }

    let response = service
        .respond(private_message("明天项目 A 的未完成事项"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("项目 A 报告"));
    assert!(!text.contains("项目 B 报告"));
    assert!(!text.contains("项目 A 后续"));
    assert_eq!(inspector.tool_call_count(), 1);

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing combined snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.result_ids.len(), 1);
}

#[tokio::test]
async fn natural_language_tool_query_supports_fuzzy_keyword_search() {
    let arguments = serde_json::json!({
        "status": "pending",
        "due_date": null,
        "date_range_text": null,
        "time_filter": null,
        "keyword": "报销"
    })
    .to_string();
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", arguments, "查询完成");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let matched = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "提交报销报告".to_owned(),
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
    create_private_todo(&service, "检查服务器日志");

    let response = service
        .respond(private_message("找标题里有报销的待办"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("提交报销报告"));
    assert!(!text.contains("检查服务器日志"));
    assert_eq!(inspector.tool_call_count(), 1);

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing keyword snapshot");
    assert_eq!(snapshot.query_type, "search");
    assert_eq!(snapshot.condition, "关键词“报销”");
    assert_eq!(snapshot.result_ids, vec![matched.id]);
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
async fn todo_completed_lists_show_up_to_ten_without_old_five_item_collapse() {
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
    assert!(!completed_text.contains("还有"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing completed snapshot");
    assert_eq!(snapshot.query_type, "completed-list");
    assert_eq!(snapshot.result_ids.len(), 9);
}

#[tokio::test]
async fn todo_date_filter_shows_nine_without_old_five_item_collapse() {
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
    assert!(!text.contains("还有"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing date snapshot");
    assert_eq!(snapshot.query_type, "completed-time");
    assert_eq!(snapshot.condition, "截至今天完成");
    assert_eq!(snapshot.result_ids.len(), 9);
}

#[tokio::test]
async fn todo_all_caps_at_ten_and_reports_real_total() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=11 {
        service
            .task_store
            .create(&owner, todo_draft(format!("全部待办 {index}")))
            .unwrap();
    }

    let collapsed = service.respond(private_message("全部待办")).await.unwrap();
    let collapsed_text = collapsed.text.unwrap();
    assert_eq!(collapsed.command.as_deref(), Some("todo_all"));
    assert!(collapsed_text.contains("📋 全部待办 · 共 11 项"));
    assert!(collapsed_text.contains("全部待办 10"));
    assert!(!collapsed_text.contains("全部待办 11"));
    assert!(collapsed_text.contains("共找到 11 条待办，当前展示前 10 条"));
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
