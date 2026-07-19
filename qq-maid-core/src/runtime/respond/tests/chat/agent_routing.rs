//! Agent 路由、场景工具开关与请求上下文测试。

use super::*;

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
    assert!(inspector.requests().is_empty());
    let tool_request = inspector.tool_requests().remove(0);
    assert_eq!(
        tool_request
            .chat
            .metadata
            .get("image_generation")
            .map(String::as_str),
        Some("true")
    );
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
    assert!(inspector.requests().is_empty());
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "agent_runtime");
    assert_eq!(diagnostics["route_reason"], "agent_runtime_available");
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_calling_used"], false);
    assert_eq!(diagnostics["agent_result"], "direct_answer");
    assert_eq!(diagnostics["todo_success_claimed"], false);
    assert_eq!(diagnostics["todo_success_verified"], true);
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
                .contains(&format!("回复：{input}"))
        );
    }
    assert_eq!(inspector.tool_call_count(), 5);
    assert!(inspector.requests().is_empty());
}
