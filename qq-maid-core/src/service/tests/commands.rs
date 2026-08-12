use super::*;
use std::collections::HashMap;

fn available_voice_config() -> crate::config::VoiceFeatureConfig {
    crate::config::VoiceFeatureConfig::from_environment(&HashMap::from([
        ("TTS_PROVIDER".to_owned(), "qwen".to_owned()),
        ("QWEN_TTS_API_KEY".to_owned(), "test-key".to_owned()),
    ]))
}

async fn voice_command_completed(output: CoreRespondOutput) -> CoreResponse {
    match output {
        CoreRespondOutput::Complete(response) => *response,
        CoreRespondOutput::Stream(mut stream) => {
            collect_completed_without_text_delta(&mut stream).await
        }
    }
}

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
    assert!(!response.suppresses_reply());
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.starts_with("女仆长助手"))
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn voice_preference_is_read_before_generation_and_forces_complete_without_text_delta() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("完整".to_owned())),
        Ok(LlmStreamEvent::TextDelta("回复".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ]);
    let mut state = test_state(provider.clone(), 5);
    state.config.voice = available_voice_config();
    let service = CoreHandle::new(state);

    let enabled = voice_command_completed(
        service
            .respond(private_request("/语音 开启"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(enabled.command.as_deref(), Some("voice"));
    assert_eq!(enabled.text_content(), Some("语音回复已开启"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    let mut response_stream =
        expect_stream(service.respond(private_request("你好")).await.unwrap());
    assert_eq!(
        response_stream.output_policy(),
        CoreOutputPolicy::CompleteThenSend
    );
    let response = collect_completed_without_text_delta(&mut response_stream).await;
    assert_eq!(response.text_content(), Some("完整回复"));
    assert_eq!(response.delivery_hint, Some(CoreDeliveryHint::Voice));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn voice_command_is_deterministic_and_group_writes_require_admin_or_owner() {
    let provider = TestProvider::replying("不应调用");
    let mut state = test_state(provider.clone(), 5);
    state.config.voice = available_voice_config();
    let service = CoreHandle::new(state);

    let mut member_enable = group_request("/语音 开启");
    member_enable.actor.group_member_role = Some(CoreGroupMemberRole::Member);
    let denied = voice_command_completed(service.respond(member_enable).await.unwrap()).await;
    assert_eq!(
        denied.text_content(),
        Some("只有群主或管理员可以修改群聊语音设置")
    );

    let mut unknown_query = group_request("/语音");
    unknown_query.actor.group_member_role = Some(CoreGroupMemberRole::Unknown);
    let queried = voice_command_completed(service.respond(unknown_query).await.unwrap()).await;
    assert_eq!(queried.text_content(), Some("当前会话语音回复：已关闭"));

    for role in [CoreGroupMemberRole::Admin, CoreGroupMemberRole::Owner] {
        let mut enable = group_request("/语音 开启");
        enable.actor.group_member_role = Some(role);
        let enabled = voice_command_completed(service.respond(enable).await.unwrap()).await;
        assert_eq!(enabled.text_content(), Some("语音回复已开启"));
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn voice_enable_rejects_unavailable_configs_without_writing_enabled_state() {
    let configs = [
        (
            crate::config::VoiceFeatureConfig::default(),
            "语音功能当前未启用，请先配置 TTS_PROVIDER=qwen",
        ),
        (
            crate::config::VoiceFeatureConfig::from_environment(&HashMap::from([(
                "TTS_PROVIDER".to_owned(),
                "qwen".to_owned(),
            )])),
            "语音功能不可用：缺少 QWEN_TTS_API_KEY",
        ),
        (
            crate::config::VoiceFeatureConfig::from_environment(&HashMap::from([
                ("TTS_PROVIDER".to_owned(), "qwen".to_owned()),
                ("QWEN_TTS_API_KEY".to_owned(), "test-key".to_owned()),
                (
                    "QWEN_TTS_BASE_URL".to_owned(),
                    "http://invalid.example.test/tts".to_owned(),
                ),
            ])),
            "语音功能配置预检失败，请联系管理员检查 TTS 配置",
        ),
    ];

    for (voice, expected) in configs {
        let provider = TestProvider::replying("不应调用");
        let mut state = test_state(provider.clone(), 5);
        state.config.voice = voice;
        let service = CoreHandle::new(state);

        let rejected = voice_command_completed(
            service
                .respond(private_request("/语音 开启"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(rejected.text_content(), Some(expected));

        let queried =
            voice_command_completed(service.respond(private_request("/语音")).await.unwrap()).await;
        assert_eq!(queried.text_content(), Some("当前会话语音回复：已关闭"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
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
    assert!(!response.suppresses_reply());
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
    assert_eq!(response.handled, Some(true));
    assert_eq!(response.output, None);
    assert!(response.suppresses_reply());
    assert_eq!(response.diagnostics.as_ref().unwrap()["suppressed"], true);
    assert_eq!(
        response.diagnostics.as_ref().unwrap()["reason"],
        "unknown_group_slash_command"
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_codex_easter_egg_replies_in_unaddressed_group_without_model_call() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let classification = service
        .classify_inbound(group_request("/status"))
        .await
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    let CoreRespondOutput::Complete(response) =
        service.respond(group_request("/status")).await.unwrap()
    else {
        panic!("Codex easter egg should complete synchronously");
    };
    assert_eq!(response.text_content(), Some("状态：还能继续写。大概。"));
    assert_eq!(response.command.as_deref(), Some("codex_easter_egg"));
    assert!(!response.suppresses_reply());
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_codex_easter_egg_does_not_create_session_or_override_registered_command() {
    let provider = TestProvider::replying("不应调用");
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Complete(response) =
        service.respond(private_request("/model")).await.unwrap()
    else {
        panic!("Codex easter egg should complete synchronously");
    };
    assert_eq!(response.text_content(), Some("模型选择困难症已启动。"));
    assert_eq!(response.command.as_deref(), Some("codex_easter_egg"));

    let meta = SessionMeta::new(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    assert!(session_store.get_active(&meta).unwrap().is_none());

    let CoreRespondOutput::Complete(registered) =
        service.respond(private_request("/compact")).await.unwrap()
    else {
        panic!("registered compact command should complete synchronously");
    };
    assert_eq!(registered.command.as_deref(), Some("compact"));
    assert_ne!(registered.command.as_deref(), Some("codex_easter_egg"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_review_easter_egg_uses_safe_mention_name_and_degrades_without_one() {
    use qq_maid_common::identity_context::{
        MentionConfidence, MentionIdentity, MessageActorContext,
    };

    let provider = TestProvider::replying("不应调用");
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);
    let mut named = private_request("/review @小明");
    named.mentions = vec![MentionIdentity {
        raw_text: Some("@小明".to_owned()),
        target: MessageActorContext {
            display_name: Some("小明".to_owned()),
            source: IdentitySource::TextWeak,
            ..Default::default()
        },
        is_self: false,
        confidence: MentionConfidence::TextWeak,
    }];

    let CoreRespondOutput::Complete(named_response) = service.respond(named).await.unwrap() else {
        panic!("review easter egg should complete synchronously");
    };
    assert_eq!(
        named_response.text_content(),
        Some("审判官 @小明 已就位：LGTM（大概）")
    );

    let mut unnamed = private_request("/review");
    unnamed.mentions = vec![MentionIdentity {
        raw_text: None,
        target: MessageActorContext {
            user_id: Some("sensitive-user-id".to_owned()),
            source: IdentitySource::Event,
            ..Default::default()
        },
        is_self: false,
        confidence: MentionConfidence::Event,
    }];
    let CoreRespondOutput::Complete(unnamed_response) = service.respond(unnamed).await.unwrap()
    else {
        panic!("review easter egg should degrade synchronously");
    };
    assert_eq!(unnamed_response.text_content(), Some("LGTM（大概）"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_unknown_private_slash_is_deterministic_without_session_or_model_call() {
    let provider = TestProvider::replying("不应调用");
    let state = test_state(provider.clone(), 5);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    for input in ["/unknown", "/记忆查看1"] {
        let CoreRespondOutput::Complete(response) =
            service.respond(private_request(input)).await.unwrap()
        else {
            panic!("unknown private slash should complete synchronously");
        };

        assert_eq!(
            response.text_content(),
            Some("未知命令，发送 `/help` 查看可用命令。")
        );
        assert_eq!(response.command.as_deref(), Some("unknown_command"));
        assert!(!response.suppresses_reply());
    }
    let meta = SessionMeta::new(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    );
    assert!(session_store.get_active(&meta).unwrap().is_none());
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_addressed_group_unknown_slash_returns_hint_without_model_call() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    for input in ["/unknown", "/记忆查看1"] {
        let mut request = group_request(input);
        request.addressed_to_bot = true;
        let CoreRespondOutput::Complete(response) = service.respond(request).await.unwrap() else {
            panic!("addressed unknown group slash should complete synchronously");
        };

        assert_eq!(
            response.text_content(),
            Some("未知命令，发送 `/help` 查看可用命令。")
        );
        assert_eq!(response.command.as_deref(), Some("unknown_command"));
        assert!(!response.suppresses_reply());
    }
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_valid_memory_show_still_uses_registered_handler() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Complete(response) = service
        .respond(private_request("/记忆 查看 1"))
        .await
        .unwrap()
    else {
        panic!("memory show should complete synchronously");
    };

    assert_eq!(response.command.as_deref(), Some("memory_show"));
    assert_ne!(response.command.as_deref(), Some("unknown_command"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
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
    assert!(!invalid.suppresses_reply());
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
    assert!(!denied.suppresses_reply());
    assert!(denied.text_content().is_some());
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
        StreamDeliveryConfig {
            output_policy: CoreOutputPolicy::CompleteThenSend,
            provider_stream_enabled: false,
            delivery_hint: None,
        },
        AgentRequestBudget {
            request_timeout: Duration::from_secs(5),
            finalization_reserve: Duration::from_secs(
                crate::config::DEFAULT_AGENT_FINALIZATION_RESERVE_SECONDS,
            ),
        },
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
