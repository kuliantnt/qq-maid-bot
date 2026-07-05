use crate::provider::ToolCallingProtocol;

use super::support::*;

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
    assert!(text.contains("🚧 当前进行中 · 共 6 项"));
    assert!(text.contains("还有 1 项进行中待办，可说“查看全部进行中待办”。"));
    assert_refreshed_pending_snapshot(&service, &owner, 5);
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
    let owner = private_todo_owner();
    create_numbered_private_todos(&service, "待办", 1..=8);

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
    assert_refreshed_pending_snapshot(&service, &owner, 5);
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
    let owner = private_todo_owner();
    create_numbered_private_todos(&service, "待办", 1..=9);

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
    assert_refreshed_pending_snapshot(&service, &owner, 5);
}
