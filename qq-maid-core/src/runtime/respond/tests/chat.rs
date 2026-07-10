use std::fs;

use qq_maid_llm::provider::{ToolCallingProtocol, ToolExecutionResult, types::ChatRole};
use serde_json::Value;

use crate::runtime::respond::{
    PlannedRespond, RespondPlan,
    agent_route::{RespondRoute, SemanticRoute, ToolDomain},
    status_hint::{StatusAction, StatusHint, StatusSubject},
};

use super::{
    super::{
        RespondRequest,
        command_dispatcher::{CommandDispatcher, DispatchOutcome},
        common::empty_respond_request,
    },
    support::*,
};
use crate::runtime::session::SessionMeta;
use crate::runtime::tools::todo::{
    TodoItemDraft, TodoPendingOperation, TodoStatus, TodoStore, TodoTimePrecision,
};
use crate::runtime::{
    pending::PendingOperation,
    rss::{RssFeedItem, RssTarget, RssTargetType},
};

fn todo_pending(pending: Option<&PendingOperation>) -> Option<TodoPendingOperation> {
    pending.and_then(|pending| {
        TodoPendingOperation::try_from_pending(pending)
            .ok()
            .flatten()
    })
}

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
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "agent_chat");
    assert_eq!(diagnostics["route_reason"], "semantic_tool_intent");
    assert_eq!(diagnostics["route_domains"], serde_json::json!(["weather"]));
    assert_eq!(diagnostics["route_semantic"], "tool_intent");
    assert_eq!(diagnostics["tool_calling_enabled"], true);
}

#[tokio::test]
async fn private_general_chat_with_tool_capability_uses_agent_direct_answer() {
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
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "agent_chat");
    assert_eq!(diagnostics["route_reason"], "semantic_plain_chat");
    assert_eq!(diagnostics["route_domains"], serde_json::json!([]));
    assert_eq!(diagnostics["route_semantic"], "plain_chat");
    assert_eq!(diagnostics["tool_calling_enabled"], serde_json::json!(true));
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_calling_used"], false);
    assert_eq!(diagnostics["used_search"], false);
    assert_eq!(diagnostics["agent_result"], "direct_answer");
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
    assert_eq!(diagnostics["agent_model_rounds"], 1);
    assert_eq!(diagnostics["agent_streaming_fallback_used"], false);
    assert_eq!(diagnostics["agent_tool_results"], serde_json::json!([]));
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
}

#[tokio::test]
async fn rejected_web_search_call_is_not_reported_as_used_search() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_rejected_tool_call("web_search", "搜索参数无效。");
    let service = test_service_with_provider_and_tool_calling(inspector, true);

    let response = service
        .respond(private_message("尝试联网搜索"))
        .await
        .unwrap();

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_call_emitted"], true);
    assert_eq!(diagnostics["tool_execution_attempted"], true);
    assert_eq!(diagnostics["used_search"], false);
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
    assert_eq!(diagnostics["agent_result"], "rejected");
    assert_eq!(diagnostics["stop_reason"], "rejected");
    assert_eq!(diagnostics["agent_model_rounds"], 1);
}

#[tokio::test]
async fn router_decision_is_passed_unchanged_to_prepared_chat() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector, true);
    let req = private_message("杭州今天要带伞吗");
    let planned = service.plan_core_respond(&req).unwrap();
    let expected_route = planned.respond_route().unwrap();

    let outcome = CommandDispatcher::new(&service)
        .dispatch(req, planned)
        .await
        .unwrap();
    let DispatchOutcome::Chat(chat) = outcome else {
        panic!("expected prepared chat");
    };

    assert_eq!(chat.respond_route, expected_route);
    assert!(chat.respond_route.uses_agent_runtime());
}

#[tokio::test]
async fn streaming_chat_uses_planned_plain_route_without_reclassification() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);
    let planned = service
        .plan_core_respond(&private_message("聊聊 Rust 的所有权"))
        .unwrap();

    // 执行时故意换成会被 Router 判为天气 Tool Agent 的文本；流式入口必须继续使用
    // 已生成的 PlainChat decision，不能读取新状态或按文本重新分类。
    let response = service
        .respond_stream_with_plan(private_message("杭州今天要带伞吗"), planned, |_| {
            Box::pin(async { Ok(()) })
        })
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "plain_chat");
    assert_eq!(diagnostics["route_reason"], "agent_unavailable");
    assert_eq!(diagnostics["route_semantic"], "plain_chat");
}

#[tokio::test]
async fn private_chinese_greetings_and_emotion_use_agent_direct_answer() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    for input in ["晚上好", "下午好呀", "早上好", "我晚上有点累", "你下午在吗"]
    {
        let response = service.respond(private_message(input)).await.unwrap();
        assert!(
            response
                .text
                .as_deref()
                .unwrap()
                .contains(&format!("回复：{input}")),
            "{input}"
        );
    }

    assert_eq!(inspector.tool_call_count(), 5);
    assert_eq!(inspector.requests().len(), 0);
}

#[tokio::test]
async fn private_generation_and_explanation_requests_use_agent_direct_answer() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    for input in ["帮我写个文案", "解释一下这个问题", "刚刚没看到，再来一条"]
    {
        let response = service.respond(private_message(input)).await.unwrap();
        assert!(
            response
                .text
                .as_deref()
                .unwrap()
                .contains(&format!("回复：{input}")),
            "{input}"
        );
    }

    assert_eq!(inspector.tool_call_count(), 3);
    assert_eq!(inspector.requests().len(), 0);
}

#[tokio::test]
async fn non_todo_agent_direct_answers_with_success_markers_are_not_guarded() {
    let cases = [
        (
            "写一句以‘已完成’开头的通知",
            "已完成：本次维护工作顺利结束。",
        ),
        ("把这句话改成：已记录，后续处理", "已记录，后续处理"),
        (
            "解释‘已删除项目不可恢复’",
            "已删除项目不可恢复，表示删除操作无法撤销。",
        ),
    ];
    let mut inspector =
        MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    for (_, reply) in cases {
        inspector = inspector.with_tool_loop_reply_without_tool(reply);
    }
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    for (input, expected) in cases {
        let response = service.respond(private_message(input)).await.unwrap();
        assert_eq!(response.text.as_deref(), Some(expected), "{input}");
        let diagnostics = response.diagnostics.unwrap();
        assert_eq!(diagnostics["todo_success_claimed"], false, "{input}");
        assert_eq!(diagnostics["todo_success_verified"], true, "{input}");
        assert_ne!(
            diagnostics["error_code"], "todo_success_not_verified",
            "{input}"
        );
        assert_eq!(diagnostics["tool_call_emitted"], false, "{input}");
    }

    assert_eq!(inspector.tool_call_count(), 3);
}

#[tokio::test]
async fn private_strong_todo_reference_without_context_enters_tool_loop_and_clarifies() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1]}"#,
            "我需要先确认是哪一条待办。",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    assert!(
        response
            .text
            .as_deref()
            .is_some_and(|text| !text.is_empty())
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["complete_todos"])
    );

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "complete_todos");
            assert_eq!(
                request.error_code.as_str(),
                "todo_visible_numbers_unavailable"
            );
        }
        other => panic!("expected todo clarification pending, got {other:?}"),
    }
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
        std::sync::Arc::new(MockWebSearchExecutor),
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
            group_enabled_tools: None,
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
        diagnostics["agent_executed_tools"],
        serde_json::json!(["get_train_schedule"])
    );
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_calling_used"], true);
    assert_eq!(diagnostics["agent_result"], "tool_used");
    assert_eq!(diagnostics["agent_model_rounds"], 2);
    assert_eq!(
        diagnostics["agent_tool_results"][0]["name"],
        "get_train_schedule"
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
        std::sync::Arc::new(MockWebSearchExecutor),
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
            group_enabled_tools: None,
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
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "查看明天 G1 车次");
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
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
        diagnostics["agent_executed_tools"],
        serde_json::json!(["get_rss_recent_items"])
    );
    assert_eq!(diagnostics["agent_turn_status"], "succeeded");
}

#[tokio::test]
async fn group_tool_loop_exposes_rss_management_but_not_todo_when_enabled() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_group_tool_calling(inspector.clone(), true, true);
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
        vec![
            "get_rss_recent_items",
            "get_train_schedule",
            "get_weather",
            "manage_rss_subscriptions",
            "web_search",
        ]
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
    assert_eq!(service.task_store.list_pending(&owner).unwrap().len(), 1);

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["tool_calling_enabled"], serde_json::json!(true));
    assert_eq!(
        diagnostics["agent_mode"],
        serde_json::json!("configured_whitelist")
    );
    assert_eq!(
        diagnostics["agent_enabled_tools"],
        serde_json::json!([
            "get_weather",
            "get_train_schedule",
            "get_rss_recent_items",
            "manage_rss_subscriptions",
            "web_search"
        ])
    );
}

#[tokio::test]
async fn private_natural_search_intent_routes_to_agent_chat_with_search_semantics() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider, true);

    for input in ["联网查一下", "联网查询下今日 ai 新闻", "查一下今天 AI 新闻"]
    {
        let planned = service.plan_core_respond(&private_message(input)).unwrap();
        assert_eq!(planned, RespondPlan::AgentChat, "{input}");

        let decision = planned.respond_route().unwrap();
        assert_eq!(decision.route, RespondRoute::AgentChat, "{input}");
        assert_eq!(
            decision.semantic_route,
            SemanticRoute::ToolIntent,
            "{input}"
        );
        assert_eq!(decision.domain, ToolDomain::Search, "{input}");
        assert_eq!(
            decision.status_hint,
            Some(StatusHint::new(StatusSubject::Tool, StatusAction::Query)),
            "{input}"
        );
    }
}

#[tokio::test]
async fn private_pasted_text_processing_does_not_route_to_web_search_plan() {
    let service = test_service_with_provider_and_tool_calling(MockProvider::new(), true);
    let input = "\
Codex 分析结果：
- 自然语言查询被 route 到 WebSearch
- 工具返回：查询内容太长了，请压缩到 200 字以内再试。
- has_search_intent 命中了 search 关键词
人话说这个";

    let plan = service.plan_core_respond(&private_message(input)).unwrap();
    assert_eq!(plan, RespondPlan::StreamingChat);
}

#[tokio::test]
async fn private_explicit_search_command_routes_to_web_search_plan() {
    let service = test_service_with_provider_and_tool_calling(MockProvider::new(), true);

    let plan = service
        .plan_core_respond(&private_message("/查 台风巴威"))
        .unwrap();
    assert_eq!(plan, RespondPlan::WebSearch);
}

#[tokio::test]
async fn complete_multiple_items_clears_last_todo_action() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for title in ["批量一", "批量二"] {
        service
            .task_store
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
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
        .task_store
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    service
        .respond(private_message("恢复刚才完成的待办"))
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
            "restore_todos",
            r#"{"numbers":null,"reference":"last"}"#,
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
        .task_store
        .delete_completed_by_ids(&owner, std::slice::from_ref(&todo.id))
        .unwrap();

    let missing = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":null,"reference":"last"}"#,
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
        todo_pending(session.pending_operation.as_ref()),
        Some(TodoPendingOperation::TodoClarify { .. })
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
    assert!(!first_text.contains("🚧 当前进行中 · 共 1 项"));
    let first_diagnostics = first.diagnostics.unwrap();
    assert_eq!(first_diagnostics["todo_success_claimed"], true);
    assert_eq!(first_diagnostics["todo_success_verified"], true);
    assert_eq!(first_diagnostics["tool_retry_count"], 0);
    assert_eq!(
        first_diagnostics["agent_executed_tools"],
        serde_json::json!(["create_todo"])
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    let todos = service.task_store.list_pending(&owner).unwrap();
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
async fn private_scheduled_task_phrase_is_handled_by_agent_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_calls_json(
            vec![(
                "create_todo",
                r#"{"content":"下午检查发布清单","title":"检查发布清单","detail":null,"due_date":null,"due_at":null,"reminder_at":null,"time_precision":null}"#,
            )],
            "任务已处理",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("下午检查发布清单"))
        .await
        .unwrap();

    let text = response.text.as_deref().unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("检查发布清单"));
    assert!(text.contains("15:00"));
    assert!(!text.contains("下午检查发布清单 · 时间"));
    assert_eq!(inspector.tool_call_count(), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["create_todo"])
    );
}

#[tokio::test]
async fn private_todo_create_phrase_is_handled_by_agent_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_create_todo_tool_call("明天接老公");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let req = private_message("新增待办，明天接老公");
    let planned = service.plan_core_respond(&req).unwrap();
    assert_eq!(planned, RespondPlan::AgentChat);
    assert!(planned.respond_route().unwrap().uses_agent_runtime());
    let response = service.respond_with_plan(req, planned).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_create"));
    let text = response.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("明天接老公"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
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
    let todos = service.task_store.list_pending(&owner).unwrap();
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
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "plain_chat");
    assert_eq!(diagnostics["route_reason"], "group_agent_disabled");
    assert_eq!(diagnostics["route_domains"], serde_json::json!([]));
    assert_eq!(diagnostics["tool_calling_enabled"], false);
}

#[tokio::test]
async fn slash_command_does_not_enter_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    service.respond(message("/天气 杭州")).await.unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn unknown_slash_command_falls_back_to_plain_chat_with_router_decision() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let req = private_message("/unknown-route-command");
    let planned = service.plan_core_respond(&req).unwrap();
    assert_eq!(planned, RespondPlan::Immediate);
    assert_eq!(
        planned.respond_route().unwrap().reason,
        "deterministic_slash_fallback"
    );
    let response = service.respond_with_plan(req, planned).await.unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    assert!(
        response
            .text
            .as_deref()
            .is_some_and(|text| text.contains("回复：/unknown-route-command"))
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "plain_chat");
    assert_eq!(diagnostics["route_reason"], "deterministic_slash_fallback");
    assert_eq!(diagnostics["route_semantic"], "deterministic");
}

#[tokio::test]
async fn unconsumed_immediate_plan_uses_preplanned_plain_chat_fallback() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let planned = PlannedRespond::immediate_chat("deterministic_handler_fallback");

    let response = service
        .respond_with_plan(private_message("没有确定性处理器消费这条消息"), planned)
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "plain_chat");
    assert_eq!(
        diagnostics["route_reason"],
        "deterministic_handler_fallback"
    );
    assert_eq!(diagnostics["route_semantic"], "deterministic");
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
        recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
