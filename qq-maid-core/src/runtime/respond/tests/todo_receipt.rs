use qq_maid_llm::provider::ToolCallingProtocol;

use super::support::*;

#[tokio::test]
async fn todo_complete_receipt_is_lightweight_and_refreshes_pending_snapshot() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    create_numbered_private_todos(&service, "待办", 1..=7);

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
    assert!(!text.contains("🚧 当前进行中 · 共 6 项"));
    assert!(!text.contains("还有 1 项进行中待办"));
    assert_refreshed_pending_snapshot(&service, &owner, 6);
}

#[tokio::test]
async fn todo_complete_receipt_refreshes_pending_snapshot_at_ten_item_limit() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "已完成第一条",
        );
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    create_numbered_private_todos(&service, "待办", 1..=12);

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
    assert!(!text.contains("🚧 当前进行中 · 共 11 项"));
    assert_refreshed_pending_snapshot(&service, &owner, 10);
}
