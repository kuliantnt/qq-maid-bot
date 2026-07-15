use std::fs;

use qq_maid_llm::provider::{ToolCallingProtocol, types::ChatRole};
use serde_json::Value;

use crate::runtime::{
    respond::{PlannedRespond, RespondPlan, agent_route::RespondRoute},
    tools::{
        StatusHint,
        status::{StatusAction, StatusSubject},
    },
};

use super::{
    super::{
        RespondRequest,
        command_dispatcher::{CommandDispatcher, DispatchOutcome},
        common::empty_respond_request,
    },
    support::*,
};
use crate::runtime::{
    tools::rss::{RssFeedItem, RssTarget, RssTargetType},
    tools::todo::{TodoItemDraft, TodoPendingOperation, TodoStore, TodoTimePrecision},
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
async fn private_weather_chat_with_openai_responses_capability_enters_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let mut request = private_message("杭州今天要带伞吗");
    request.platform = "onebot11".to_owned();
    request.account_id = Some("bot-1".to_owned());
    request.scope_key = "opaque-private-conversation".to_owned();
    let response = service.respond(request).await.unwrap();

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
    assert_eq!(
        tool_request.tool_context.actor.user_id.as_deref(),
        Some("u1")
    );
    assert_eq!(
        tool_request.tool_context.conversation.kind,
        qq_maid_common::identity_context::ConversationKind::Private
    );
    assert_eq!(tool_request.tool_context.conversation.platform, "onebot11");
    assert_eq!(
        tool_request.tool_context.conversation.account_id.as_deref(),
        Some("bot-1")
    );
    assert_eq!(
        tool_request.tool_context.conversation.target_id.as_deref(),
        Some("u1")
    );
    assert_eq!(
        tool_request.tool_context.conversation.scope_id,
        "opaque-private-conversation"
    );
    assert_eq!(
        tool_request.tool_context.conversation.interaction_scope_id,
        "opaque-private-conversation"
    );
    assert!(!tool_request.tool_context.task_id.trim().is_empty());
    assert!(tool_request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System
            && message.content.contains("存在歧义")
            && message.content.contains("不要调用写工具")
    }));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "agent_runtime");
    assert_eq!(diagnostics["route_reason"], "agent_runtime_available");
    assert_eq!(diagnostics["route_domains"], serde_json::json!(["weather"]));
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
    assert_eq!(diagnostics["respond_route"], "agent_runtime");
    assert_eq!(diagnostics["route_reason"], "agent_runtime_available");
    assert_eq!(diagnostics["route_domains"], serde_json::json!([]));
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
async fn empty_agent_chat_reply_uses_configured_bot_display_name() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("");
    let mut service = test_service_with_provider_and_tool_calling(provider, true);
    service.bot_display_name = "小助手".to_owned();

    let response = service
        .respond(private_message("聊聊 Rust 的所有权"))
        .await
        .unwrap();

    assert_eq!(
        response.text.as_deref(),
        Some("唔，小助手刚刚没整理出可用回复。可以再说一次。")
    );
    assert_eq!(response.markdown, None);
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
    // 已生成的 StandardChat decision，不能读取新状态或按文本重新分类。
    let response = service
        .respond_stream_with_plan(private_message("杭州今天要带伞吗"), planned, |_| {
            Box::pin(async { Ok(()) })
        })
        .await
        .unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "standard_chat");
    assert_eq!(diagnostics["route_reason"], "agent_unavailable");
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
    assert_eq!(
        tool_request.tool_context.actor.user_id.as_deref(),
        Some("u1")
    );
    assert_eq!(
        tool_request.tool_context.conversation.kind,
        qq_maid_common::identity_context::ConversationKind::Group
    );
    assert_eq!(
        tool_request.tool_context.conversation.target_id.as_deref(),
        Some("g1")
    );
    assert_eq!(tool_request.tool_context.conversation.scope_id, "group:g1");
    assert_eq!(
        tool_request.tool_context.conversation.interaction_scope_id,
        "group:g1"
    );
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
async fn private_natural_search_intent_routes_to_agent_runtime_with_search_semantics() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider, true);

    for input in ["联网查一下", "联网查询下今日 ai 新闻", "查一下今天 AI 新闻"]
    {
        let planned = service.plan_core_respond(&private_message(input)).unwrap();
        assert_eq!(planned, RespondPlan::AgentRuntime, "{input}");

        let decision = planned.respond_route().unwrap();
        assert_eq!(decision.route, RespondRoute::AgentRuntime, "{input}");
        assert_eq!(
            planned.classified_status_hint(),
            Some(StatusHint::new(StatusSubject::Search, StatusAction::Query)),
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
    assert_eq!(planned, RespondPlan::AgentRuntime);
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
    assert_eq!(diagnostics["respond_route"], "standard_chat");
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
async fn unknown_slash_command_falls_back_to_standard_chat_with_router_decision() {
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
    assert_eq!(diagnostics["respond_route"], "standard_chat");
    assert_eq!(diagnostics["route_reason"], "deterministic_slash_fallback");
}

#[tokio::test]
async fn unconsumed_immediate_plan_uses_preplanned_standard_chat_fallback() {
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
    assert_eq!(diagnostics["respond_route"], "standard_chat");
    assert_eq!(
        diagnostics["route_reason"],
        "deterministic_handler_fallback"
    );
}

#[tokio::test]
async fn tool_calling_disabled_uses_standard_chat() {
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
async fn unsupported_provider_capability_falls_back_to_standard_chat() {
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
