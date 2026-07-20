//! Todo 查询、过滤、折叠结果和用户可见快照的 Respond 集成测试。

use std::collections::HashSet;

use qq_maid_llm::provider::ToolCallingProtocol;
use serde_json::Value;

use crate::runtime::{
    respond::RustRespondService,
    session::SessionMeta,
    tools::todo::{
        TodoItemDraft, TodoRecurrenceKind, TodoRecurrenceUnit, TodoStatus, TodoStore,
        TodoTimePrecision,
    },
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
async fn explicit_todo_command_aliases_and_filters_stay_deterministic() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
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

    for input in ["/todo", "/todo list", "/todo list 未完成"] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(text.contains("未完成条目"), "{input}");
        assert!(!text.contains("已完成条目"), "{input}");
    }

    let all = service.respond(private_message("/todo all")).await.unwrap();
    let all_text = all.text.unwrap();
    assert_eq!(all.command.as_deref(), Some("todo_all"));
    assert!(all_text.contains("全部待办"));
    assert!(all_text.contains("未完成条目"));
    assert!(all_text.contains("已完成条目"));

    let completed_only = service
        .respond(private_message("/todo done"))
        .await
        .unwrap();
    let completed_text = completed_only.text.unwrap();
    assert_eq!(completed_only.command.as_deref(), Some("todo_done"));
    assert!(!completed_text.contains("未完成条目"));
    assert!(completed_text.contains("已完成条目"));

    assert_eq!(pending.status, TodoStatus::Pending);
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn natural_language_todo_queries_enter_tool_loop_instead_of_shortcut() {
    let list_args = r#"{"status":"pending","due_date":null,"date_range_text":null,"time_filter":null,"keyword":null,"recurring":null}"#;
    // MockProvider 的 tool action 按次消费，多轮自然语言查询要预置同样次数的 list_todos。
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", list_args, "查询完成")
        .with_tool_call_json("list_todos", list_args, "查询完成")
        .with_tool_call_json("list_todos", list_args, "查询完成");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    create_private_todo(&service, "自然语言待办");

    // route 层识别“看一下/查看 + 待办/代办”为 Todo 查询意图后进入 Tool Loop。
    for input in ["看一下待办", "看一下代办", "查看待办"] {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(response.text.unwrap().contains("自然语言待办"), "{input}");
    }
    assert_eq!(inspector.tool_call_count(), 3);
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
        .respond(private_message("/todo done"))
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

    let collapsed = service.respond(private_message("/todo all")).await.unwrap();
    let collapsed_text = collapsed.text.unwrap();
    assert_eq!(collapsed.command.as_deref(), Some("todo_all"));
    assert!(collapsed_text.contains("📋 全部待办 · 共 11 项"));
    assert!(collapsed_text.contains("全部待办 10"));
    assert!(!collapsed_text.contains("全部待办 11"));
    assert!(collapsed_text.contains("共找到 11 条待办，当前展示前 10 条"));
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn explicit_todo_and_full_result_restore_use_visible_snapshot() {
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
    let pending = service.respond(private_message("/todo")).await.unwrap();
    let pending_text = pending.text.unwrap();
    assert_eq!(pending.command.as_deref(), Some("todo_list"));
    assert!(pending_text.contains("🚧 进行中 · 共 6 项"));
    assert!(!pending_text.contains("已完成待办 1"));

    // 先建立可见快照，再请求完整结果；普通自然语言“查看完整待办”不再短路。
    let full = service
        .respond(private_message("查看完整结果"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();
    assert_eq!(full.command.as_deref(), Some("todo_list"));
    assert!(full_text.contains("进行中待办 6"));
    assert!(!full_text.contains("已完成待办 1"));
    assert_eq!(inspector.tool_call_count(), 0);
}

#[derive(Debug, Clone, Copy)]
enum ReplayCombination {
    PendingDateRecurring,
    CompletedRecurring,
    AllOneOff,
    PendingKeywordRecurring,
    PendingOverdueOneOff,
}

impl ReplayCombination {
    fn name(self) -> &'static str {
        match self {
            Self::PendingDateRecurring => "pending-date-recurring",
            Self::CompletedRecurring => "completed-recurring",
            Self::AllOneOff => "all-one-off",
            Self::PendingKeywordRecurring => "pending-keyword-recurring",
            Self::PendingOverdueOneOff => "pending-overdue-one-off",
        }
    }

    fn arguments(self, target_date: &str) -> String {
        let value = match self {
            Self::PendingDateRecurring => serde_json::json!({
                "status": "pending",
                "due_date": target_date,
                "date_range_text": null,
                "time_filter": null,
                "keyword": null,
                "recurring": true
            }),
            Self::CompletedRecurring => serde_json::json!({
                "status": "completed",
                "due_date": null,
                "date_range_text": null,
                "time_filter": null,
                "keyword": null,
                "recurring": true
            }),
            Self::AllOneOff => serde_json::json!({
                "status": "all",
                "due_date": null,
                "date_range_text": null,
                "time_filter": null,
                "keyword": null,
                "recurring": false
            }),
            Self::PendingKeywordRecurring => serde_json::json!({
                "status": "pending",
                "due_date": null,
                "date_range_text": null,
                "time_filter": null,
                "keyword": "专项",
                "recurring": true
            }),
            Self::PendingOverdueOneOff => serde_json::json!({
                "status": "pending",
                "due_date": null,
                "date_range_text": null,
                "time_filter": "overdue",
                "keyword": null,
                "recurring": false
            }),
        };
        value.to_string()
    }
}

#[tokio::test]
async fn full_result_replays_all_structured_todo_filter_combinations() {
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let target_date = (today + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let overdue_date = (today - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let future_date = (today + chrono::Duration::days(2))
        .format("%Y-%m-%d")
        .to_string();

    for combination in [
        ReplayCombination::PendingDateRecurring,
        ReplayCombination::CompletedRecurring,
        ReplayCombination::AllOneOff,
        ReplayCombination::PendingKeywordRecurring,
        ReplayCombination::PendingOverdueOneOff,
    ] {
        let inspector = MockProvider::new()
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_call_json(
                "list_todos",
                combination.arguments(&target_date),
                "查询完成",
            );
        let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
        let owner = TodoStore::owner(Some("u1"), "private:u1");
        let mut expected_ids = Vec::new();

        for index in 1..=11 {
            let title = format!("{} 匹配项 {index}", combination.name());
            let mut draft = todo_draft(title);
            match combination {
                ReplayCombination::PendingDateRecurring => {
                    draft.due_date = Some(target_date.clone());
                    draft.time_precision = TodoTimePrecision::Date;
                    make_draft_recurring(&mut draft);
                }
                ReplayCombination::CompletedRecurring => {
                    draft.due_date = Some(target_date.clone());
                    draft.time_precision = TodoTimePrecision::Date;
                    make_draft_recurring(&mut draft);
                }
                ReplayCombination::AllOneOff => {}
                ReplayCombination::PendingKeywordRecurring => {
                    draft.title = format!("专项周期匹配项 {index}");
                    draft.due_date = Some(target_date.clone());
                    draft.time_precision = TodoTimePrecision::Date;
                    make_draft_recurring(&mut draft);
                }
                ReplayCombination::PendingOverdueOneOff => {
                    draft.due_date = Some(overdue_date.clone());
                    draft.time_precision = TodoTimePrecision::Date;
                }
            }
            let item = service.task_store.create(&owner, draft).unwrap();
            if matches!(combination, ReplayCombination::CompletedRecurring)
                || matches!(combination, ReplayCombination::AllOneOff) && index > 6
            {
                service.task_store.complete(&owner, &item.id).unwrap();
            }
            expected_ids.push(item.id);
        }

        let interference_title = format!("{} 干扰项", combination.name());
        let mut interference = todo_draft(interference_title.clone());
        match combination {
            ReplayCombination::PendingDateRecurring => {
                interference.due_date = Some(target_date.clone());
                interference.time_precision = TodoTimePrecision::Date;
            }
            ReplayCombination::CompletedRecurring => {
                let item = service.task_store.create(&owner, interference).unwrap();
                service.task_store.complete(&owner, &item.id).unwrap();
                assert_combination_replay(
                    &service,
                    &inspector,
                    combination,
                    &expected_ids,
                    &interference_title,
                )
                .await;
                continue;
            }
            ReplayCombination::AllOneOff | ReplayCombination::PendingKeywordRecurring => {
                interference.due_date = Some(target_date.clone());
                interference.time_precision = TodoTimePrecision::Date;
                make_draft_recurring(&mut interference);
            }
            ReplayCombination::PendingOverdueOneOff => {
                interference.due_date = Some(future_date.clone());
                interference.time_precision = TodoTimePrecision::Date;
            }
        }
        service.task_store.create(&owner, interference).unwrap();
        assert_combination_replay(
            &service,
            &inspector,
            combination,
            &expected_ids,
            &interference_title,
        )
        .await;
    }
}

#[tokio::test]
async fn todo_retry_keeps_replay_context_on_final_truncated_list_result() {
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let target_date = (today + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let arguments = serde_json::json!({
        "status": "pending",
        "due_date": target_date,
        "date_range_text": null,
        "time_filter": null,
        "keyword": null,
        "recurring": true
    })
    .to_string();
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_todo_list_retry(arguments, "查询完成");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let mut matching_ids = Vec::new();

    for index in 1..=11 {
        let mut draft = todo_draft(format!("周期查询匹配项 {index}"));
        draft.due_date = Some(target_date.clone());
        draft.time_precision = TodoTimePrecision::Date;
        make_draft_recurring(&mut draft);
        matching_ids.push(service.task_store.create(&owner, draft).unwrap().id);
    }
    let mut interference = todo_draft("周期查询一次性干扰项");
    interference.due_date = Some(target_date);
    interference.time_precision = TodoTimePrecision::Date;
    service.task_store.create(&owner, interference).unwrap();

    let collapsed = service
        .respond(private_message("查询明天的周期性待办"))
        .await
        .unwrap();
    let collapsed_text = collapsed.text.unwrap();
    assert!(collapsed_text.contains("周期查询匹配项 1"));
    assert!(collapsed_text.contains("周期查询匹配项 10"));
    assert!(!collapsed_text.contains("周期查询匹配项 11"));
    assert!(!collapsed_text.contains("周期查询一次性干扰项"));
    assert!(!collapsed_text.contains("旧失败结果不应展示"));

    let snapshot = last_todo_snapshot(&service, "retry collapsed");
    assert_eq!(snapshot.result_ids.len(), 10);
    assert!(
        snapshot
            .result_ids
            .iter()
            .all(|id| matching_ids.contains(id))
    );
    assert_eq!(
        snapshot
            .replay_context
            .as_ref()
            .and_then(|value| value.get("recurring"))
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        active_private_session(&service)
            .extra
            .get("tool_todo_task_query_history"),
        None
    );

    let full = service
        .respond(private_message("查看完整结果"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();
    assert!(full_text.contains("周期查询匹配项 11"));
    assert!(!full_text.contains("周期查询一次性干扰项"));
    assert_eq!(
        last_todo_snapshot(&service, "retry full").result_ids.len(),
        11
    );
    assert_eq!(inspector.tool_call_count(), 1);
}

fn make_draft_recurring(draft: &mut TodoItemDraft) {
    draft.recurrence_kind = TodoRecurrenceKind::Daily;
    draft.recurrence_interval_days = 1;
    draft.recurrence_interval = 1;
    draft.recurrence_unit = TodoRecurrenceUnit::Day;
}

async fn assert_combination_replay(
    service: &RustRespondService,
    inspector: &MockProvider,
    combination: ReplayCombination,
    expected_ids: &[String],
    interference_title: &str,
) {
    let collapsed = service
        .respond(private_message(&format!("查询 {}", combination.name())))
        .await
        .unwrap();
    let collapsed_text = collapsed.text.unwrap();
    assert!(
        !collapsed_text.contains(interference_title),
        "{} collapsed result contains interference: {collapsed_text}",
        combination.name()
    );
    let collapsed_snapshot = last_todo_snapshot(service, "collapsed combination");
    let replay_context = collapsed_snapshot
        .replay_context
        .as_ref()
        .expect("structured replay context");
    let expected_recurring = match combination {
        ReplayCombination::PendingDateRecurring
        | ReplayCombination::CompletedRecurring
        | ReplayCombination::PendingKeywordRecurring => Some(true),
        ReplayCombination::AllOneOff | ReplayCombination::PendingOverdueOneOff => Some(false),
    };
    assert_eq!(
        replay_context
            .get("recurring")
            .and_then(serde_json::Value::as_bool),
        expected_recurring,
        "{} replay context: {replay_context}",
        combination.name()
    );
    let replayed = crate::runtime::tools::todo::replay_todo_query(&collapsed_snapshot)
        .unwrap_or_else(|| panic!("{} replay context should decode", combination.name()));
    assert_eq!(replayed.recurring, expected_recurring);
    assert!(
        active_private_session(service)
            .extra
            .get("tool_todo_task_query_history")
            .is_none(),
        "{} pending query context should be consumed by receipt",
        combination.name()
    );
    assert_eq!(collapsed_snapshot.result_ids.len(), 10);
    assert!(
        collapsed_snapshot
            .result_ids
            .iter()
            .all(|id| expected_ids.contains(id))
    );

    let full = service
        .respond(private_message("查看完整结果"))
        .await
        .unwrap();
    let full_text = full.text.unwrap();
    assert!(
        !full_text.contains(interference_title),
        "{} full result contains interference: {full_text}",
        combination.name()
    );
    let full_snapshot = last_todo_snapshot(service, "full combination");
    assert!(full_snapshot.replay_context.is_some());
    assert_eq!(full_snapshot.result_ids.len(), 11);
    assert_eq!(
        full_snapshot.result_ids.into_iter().collect::<HashSet<_>>(),
        expected_ids.iter().cloned().collect::<HashSet<_>>()
    );
    assert_eq!(inspector.tool_call_count(), 1);
}
