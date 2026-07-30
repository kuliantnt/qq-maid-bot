use qq_maid_common::input_part::{MessageInputPart, MessageMedia};

use super::*;

#[tokio::test]
async fn private_image_chat_keeps_image_in_agent_tool_loop_request() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_vision()
        .with_tool_loop_reply_without_tool("图片已收到");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let mut req = private_message("看看这张图");
    req.input_parts = vec![
        MessageInputPart::text("看看这张图"),
        MessageInputPart::image(MessageMedia {
            mime_type: Some("image/jpeg".to_owned()),
            url: Some("https://example.test/image.jpg".to_owned()),
            ..Default::default()
        }),
    ];

    let planned = service.plan_core_respond(&req).unwrap();
    assert_eq!(planned, RespondPlan::AgentRuntime);
    let response = service.respond_with_plan(req, planned).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("图片已收到"));
    assert_eq!(inspector.tool_call_count(), 1);
    let request = inspector.tool_requests().remove(0);
    assert!(
        request
            .chat
            .messages
            .last()
            .unwrap()
            .content_parts
            .iter()
            .any(|part| matches!(part, MessageInputPart::Image { .. }))
    );
}
