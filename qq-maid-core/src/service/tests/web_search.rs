use super::*;

#[tokio::test]
async fn core_web_search_private_command_uses_query_when_provider_stream_disabled() {
    let provider = TestProvider::replying("普通聊天不应调用");
    let query_executor = MockWebSearchExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("/查 今日 ai 新闻"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(
        response
            .markdown_content()
            .is_some_and(|text| text.starts_with("【联网查询】"))
    );
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("web answer: 今日 ai 新闻"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "今日 ai 新闻");
    assert_eq!(
        requests[0].raw_question.as_deref(),
        Some("/查 今日 ai 新闻")
    );
}

#[tokio::test]
async fn core_web_search_wechat_sync_path_uses_query() {
    let provider = TestProvider::replying("普通聊天不应调用").with_stream_enabled(true);
    let query_executor = MockWebSearchExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(wechat_service_request("/search 今日 ai 新闻"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(
        response
            .markdown_content()
            .is_some_and(|text| text.starts_with("【联网查询】"))
    );
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("web answer: 今日 ai 新闻"))
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "今日 ai 新闻");
}

#[tokio::test]
async fn core_web_search_query_raw_question_includes_quoted_context() {
    let provider = TestProvider::replying("普通聊天不应调用");
    let query_executor = MockWebSearchExecutor::default();
    let state =
        test_state_with_query_executor(provider.clone(), 5, Arc::new(query_executor.clone()));
    let service = CoreHandle::new(state);
    let mut request = private_request("/查询 这件事的最新进展");
    request.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        text_summary: Some("Rust 1.90 发布候选版已经开放测试".to_owned()),
        ..QuotedMessageContext::default()
    });

    let mut stream = expect_stream(service.respond(request).await.unwrap());
    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let requests = query_executor.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "这件事的最新进展");
    let raw_question = requests[0].raw_question.as_deref().unwrap();
    assert!(raw_question.contains("/查询 这件事的最新进展"));
    assert!(raw_question.contains("引用消息上下文"));
    assert!(raw_question.contains("引用文本：Rust 1.90 发布候选版已经开放测试"));
}
