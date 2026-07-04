use std::fs;

use serde_json::Value;

use crate::provider::{ToolCallingProtocol, ToolExecutionResult, types::ChatRole};

use super::{
    super::{
        RespondRequest,
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT, empty_respond_request,
        },
    },
    support::*,
};
use crate::runtime::session::SessionMeta;
use crate::runtime::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};
use crate::runtime::{
    memory::{CreateScopedMemoryRequest, MemoryScopeType},
    pending::PendingOperation,
    rss::{RssFeedItem, RssTarget, RssTargetType},
};

fn raw_tool_result(name: &str, output: serde_json::Value, succeeded: bool) -> ToolExecutionResult {
    ToolExecutionResult {
        name: name.to_owned(),
        output,
        succeeded,
    }
}

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
async fn private_weather_chat_with_openai_responses_capability_enters_tool_loop() {
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
    assert!(tool_request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System
            && message.content.contains("存在歧义")
            && message.content.contains("不要调用写工具")
    }));
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(true)
    );
}

#[tokio::test]
async fn private_general_chat_with_tool_capability_uses_plain_chat() {
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
            .contains("回复：聊聊 Rust 的所有权")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_calling_enabled"],
        serde_json::json!(false)
    );
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
                reminder_at: None,
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
async fn private_tool_loop_can_query_train_schedule_with_trusted_rendering() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_train_schedule",
            r#"{"train_code":"g1","travel_date":"2026-06-28"}"#,
            "这趟车早上发车，适合当天安排。",
        );
    let train = MockTrainExecutor::new();
    let train_inspector = train.clone();
    let (service, _) = test_service_with_provider_base_title_query_weather_train_models_and_options(
        inspector.clone(),
        None,
        std::sync::Arc::new(MockQueryExecutor),
        std::sync::Arc::new(MockWeatherExecutor::new()),
        std::sync::Arc::new(train),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
        TestToolCallingOptions {
            enabled: true,
            group_enabled: false,
        },
    );

    let response = service
        .respond(private_message("查一下 G1 时刻"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    let requests = train_inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].train_code, "G1");
    assert_eq!(
        requests[0].travel_date,
        chrono::NaiveDate::from_ymd_opt(2026, 6, 28).unwrap()
    );
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("G1 列车时刻"));
    assert!(text.contains("北京南"));
    assert!(text.contains("上海虹桥"));
    assert!(text.contains("这趟车早上发车，适合当天安排。"));
    assert_eq!(response.command.as_deref(), Some("train"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["get_train_schedule"])
    );
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
}

#[tokio::test]
async fn mixed_train_and_todo_request_is_not_captured_by_todo_date_query() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                (
                    "get_train_schedule",
                    r#"{"train_code":"g1","travel_date":"明天"}"#,
                ),
                (
                    "create_todo",
                    r#"{"content":"查看明天 G1 车次","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "G1 可查，已新增待办",
        );
    let train = MockTrainExecutor::new();
    let train_inspector = train.clone();
    let (service, _) = test_service_with_provider_base_title_query_weather_train_models_and_options(
        inspector.clone(),
        None,
        std::sync::Arc::new(MockQueryExecutor),
        std::sync::Arc::new(MockWeatherExecutor::new()),
        std::sync::Arc::new(train),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
        TestToolCallingOptions {
            enabled: true,
            group_enabled: false,
        },
    );

    let response = service
        .respond(private_message(
            "明天有没有g1，我想看看，如果有车，我要加个待办，是上海到北京么",
        ))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    assert_ne!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.as_deref().unwrap();
    assert!(!text.contains("这一天暂无未完成待办"));
    assert!(text.contains("G1 列车时刻"));
    assert!(text.contains("✅ 已新增待办"));
    let train_requests = train_inspector.requests();
    assert_eq!(train_requests.len(), 1);
    assert_eq!(train_requests[0].train_code, "G1");
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "查看明天 G1 车次");
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["get_train_schedule", "create_todo"])
    );
}

#[tokio::test]
async fn conditional_external_query_is_not_captured_by_todo_date_query() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("需要先确认具体车次或可查询来源。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("明天上海到北京有高铁吗，有的话提醒我"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    assert_ne!(response.command.as_deref(), Some("todo_due_date"));
    assert!(
        !response
            .text
            .as_deref()
            .unwrap()
            .contains("这一天暂无未完成待办")
    );
}

#[tokio::test]
async fn private_tool_loop_can_query_recent_rss_items_with_trusted_rendering() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_rss_recent_items",
            r#"{"query":"codex","limit":1}"#,
            "这条更新主要值得关注工具调用改进。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let target = RssTarget {
        target_type: RssTargetType::Private,
        target_id: "u1".to_owned(),
        scope_key: "private:u1".to_owned(),
    };
    let subscription = service
        .rss_store
        .create_subscription(
            &target,
            "https://example.test/codex.xml",
            "Codex 发布",
            &[],
            500,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &subscription.id,
            &[RssFeedItem {
                item_key: "codex-release-1".to_owned(),
                revision_hash: "rev:codex-release-1".to_owned(),
                title: "Codex CLI 0.42 发布".to_owned(),
                link: Some("https://example.test/codex-release-1".to_owned()),
                published_at: Some("2026-06-18T00:00:00+00:00".to_owned()),
                updated_at: Some("2026-06-18T01:00:00+00:00".to_owned()),
                summary: Some("这次发布改进了工具调用和日志展示。".to_owned()),
                source_order: 0,
            }],
            500,
        )
        .unwrap();

    let response = service
        .respond(private_message("查看上次 codex 发布的 rss"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("RSS 最近记录"));
    assert!(text.contains("Codex 发布"));
    assert!(text.contains("Codex CLI 0.42 发布"));
    assert!(text.contains("这次发布改进了工具调用和日志展示"));
    assert!(text.contains("本地已轮询入库记录"));
    assert!(text.contains("这条更新主要值得关注工具调用改进。"));
    assert_eq!(response.command.as_deref(), Some("rss"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["get_rss_recent_items"])
    );
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
}

#[tokio::test]
async fn group_tool_loop_exposes_query_only_tools_when_enabled() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_group_tool_calling(inspector.clone(), true, true);
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "群里个人待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let response = service
        .respond(group_message("杭州天气和最近 RSS 有什么"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    let tool_request = inspector.tool_requests().remove(0);
    assert_eq!(tool_request.tool_context.user_id.as_deref(), Some("u1"));
    assert_eq!(tool_request.tool_context.scope_id, "group:g1");
    assert!(tool_request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System
            && message
                .content
                .contains("群聊只允许调用本场景配置白名单中的工具")
            && message.content.contains("不要声称已经执行未开放的工具")
    }));

    let mut tool_names = tool_request
        .tools
        .metadata()
        .into_iter()
        .map(|metadata| metadata.name)
        .collect::<Vec<_>>();
    tool_names.sort();
    assert_eq!(
        tool_names,
        vec!["get_rss_recent_items", "get_train_schedule", "get_weather",]
    );

    let list_err = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending","due_date":null}"#,
        )
        .await
        .unwrap_err();
    assert_eq!(list_err.code, "tool_not_found");
    let create_err = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "create_todo",
            r#"{"content":"不应写入","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
        )
        .await
        .unwrap_err();
    assert_eq!(create_err.code, "tool_not_found");
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_calling_enabled"], serde_json::json!(true));
    assert_eq!(
        diagnostics["tool_loop_mode"],
        serde_json::json!("configured_whitelist")
    );
    assert_eq!(
        diagnostics["tool_loop_enabled_tools"],
        serde_json::json!(["get_weather", "get_train_schedule", "get_rss_recent_items"])
    );
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
    assert_eq!(created["ok"], true);
    assert_eq!(created["created"]["title"], "今晚检查机器人日志");
    assert!(created.get("requires_confirmation").is_none());
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
    assert_eq!(cancel["ok"], true);
    assert_eq!(cancel["cancelled"][0]["visible_number"], 1);
    assert!(cancel["missing_numbers"].as_array().unwrap().is_empty());
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
async fn restore_then_cancel_last_reference_executes_without_relisting() {
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
                reminder_at: None,
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
    assert_eq!(cancel["cancelled"][0]["reference"], "last");

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(
        service
            .todo_store
            .list_all(&owner)
            .unwrap()
            .into_iter()
            .find(|item| item.title == "恢复后继续取消")
            .unwrap()
            .status,
        TodoStatus::Cancelled
    );
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
                reminder_at: None,
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
                    reminder_at: None,
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
                reminder_at: None,
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
async fn tool_loop_created_todo_survives_chat_history_save_and_records_last_action() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_create_todo_tool_call("今晚检查机器人日志");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let first = service
        .respond(private_message("帮我记一个待办，今晚检查机器人日志"))
        .await
        .unwrap();
    let first_text = first.text.unwrap();
    assert!(first_text.contains("✅ 已新增待办"));
    assert!(first_text.contains("🚧 当前进行中 · 共 1 项"));
    let first_diagnostics = first.diagnostics.unwrap();
    assert_eq!(first_diagnostics["todo_success_claimed"], true);
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
    assert!(session.pending_operation.is_none());
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].raw_text.as_deref(), Some("今晚检查机器人日志"));
    assert_eq!(
        session
            .last_todo_query
            .as_ref()
            .expect("missing refreshed todo query")
            .result_ids,
        vec![todos[0].id.clone()]
    );
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.item_id, todos[0].id);
    assert_eq!(last_action.action, "created");
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

    assert_eq!(response.command.as_deref(), Some("todo_create"));
    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("明天接老公"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["create_todo"])
    );
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"][0]["effect"], "created");
    assert_eq!(diagnostics["todo_success_verified"], true);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].raw_text.as_deref(), Some("明天接老公"));
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.item_id, todos[0].id);
    assert_eq!(last_action.action, "created");
    let snapshot = session
        .last_todo_query
        .expect("missing refreshed todo snapshot");
    assert_eq!(snapshot.result_ids, vec![todos[0].id.clone()]);
    assert_eq!(inspector.tool_call_count(), 1);
}

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
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "新增后保留");
}

#[tokio::test]
async fn multiple_successful_todo_writes_share_one_related_list() {
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
    assert_eq!(text.matches("🚧 当前进行中").count(), 1);
    assert!(text.contains("第一条新增"));
    assert!(text.contains("第二条新增"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"].as_array().unwrap().len(), 3);
    let todos = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 2);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing final related list snapshot");
    assert_eq!(snapshot.result_ids.len(), 2);
}

#[tokio::test]
async fn weather_success_and_todo_success_are_both_rendered_in_order() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "create_todo",
                    r#"{"content":"出门带伞","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "杭州小雨，已新增带伞待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查一下杭州天气，顺便加一个带伞待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    let weather_pos = text.find("杭州天气").expect("missing weather fact card");
    let todo_pos = text.find("✅ 已新增待办").expect("missing todo receipt");
    assert!(weather_pos < todo_pos);
    assert!(text.contains("当前 20:15"));
    assert!(text.contains("出门带伞"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["error_code"], Value::Null);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
    assert_eq!(diagnostics["tool_outcomes"][1]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn readonly_weather_result_preserves_model_advice() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_weather",
            r#"{"city":"杭州","forecast_days":3}"#,
            "湿度偏高，户外运动建议降低强度，优先选清晨或室内。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州天气怎么样，是不是要运动"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("湿度偏高，户外运动建议降低强度"));
    assert_eq!(response.command.as_deref(), Some("weather"));
}

#[tokio::test]
async fn conditional_weather_and_todo_request_uses_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "create_todo",
                    r#"{"content":"明天带伞","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "明天可能有雨，已新增带伞待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("如果明天下雨，帮我加个带伞的待办"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    assert_ne!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.as_deref().unwrap();
    assert!(!text.contains("这一天暂无未完成待办"));
    assert!(text.contains("杭州天气"));
    assert!(text.contains("✅ 已新增待办"));
}

#[tokio::test]
async fn weather_success_and_todo_failure_keep_fact_and_error() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![
                ("get_weather", r#"{"city":"杭州","forecast_days":3}"#),
                (
                    "edit_todo",
                    r#"{"number":99,"reference":null,"raw_text":"带伞","title":"带伞","detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
                ),
            ],
            "杭州天气已查，待办已修改",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查杭州天气，再把不存在的待办改成带伞"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("我现在没有可用的待办列表编号"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(
        diagnostics["error_code"],
        "todo_visible_numbers_unavailable"
    );
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], false);
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(
        diagnostics["tool_outcomes"][1]["status"],
        "requires_clarification"
    );
}

#[tokio::test]
async fn weather_failure_and_todo_success_keep_error_and_side_effect() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "get_weather",
                    serde_json::json!({
                        "ok": false,
                        "error": {
                            "code": "timeout",
                            "message": "upstream timed out",
                            "stage": "tool"
                        }
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "出门带伞",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "已新增待办",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("查杭州天气，顺便加带伞待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("天气服务超时了"));
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("出门带伞"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["error_code"], "timeout");
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn weather_failure_and_dependency_skipped_todo_keep_root_cause() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "get_weather",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "not_found",
                        "message": "city not found"
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
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
        .respond(private_message("查不存在城市天气后新增待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没找到这个城市"));
    assert!(text.contains("前序工具没有成功"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "failed");
    assert_eq!(diagnostics["error_code"], "not_found");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "failed");
    assert_eq!(diagnostics["tool_outcomes"][1]["status"], "skipped");
}

#[tokio::test]
async fn unadapted_success_with_todo_success_is_not_silently_dropped() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "unknown_tool",
                    serde_json::json!({
                        "ok": true,
                        "summary": "未知工具成功"
                    }),
                    true,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "确认副作用",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "未知工具成功，待办也已新增",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("新增待办并执行两个工具"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("确认副作用"));
    assert!(text.contains("部分工具结果未生成确定性展示"));
    assert!(text.contains("unknown_tool"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["error_code"], "tool_outcome_unhandled");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "unhandled");
    assert_eq!(diagnostics["tool_outcomes"][1]["presentation"], "trusted");
}

#[tokio::test]
async fn unadapted_failure_with_todo_success_is_user_visible() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![
                raw_tool_result(
                    "unknown_tool",
                    serde_json::json!({
                        "ok": false,
                        "error_code": "unknown_failed",
                        "message": "internal detail should not be rendered"
                    }),
                    false,
                ),
                raw_tool_result(
                    "create_todo",
                    serde_json::json!({
                        "ok": true,
                        "created": {
                            "title": "仍然新增成功",
                            "detail": null,
                            "display_time": "无时间"
                        }
                    }),
                    true,
                ),
            ],
            "待办成功",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("执行未知工具并新增待办"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("仍然新增成功"));
    assert!(text.contains("unknown_tool"));
    assert!(text.contains("执行失败，当前没有可信错误展示适配器"));
    assert!(!text.contains("internal detail should not be rendered"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "partial_success");
    assert_eq!(diagnostics["error_code"], "unknown_failed");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "unhandled");
}

#[tokio::test]
async fn only_weather_tool_renders_fact_card() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "get_weather",
            r#"{"city":"杭州","forecast_days":3}"#,
            "杭州天气如下",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(private_message("杭州天气")).await.unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("杭州天气"));
    assert!(text.contains("未来 3 天"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["todo_tool_results"], serde_json::json!([]));
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "weather");
    assert_eq!(diagnostics["tool_outcomes"][0]["presentation"], "trusted");
}

#[tokio::test]
async fn only_list_todos_success_does_not_claim_todo_write_success() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json("list_todos", r#"{"status":"pending"}"#, "当前待办列表");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
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
            },
        )
        .unwrap();

    let response = service
        .respond(private_message("检查待办状态"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_outcomes"][0]["domain"], "todo");
    assert_eq!(diagnostics["tool_outcomes"][0]["effect"], "read_only");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
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
        .todo_store
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
            },
        )
        .unwrap();
    let today = service
        .todo_store
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
            },
        )
        .unwrap();
    service
        .todo_store
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
        .todo_store
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
    // 修改回执和自动刷新的当前列表都应展示详情，避免用户误以为详情没有写入。
    assert!(text.contains("   提前确认地址"));
    assert!(!text.contains("旧详情"));
    assert!(!text.contains("created_at"));
    assert_eq!(
        service.todo_store.list_pending(&owner).unwrap()[0]
            .detail
            .as_deref(),
        Some("提前确认地址")
    );
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
        .todo_store
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
async fn todo_cancel_pending_item_executes_without_confirmation() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "cancel_todo",
            r#"{"number":2,"reference":null}"#,
            "已取消待办",
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
                reminder_at: None,
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
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(private_message("/todo")).await.unwrap();
    let response = service
        .respond(private_message("删除第二条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_cancel"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("⛔ 已取消待办"));
    assert!(
        text.contains("第二条待取消"),
        "response should mention cancelled item: {text}"
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
    assert_eq!(diagnostics["tool_outcomes"][0]["status"], "succeeded");
    assert_eq!(diagnostics["todo_success_claimed"], true);
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(diagnostics["tool_retry_count"], 0);
    assert_eq!(diagnostics["error_code"], Value::Null);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(
        service
            .todo_store
            .list_all(&owner)
            .unwrap()
            .into_iter()
            .find(|item| item.title == "第二条待取消")
            .unwrap()
            .status,
        TodoStatus::Cancelled
    );
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
        .todo_store
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
            },
        )
        .unwrap();
    service
        .todo_store
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
    assert!(text.contains("🚧 当前进行中 · 共 1 项"));
    assert!(!text.contains("先完成\n状态：未完成"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["list_todos", "complete_todos"])
    );
    let outcomes = diagnostics["tool_outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["tool"], "complete_todos");
    assert_eq!(outcomes[1]["tool"], "todo_related_list");
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing visible snapshot");
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
        .todo_store
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
            },
        )
        .unwrap();
    service
        .todo_store
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
        diagnostics["tool_loop_executed_tools"],
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
        .todo_store
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
        .todo_store
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
    assert!(service.todo_store.list_pending(&owner).unwrap().len() == 1);
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete {
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
        .todo_store
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
        Some(PendingOperation::TodoDelete { item, .. }) => {
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
        .todo_store
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
    assert!(text.contains("确认删除以下 1 项待办吗"));
    assert!(text.contains("删除后不可恢复"));
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
        Some(PendingOperation::TodoDelete { .. })
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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

    for input in ["查看完成的待办", "看看做完的任务"] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_done"), "{input}");
        assert!(!text.contains("未完成条目"), "{input}");
        assert!(text.contains("已完成条目"), "{input}");
        assert!(!text.contains("已取消条目"), "{input}");
    }

    for input in [
        "查看取消的待办",
        "看看取消的任务",
        "查询已取消待办",
        "列出取消列表",
        "查看被取消的待办",
    ] {
        let response = service.respond(private_message(input)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(
            response.command.as_deref(),
            Some("todo_cancelled_list"),
            "{input}"
        );
        assert!(!text.contains("未完成条目"), "{input}");
        assert!(!text.contains("已完成条目"), "{input}");
        assert!(text.contains("已取消条目"), "{input}");
    }

    assert_eq!(pending.status, TodoStatus::Pending);
    assert!(inspector.requests().is_empty());
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn todo_completed_and_cancelled_lists_use_dynamic_collapse_hints() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=9 {
        let completed = service
            .todo_store
            .create(&owner, todo_draft(format!("已完成 {index}")))
            .unwrap();
        service.todo_store.complete(&owner, &completed.id).unwrap();
        let cancelled = service
            .todo_store
            .create(&owner, todo_draft(format!("已取消 {index}")))
            .unwrap();
        service.todo_store.cancel(&owner, &cancelled.id).unwrap();
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

    let cancelled = service
        .respond(private_message("查看已取消待办"))
        .await
        .unwrap();
    let cancelled_text = cancelled.text.unwrap();
    assert!(cancelled_text.contains("⛔ 已取消 · 共 9 项"));
    assert!(cancelled_text.contains("还有 4 项已取消待办，可说“查看全部已取消待办”。"));

    let cancelled_full = service
        .respond(private_message("查看全部已取消待办"))
        .await
        .unwrap();
    let cancelled_full_text = cancelled_full.text.unwrap();
    assert!(!cancelled_full_text.contains("还有 4 项已取消待办"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .expect("missing full cancelled snapshot");
    assert_eq!(snapshot.query_type, "cancelled-list");
    assert_eq!(snapshot.result_ids.len(), 9);
}

#[tokio::test]
async fn todo_date_filter_collapse_hint_restores_full_result_scope() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=9 {
        let item = service
            .todo_store
            .create(&owner, todo_draft(format!("今天完成 {index}")))
            .unwrap();
        service.todo_store.complete(&owner, &item.id).unwrap();
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
            .todo_store
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
            .todo_store
            .create(&owner, todo_draft(format!("进行中待办 {index}")))
            .unwrap();
    }
    for index in 1..=2 {
        let item = service
            .todo_store
            .create(&owner, todo_draft(format!("已完成待办 {index}")))
            .unwrap();
        service.todo_store.complete(&owner, &item.id).unwrap();
    }
    for index in 1..=2 {
        let item = service
            .todo_store
            .create(&owner, todo_draft(format!("已取消待办 {index}")))
            .unwrap();
        service.todo_store.cancel(&owner, &item.id).unwrap();
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
    assert!(full_text.contains("📋 全部待办 · 共 10 项"));
    assert!(full_text.contains("进行中待办 6"));
    assert!(full_text.contains("已完成待办 1"));
    assert!(full_text.contains("已取消待办 1"));
    assert!(!full_text.contains("还有 5 项待办"));
    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn negated_cancelled_query_phrases_do_not_list_cancelled_items() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let cancelled = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "不应展示的已取消条目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, &cancelled.id).unwrap();

    for input in ["查看未取消的待办", "查看没取消的待办"] {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_ne!(
            response.command.as_deref(),
            Some("todo_cancelled_list"),
            "{input}"
        );
    }
}

#[tokio::test]
async fn todo_write_or_question_phrases_do_not_enter_natural_query_path() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);

    for input in ["取消这个待办", "怎么取消待办", "帮我取消第一条", "不做了"]
    {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_ne!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert_ne!(
            response.command.as_deref(),
            Some("todo_cancelled_list"),
            "{input}"
        );
    }
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
                reminder_at: None,
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
                reminder_at: None,
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
async fn deterministic_date_query_then_tool_loop_complete_first_uses_date_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let today = crate::util::time_context::request_time_context()
        .local_date()
        .format("%Y-%m-%d")
        .to_string();
    let today_item = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "今天要完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some(today.clone()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    let no_time = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "无时间待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let listed = service
        .respond(private_message("查看今天待办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_due_date"));
    let listed_text = listed.text.unwrap();
    assert!(listed_text.contains("1. 今天要完成"));
    assert!(!listed_text.contains("无时间待办"));

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing date snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, today);
    assert_eq!(snapshot.result_ids, vec![today_item.id.clone()]);

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
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
    assert_eq!(completed["completed"][0]["title"], "今天要完成");

    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &today_item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &no_time.id)
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
                reminder_at: None,
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
                reminder_at: None,
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
async fn todo_complete_receipt_refreshes_pending_list_and_collapses_one_hidden_item() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=7 {
        service
            .todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: format!("待办 {index}"),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
    }

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("✅ 已完成待办 · 1条"));
    assert!(text.contains("🚧 当前进行中 · 共 6 项"));
    assert!(text.contains("还有 1 项进行中待办，可说“查看全部进行中待办”。"));
    let remaining = service.todo_store.list_pending(&owner).unwrap();
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing refreshed snapshot");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(
        snapshot.result_ids,
        remaining
            .iter()
            .take(5)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.result_ids.len(), 5);
}

#[tokio::test]
async fn todo_complete_receipt_collapses_two_hidden_items() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=8 {
        service
            .todo_store
            .create(&owner, todo_draft(format!("待办 {index}")))
            .unwrap();
    }

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("🚧 当前进行中 · 共 7 项"));
    assert!(text.contains("还有 2 项进行中待办，可说“查看全部进行中待办”。"));
    let remaining = service.todo_store.list_pending(&owner).unwrap();
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing refreshed snapshot");
    assert_eq!(
        snapshot.result_ids,
        remaining
            .iter()
            .take(5)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.result_ids.len(), 5);
}

#[tokio::test]
async fn todo_complete_receipt_uses_dynamic_pending_collapse_hint() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=9 {
        service
            .todo_store
            .create(&owner, todo_draft(format!("待办 {index}")))
            .unwrap();
    }

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    let response = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("🚧 当前进行中 · 共 8 项"));
    assert!(text.contains("还有 3 项进行中待办，可说“查看全部进行中待办”。"));
    let remaining = service.todo_store.list_pending(&owner).unwrap();
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing refreshed snapshot");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(
        snapshot.result_ids,
        remaining
            .iter()
            .take(5)
            .map(|item| item.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.result_ids.len(), 5);
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
async fn cancelled_query_then_delete_all_creates_bulk_pending_and_confirm_deletes_only_cancelled() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":null,"reference":null,"query":null,"all_status":"cancelled"}"#,
            "已发起删除已取消待办确认",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let pending_a = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let cancelled_a = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let cancelled_b = service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, &cancelled_a.id).unwrap();
    service.todo_store.cancel(&owner, &cancelled_b.id).unwrap();

    let listed = service
        .respond(private_message("查看取消的待办"))
        .await
        .unwrap();
    let listed_text = listed.text.unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_cancelled_list"));
    assert!(listed_text.contains("⛔ 已取消 · 共 2 项"));
    assert!(listed_text.contains("已取消 A"));
    assert!(listed_text.contains("已取消 B"));
    assert!(!listed_text.contains("进行中 A"));
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing cancelled snapshot");
    let expected_cancelled_ids = service
        .todo_store
        .list_cancelled(&owner)
        .unwrap()
        .into_iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    assert_eq!(snapshot.query_type, "cancelled-list");
    assert_eq!(snapshot.condition, "已取消列表");
    assert_eq!(snapshot.result_ids, expected_cancelled_ids);

    let delete = service
        .respond(private_message("把这些已取消待办都删除了吧"))
        .await
        .unwrap();
    assert!(
        delete
            .text
            .as_deref()
            .unwrap()
            .contains("确认删除以下 2 项待办吗")
    );
    let diagnostics = delete.diagnostics.unwrap();
    assert_eq!(
        diagnostics["tool_loop_executed_tools"],
        serde_json::json!(["delete_todos"])
    );
    assert_eq!(diagnostics["todo_success_verified"], true);
    assert_eq!(
        diagnostics["todo_tool_results"][0]["requires_confirmation"],
        true
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(status, TodoStatus::Cancelled);
            assert_eq!(item_ids.len(), 2);
            assert!(item_ids.contains(&cancelled_a.id));
            assert!(item_ids.contains(&cancelled_b.id));
            assert!(!item_ids.contains(&pending_a.id));
        }
        other => panic!("expected TodoBulkDelete pending operation, got {other:?}"),
    }
    assert_eq!(service.todo_store.list_cancelled(&owner).unwrap().len(), 2);
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);

    let confirmed = service.respond(private_message("确认")).await.unwrap();
    assert!(confirmed.text.as_deref().unwrap().contains("已永久删除"));
    assert!(
        service
            .todo_store
            .list_cancelled(&owner)
            .unwrap()
            .is_empty()
    );
    let pending = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, pending_a.id);
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
                reminder_at: None,
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
    // 不确定的私聊文本默认走普通聊天；不能仅因 provider 支持工具而进入 Tool Loop。
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
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
                reminder_at: None,
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
async fn chat_does_not_inject_member_id_mapping_or_speaker_hint() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());

    let response = service.respond(message("我是407，继续")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    let requests = inspector.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.messages.iter().all(|message| {
                !message.content.contains("成员编号映射来自外部配置文件")
                    && !message.content.contains("本轮用户消息命中了已知成员编号")
            }))
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(
        session.history.last().map(|item| item.content.as_str()),
        Some("回复：我是407，继续")
    );
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

fn todo_draft(title: impl Into<String>) -> TodoItemDraft {
    TodoItemDraft {
        title: title.into(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        reminder_at: None,
        time_precision: TodoTimePrecision::None,
    }
}

fn group_message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "group:g1".to_owned(),
        user_id: Some("u1".to_owned()),
        group_id: Some("g1".to_owned()),
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
