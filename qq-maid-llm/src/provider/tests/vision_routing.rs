use super::*;
use qq_maid_common::input_part::{MessageInputPart, MessageMedia};

/// 普通、流式与 Tool Loop 均只降级候选副本，两个方向的能力切换都不能污染原图。
#[tokio::test]
async fn mixed_vision_routes_preserve_original_media_for_each_candidate() {
    for primary_vision in [false, true] {
        for mode in ["chat", "stream", "tools"] {
            let mut first = MockProvider::new("openai", vec![Err(LlmError::timeout("provider"))])
                .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
                .with_tool_results(vec![Err(LlmError::timeout("provider"))]);
            let mut second = MockProvider::new("deepseek", vec![Ok(outcome("fallback"))])
                .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
                .with_tool_results(vec![Ok(outcome("fallback"))]);
            if mode == "stream" {
                first =
                    MockProvider::with_streams("openai", vec![Err(LlmError::timeout("provider"))]);
                second = MockProvider::with_streams(
                    "deepseek",
                    vec![Ok(stream_events(vec![
                        Ok(LlmStreamEvent::TextDelta("fallback".to_owned())),
                        Ok(LlmStreamEvent::Completed {
                            usage: None,
                            finish_reason: None,
                            fallback_used: false,
                        }),
                    ]))],
                );
            }
            if primary_vision {
                first = first.with_vision();
            } else {
                second = second.with_vision();
            }
            let first = Arc::new(first);
            let second = Arc::new(second);
            let provider = ModelRouteProvider::new(
                "auto",
                ModelProvider::OpenAi,
                ModelRoute::parse_config("openai:text,deepseek:vision", "test").unwrap(),
                vec![
                    (ModelProvider::OpenAi, first.clone()),
                    (ModelProvider::DeepSeek, second.clone()),
                ],
            )
            .unwrap();
            assert!(provider.supports_vision(None));
            assert_eq!(
                provider.supports_vision(Some("openai:text")),
                primary_vision
            );
            assert_eq!(
                provider.supports_vision(Some("deepseek:vision")),
                !primary_vision
            );
            let image = MessageInputPart::image(MessageMedia {
                url: Some("https://example.test/pxe.png".to_owned()),
                ..Default::default()
            });
            let mut req = request();
            req.messages = vec![ChatMessage::user_with_parts("看图", vec![image.clone()])];
            let result = match mode {
                "chat" => provider.chat(req).await.unwrap(),
                "stream" => collect_llm_stream(
                    provider.stream_chat(req).await.unwrap(),
                    provider.name(),
                    provider.model(),
                )
                .await
                .unwrap(),
                _ => {
                    let mut tools = tool_request();
                    tools.chat = req;
                    provider.chat_with_tools(tools).await.unwrap()
                }
            };
            assert!(result.fallback_used);
            for (mock, vision) in [(&first, primary_vision), (&second, !primary_vision)] {
                let sent = if mode == "tools" {
                    mock.tool_requests()[0].chat.clone()
                } else {
                    mock.requests()[0].clone()
                };
                let parts = &sent.messages[0].content_parts;
                if vision {
                    assert_eq!(parts, std::slice::from_ref(&image));
                } else {
                    assert!(matches!(parts[0], MessageInputPart::Text { .. }));
                    assert!(parts[0].fallback_text().contains("当前模型不支持读取图片"));
                }
            }
        }
    }
}
