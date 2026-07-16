use super::*;

#[test]
fn core_plan_routes_help_to_command_event_only() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state).respond_service();

    for input in ["/help", "/help rss", "/帮助"] {
        let req: RespondRequest = private_request(input).into();
        assert_eq!(
            service.plan_core_respond(&req).unwrap(),
            RespondPlan::CommandEvent,
            "{input}"
        );
    }

    let req: RespondRequest = private_request("/天气 杭州").into();
    assert_eq!(
        service.plan_core_respond(&req).unwrap(),
        RespondPlan::Immediate
    );
}

#[tokio::test]
async fn core_help_command_is_wrapped_as_response_events() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);
    let mut stream = expect_stream(service.respond(private_request("/help")).await.unwrap());

    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandStarted);

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command finished status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandFinished);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed help response");
    };
    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.starts_with("女仆长助手"))
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_group_registered_command_executes_without_model() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);
    let mut stream = expect_stream(service.respond(group_request("/help")).await.unwrap());
    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("帮助"))
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_unknown_group_slash_is_silent_without_model_call() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let classification = service
        .classify_inbound(group_request("/unknown"))
        .await
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    let CoreRespondOutput::Complete(response) =
        service.respond(group_request("/unknown")).await.unwrap()
    else {
        panic!("unknown group slash should complete synchronously");
    };
    assert!(response.suppresses_reply());
    assert_eq!(
        response.diagnostics.as_ref().unwrap()["reason"],
        "unknown_group_slash_command"
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_unknown_private_slash_keeps_existing_chat_behavior() {
    let provider = TestProvider::replying("私聊仍按普通聊天处理");
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Complete(response) =
        service.respond(private_request("/unknown")).await.unwrap()
    else {
        panic!("immediate private fallback should complete synchronously");
    };

    assert_eq!(response.text_content(), Some("私聊仍按普通聊天处理"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn group_command_validation_and_role_checks_stay_in_core() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Complete(invalid) =
        service.respond(group_request("/翻译")).await.unwrap()
    else {
        panic!("invalid command arguments should complete synchronously");
    };
    assert_eq!(invalid.command.as_deref(), Some("translation"));
    assert!(
        invalid
            .text_content()
            .is_some_and(|text| text.contains("用法：/翻译"))
    );

    let mut denied_request = group_request("/memory group add 群规则");
    denied_request.actor.group_member_role = Some(crate::service::CoreGroupMemberRole::Member);
    let CoreRespondOutput::Complete(denied) = service.respond(denied_request).await.unwrap() else {
        panic!("permission rejection should complete synchronously");
    };
    assert_eq!(denied.command.as_deref(), Some("group_admin_required"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn onebot_commands_use_real_core_and_account_scoped_conversation() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);
    let mut request = private_request("/new OneBot 回归");
    request.platform = Platform::OneBot;
    request.account_id = Some("10001".to_owned());
    request.actor.user_id = Some("20002".to_owned());
    request.conversation = CoreConversation::Private {
        peer_id: "20002".to_owned(),
    };

    assert_eq!(
        request.scope_key(),
        "platform:onebot:account:10001:private:20002"
    );
    let new_response = match service.respond(request.clone()).await.unwrap() {
        CoreRespondOutput::Complete(response) => *response,
        CoreRespondOutput::Stream(mut stream) => {
            collect_completed_without_text_delta(&mut stream).await
        }
    };
    assert_eq!(new_response.command.as_deref(), Some("new"));
    assert!(new_response.session_id.is_some());

    request.text = "/help".to_owned();
    let mut stream = expect_stream(service.respond(request).await.unwrap());
    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.starts_with("女仆长助手"))
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_command_event_failure_does_not_send_finished_or_completed() {
    let provider = TestProvider::failing(LlmError::provider("compact unavailable", "provider"))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let session_store = state.stores.session_store.clone();
    let meta = SessionMeta::new(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    let mut session = session_store.get_or_create_active(&meta).unwrap();
    session_store
        .append_exchange(&mut session, "上一轮用户消息", "上一轮助手回复")
        .unwrap();
    let service = CoreHandle::new(state).respond_service();
    let req: RespondRequest = private_request("/compact").into();
    let mut stream = start_core_response_stream(
        service,
        req,
        PlannedRespond::command_event(),
        CoreOutputPolicy::CompleteThenSend,
        false,
        Duration::from_secs(5),
        ProgressStatusConfig {
            hint: StatusHint::model(),
            audience: StatusAudience::Private,
            display_name: "小女仆".to_owned(),
        },
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected command started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::CommandStarted);

    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(status) => {
                assert_ne!(status.kind, CoreResponseStatusKind::CommandFinished);
            }
            CoreResponseEvent::Failed(failure) => {
                assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
                assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
                return;
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response after command failure: {response:?}");
            }
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta in command event failure path: {delta}");
            }
        }
    }
    panic!("stream ended without failure");
}

#[tokio::test]
async fn wechat_service_chat_completes_without_direct_stream() {
    let provider = TestProvider::replying("微信完整回复").with_stream_enabled(true);
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) = service
        .respond(wechat_service_request("hello"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };
    assert_eq!(stream.output_policy(), CoreOutputPolicy::CompleteThenSend);

    let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
        panic!("expected completed response");
    };

    assert_eq!(response.text_content(), Some("微信完整回复"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        provider.requests()[0].metadata.get("purpose").unwrap(),
        "chat"
    );
}
