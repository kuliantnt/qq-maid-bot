use qq_maid_llm::provider::ToolCallingProtocol;

use crate::runtime::respond::tests::support::{
    MockProvider, private_message, sync_test_knowledge,
    test_service_with_provider_tool_calling_and_base,
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
