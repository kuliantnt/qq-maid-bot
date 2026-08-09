use super::*;

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

pub(super) fn group_message(text: &str) -> RespondRequest {
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
