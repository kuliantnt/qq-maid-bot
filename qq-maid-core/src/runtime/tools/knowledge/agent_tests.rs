use qq_maid_llm::provider::{ToolCallingProtocol, types::ChatRole};

use crate::runtime::respond::tests::support::{
    MockProvider, break_test_knowledge_search, private_message, sync_test_knowledge,
    test_service_with_provider_tool_calling_and_base,
    test_service_with_provider_tool_calling_mode_and_base,
};

use super::KNOWLEDGE_SEARCH_TOOL_NAME;

#[tokio::test]
async fn private_agent_executes_knowledge_search_and_answers_from_evidence() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            KNOWLEDGE_SEARCH_TOOL_NAME,
            r#"{"query":"RAG-504 是什么错误","max_results":null}"#,
            "根据本地知识证据，RAG-504 表示上游请求超时。",
        );
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_and_base(provider);
    sync_test_knowledge(
        &service,
        &base,
        "operations/errors.md",
        "# 错误码\n\n## RAG-504\n\nRAG-504 表示上游请求超时。",
    );

    let response = service
        .respond(private_message("项目里的 RAG-504 是什么错误？"))
        .await
        .unwrap();

    assert_eq!(
        response.text.as_deref(),
        Some("根据本地知识证据，RAG-504 表示上游请求超时。")
    );
    assert_eq!(inspector.tool_call_count(), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!([KNOWLEDGE_SEARCH_TOOL_NAME])
    );
    assert_eq!(diagnostics["agent_tool_results"][0]["succeeded"], true);
}

#[tokio::test]
async fn tool_mode_smalltalk_does_not_search_or_inject_knowledge() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_and_base(provider);
    sync_test_knowledge(
        &service,
        &base,
        "private/smalltalk.md",
        "# 闲聊资料\n\n今天心情不错，随便聊聊。知识标记 TOOL-MODE-MUST-NOT-INJECT。",
    );

    let response = service
        .respond(private_message("今天心情不错，随便聊聊"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    assert!(
        !request
            .chat
            .messages
            .iter()
            .any(|message| message.role == ChatRole::System
                && message.content.contains("TOOL-MODE-MUST-NOT-INJECT"))
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["knowledge_mode"], "tool");
    assert_eq!(diagnostics["used_knowledge"], false);
    assert_eq!(diagnostics["knowledge_hit_count"], 0);
    assert_eq!(diagnostics["agent_executed_tools"], serde_json::json!([]));
}

#[tokio::test]
async fn auto_mode_injects_knowledge_and_hides_search_tool() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_mode_and_base(
        provider,
        crate::config::KnowledgeRetrievalMode::Auto,
    );
    sync_test_knowledge(
        &service,
        &base,
        "operations/errors.md",
        "# 错误码\n\nRAG-504 表示上游请求超时。",
    );

    let response = service
        .respond(private_message("RAG-504 是什么？"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    assert!(request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System && message.content.contains("RAG-504 表示上游请求超时")
    }));
    let names = request
        .tools
        .metadata()
        .into_iter()
        .map(|metadata| metadata.name)
        .collect::<Vec<_>>();
    assert!(!names.contains(&KNOWLEDGE_SEARCH_TOOL_NAME.to_owned()));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["knowledge_mode"], "auto");
    assert_eq!(diagnostics["used_knowledge"], true);
    assert_eq!(diagnostics["knowledge_hit_count"], 1);
}

#[tokio::test]
async fn preflight_injects_only_high_confidence_primary_evidence_and_keeps_tool() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_mode_and_base(
        provider,
        crate::config::KnowledgeRetrievalMode::Preflight,
    );
    sync_test_knowledge(
        &service,
        &base,
        "operations/errors.md",
        "# 错误码\n\n## RAG-504\n\nRAG-504 表示上游请求超时。",
    );

    let response = service
        .respond(private_message("RAG-504 是什么错误？"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    let knowledge_messages = request
        .chat
        .messages
        .iter()
        .filter(|message| {
            message.role == ChatRole::System && message.content.contains("本地 Markdown 知识资料")
        })
        .collect::<Vec<_>>();
    assert_eq!(knowledge_messages.len(), 1);
    assert!(
        knowledge_messages[0]
            .content
            .contains("RAG-504 表示上游请求超时")
    );
    assert!(!knowledge_messages[0].content.contains("片段：章节补充"));
    assert!(
        request
            .tools
            .metadata()
            .iter()
            .any(|metadata| metadata.name == KNOWLEDGE_SEARCH_TOOL_NAME)
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["knowledge_mode"], "preflight");
    assert_eq!(diagnostics["knowledge_injection_allowed"], true);
    assert_eq!(
        diagnostics["knowledge_injection_reason"],
        "lexical_high_confidence"
    );
    assert_eq!(diagnostics["knowledge_hit_count"], 1);
}

#[tokio::test]
async fn preflight_low_relevance_candidate_is_zero_injection() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_mode_and_base(
        provider,
        crate::config::KnowledgeRetrievalMode::Preflight,
    );
    sync_test_knowledge(
        &service,
        &base,
        "operations/service.md",
        "# 服务手册\n\n## 部署\n\n服务启动后会同步本地索引。LOW-RELEVANCE-MARKER",
    );

    let response = service
        .respond(private_message("这个服务挺不错，晚饭吃什么？"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    assert!(!request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System && message.content.contains("LOW-RELEVANCE-MARKER")
    }));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["knowledge_mode"], "preflight");
    assert_eq!(diagnostics["knowledge_status"], "low_relevance");
    assert_eq!(diagnostics["knowledge_injection_allowed"], false);
    assert_eq!(diagnostics["knowledge_injection_reason"], "below_threshold");
    assert!(diagnostics["knowledge_candidate_count"].as_u64().unwrap() > 0);
    assert_eq!(diagnostics["knowledge_hit_count"], 0);
}

#[tokio::test]
async fn preflight_search_failure_is_zero_injection_without_blocking_chat() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let (service, base) = test_service_with_provider_tool_calling_mode_and_base(
        provider,
        crate::config::KnowledgeRetrievalMode::Preflight,
    );
    sync_test_knowledge(
        &service,
        &base,
        "operations/errors.md",
        "# 错误码\n\nRAG-504 表示上游请求超时。",
    );
    break_test_knowledge_search(&service);

    let response = service
        .respond(private_message("RAG-504 是什么？"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    assert!(!request.chat.messages.iter().any(|message| {
        message.role == ChatRole::System && message.content.contains("RAG-504 表示上游请求超时")
    }));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["knowledge_status"], "failed");
    assert_eq!(diagnostics["knowledge_injection_allowed"], false);
    assert_eq!(diagnostics["knowledge_injection_reason"], "search_failed");
    assert_eq!(diagnostics["knowledge_hit_count"], 0);
}

#[tokio::test]
async fn no_hit_does_not_preserve_fabricated_model_conclusion() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            KNOWLEDGE_SEARCH_TOOL_NAME,
            r#"{"query":"ZZ-UNKNOWN-999","max_results":null}"#,
            "ZZ-UNKNOWN-999 已确认表示数据库连接耗尽。",
        );
    let (service, _) = test_service_with_provider_tool_calling_and_base(provider);

    let response = service
        .respond(private_message("ZZ-UNKNOWN-999 是什么？"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("没有找到相关证据"));
    assert!(!text.contains("数据库连接耗尽"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["error_code"], "knowledge_no_hit");
    assert_eq!(diagnostics["agent_tool_results"][0]["succeeded"], true);
}

#[tokio::test]
async fn failed_search_does_not_preserve_fabricated_model_conclusion() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            KNOWLEDGE_SEARCH_TOOL_NAME,
            r#"{"query":"RAG-504","max_results":null}"#,
            "RAG-504 已确认表示权限不足。",
        );
    let (service, base) = test_service_with_provider_tool_calling_and_base(provider);
    sync_test_knowledge(
        &service,
        &base,
        "operations/errors.md",
        "# 错误码\n\nRAG-504 表示上游请求超时。",
    );
    break_test_knowledge_search(&service);

    let response = service
        .respond(private_message("RAG-504 是什么？"))
        .await
        .unwrap();

    let text = response.text.unwrap();
    assert!(text.contains("知识检索失败"));
    assert!(!text.contains("权限不足"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["error_code"], "knowledge_db_error");
    assert_eq!(diagnostics["agent_tool_results"][0]["succeeded"], false);
}

#[tokio::test]
async fn group_whitelist_contains_knowledge_search_when_tool_loop_is_enabled() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let inspector = provider.clone();
    let service =
        crate::runtime::respond::tests::support::test_service_with_provider_and_group_tool_calling(
            provider, true, true,
        );

    service
        .respond(crate::runtime::respond::tests::support::message(
            "群知识库里的部署步骤是什么？",
        ))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    let names = request
        .tools
        .metadata()
        .into_iter()
        .map(|metadata| metadata.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&KNOWLEDGE_SEARCH_TOOL_NAME.to_owned()));
}
