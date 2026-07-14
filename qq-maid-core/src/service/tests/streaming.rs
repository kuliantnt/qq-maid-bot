use super::*;

#[tokio::test]
async fn upstream_check_calls_provider_without_creating_session() {
    let provider = TestProvider::replying("OK");
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    service.upstream_check().await.unwrap();

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].session_id, "diagnostic:upstream_check");
    assert_eq!(
        requests[0].metadata.get("purpose").map(String::as_str),
        Some("upstream_check")
    );
    // `/ping check` 只验证 provider 连通性，不能创建业务会话或写聊天历史。
    let sessions = session_store
        .list_for_scope("diagnostic:upstream_check", None)
        .unwrap();
    assert!(sessions.is_empty());
}

#[tokio::test]
async fn provider_error_is_returned_as_stream_failure() {
    let state = test_state(
        TestProvider::failing(LlmError::provider("boom", "provider")),
        5,
    );
    let service = CoreHandle::new(state);

    let failure = collect_stream_failure(service.respond(private_request("hello")).await).await;

    assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
    assert!(failure.retryable);
}

#[tokio::test]
async fn stream_response_is_not_cut_by_request_total_timeout() {
    let state = test_state(TestProvider::delayed("late", Duration::from_millis(80)), 0);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("late"));
}

#[tokio::test]
async fn chat_stream_forwards_text_delta_and_completed_from_same_stream() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("你".to_owned())),
        Ok(LlmStreamEvent::TextDelta("好".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ]);
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) =
        service.respond(private_request("hello")).await.unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::DirectStream);

    assert_eq!(
        stream.recv().await,
        Some(CoreResponseEvent::TextDelta("你".to_owned()))
    );
    assert_eq!(
        stream.recv().await,
        Some(CoreResponseEvent::TextDelta("好".to_owned()))
    );
    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("你好"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let sessions = session_store.list_for_scope(private_scope(), None).unwrap();
    assert_eq!(sessions[0].history.last().unwrap().content, "你好");
}

#[tokio::test]
async fn stream_disabled_chat_completes_without_synthetic_delta() {
    let provider = TestProvider::replying("非流完整回复");
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) =
        service.respond(private_request("hello")).await.unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("非流完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
}

#[test]
fn output_policy_names_are_consistent_across_stream_plans() {
    let cases = [
        (
            RespondPlan::StreamingChat,
            true,
            CoreOutputPolicy::DirectStream,
            "direct_stream",
        ),
        (
            RespondPlan::StreamingChat,
            false,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::CommandEvent,
            true,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::WebSearch,
            true,
            CoreOutputPolicy::DirectStream,
            "direct_stream",
        ),
        (
            RespondPlan::WebSearch,
            false,
            CoreOutputPolicy::CompleteThenSend,
            "ordinary_complete",
        ),
        (
            RespondPlan::AgentRuntime,
            true,
            CoreOutputPolicy::ProgressThenStream,
            "progress_then_stream",
        ),
        (
            RespondPlan::AgentRuntime,
            false,
            CoreOutputPolicy::ProgressThenComplete,
            "progress_then_complete",
        ),
    ];

    for (plan, provider_stream_enabled, expected_policy, expected_name) in cases {
        let policy = output_policy_for_stream(plan, provider_stream_enabled);
        assert_eq!(policy, expected_policy);
        assert_eq!(policy.as_str(), expected_name);
    }
}
