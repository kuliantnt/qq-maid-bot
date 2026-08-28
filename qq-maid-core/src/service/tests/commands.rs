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
async fn core_roll_defaults_to_d20_and_is_consumed_without_model_or_session() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    let classification = service
        .classify_inbound(private_request("/roll"))
        .await
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    let CoreRespondOutput::Complete(response) =
        service.respond(private_request("/roll")).await.unwrap()
    else {
        panic!("roll command should complete synchronously");
    };
    let assert_d20_reply = |response: &CoreResponse| {
        let text = response.text_content().expect("roll should return text");
        let value = text
            .strip_prefix("🎲 掷出了 ")
            .and_then(|text| text.strip_suffix(" / 20"))
            .and_then(|value| value.parse::<u8>().ok())
            .expect("roll reply should contain a D20 value");
        assert!((1..=20).contains(&value));
        assert_eq!(response.command.as_deref(), Some("roll"));
        let diagnostics = response
            .diagnostics
            .as_ref()
            .expect("local roll should expose diagnostics");
        assert_eq!(diagnostics["roll_execution_kind"], "local");
        assert_eq!(diagnostics["roll_provider"], "rust");
        assert_eq!(diagnostics["roll_model"], "roll-local");
        assert_eq!(diagnostics["roll_fallback_used"], false);
    };
    assert_d20_reply(&response);

    let CoreRespondOutput::Complete(group_response) =
        service.respond(group_request("/roll")).await.unwrap()
    else {
        panic!("group roll command should complete synchronously");
    };
    assert_d20_reply(&group_response);

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
async fn core_dot_nn_query_is_immediate_and_returns_manual_display_name() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut set_request = group_request(".nn 雪雪");
    set_request.input_parts = vec![qq_maid_common::input_part::MessageInputPart::text(
        ".nn 雪雪",
    )];
    let set_response = voice_command_completed(service.respond(set_request).await.unwrap()).await;
    assert_eq!(set_response.command.as_deref(), Some("set"));
    assert!(
        set_response
            .text_content()
            .is_some_and(|text| text.contains("雪雪"))
    );

    let mut query_request = group_request(".nn");
    query_request.input_parts = vec![qq_maid_common::input_part::MessageInputPart::text(".nn")];
    let classification = service
        .classify_inbound(query_request.clone())
        .await
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    let query_response =
        voice_command_completed(service.respond(query_request).await.unwrap()).await;
    assert_eq!(query_response.command.as_deref(), Some("set"));
    assert!(
        query_response
            .text_content()
            .is_some_and(|text| text.contains("雪雪"))
    );

    let mut help_request = group_request(".help");
    help_request.input_parts = vec![qq_maid_common::input_part::MessageInputPart::text(".help")];
    let help_response = voice_command_completed(service.respond(help_request).await.unwrap()).await;
    assert_eq!(help_response.command.as_deref(), Some("help"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_roll_executes_dice_expression_without_model_tool_or_session() {
    let provider =
        TestProvider::replying("不应调用").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    let classification = service
        .classify_inbound(private_request("/roll 2d6"))
        .await
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    let CoreRespondOutput::Complete(response) =
        service.respond(private_request("/roll 2d6")).await.unwrap()
    else {
        panic!("dice expression should complete synchronously");
    };
    let text = response
        .text_content()
        .expect("dice expression should return text");
    let calculation = text
        .strip_prefix("🎲 2d6：")
        .expect("2d6 reply should identify the expression");
    let (values, total) = calculation
        .split_once(" = ")
        .expect("2d6 reply should contain a total");
    let values = values
        .split(" + ")
        .map(|value| value.parse::<u8>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert!(values.iter().all(|value| (1..=6).contains(value)));
    assert_eq!(
        total.parse::<u16>().unwrap(),
        values.iter().map(|value| u16::from(*value)).sum::<u16>()
    );
    assert_eq!(response.command.as_deref(), Some("roll"));

    let CoreRespondOutput::Complete(modifier_response) = service
        .respond(private_request("/roll 1d20 + 3"))
        .await
        .unwrap()
    else {
        panic!("modifier expression should complete synchronously");
    };
    let modifier_text = modifier_response
        .text_content()
        .expect("modifier expression should return text");
    assert!(modifier_text.starts_with("🎲 1d20+3："));
    assert!(modifier_text.contains(" + 3 = "));
    assert_eq!(modifier_response.command.as_deref(), Some("roll"));

    let CoreRespondOutput::Complete(short_alias_response) = service
        .respond(private_request("/r 1d8+1d6+4"))
        .await
        .unwrap()
    else {
        panic!("/r dice expression should complete synchronously");
    };
    let short_alias_text = short_alias_response
        .text_content()
        .expect("/r expression should return text");
    assert!(short_alias_text.starts_with("🎲 1d8+1d6+4："));
    assert_eq!(short_alias_response.command.as_deref(), Some("roll"));

    let meta = SessionMeta::new_with_account(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
        Some("app-1".to_owned()),
    );
    assert!(session_store.get_active(&meta).unwrap().is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_roll_dm_uses_one_plain_model_call_without_tool_loop_or_session() {
    let provider = TestProvider::replying(
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"今晚适合出门","failure_meaning":"今晚适合宅家"}"#,
    )
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let session_store = state.stores.session_store.clone();
    let service = CoreHandle::new(state);

    let assert_dm_reply = |response: &CoreResponse| {
        let text = response.text_content().expect("roll DM should return text");
        assert!(text.contains("🎲 命运检定"));
        assert!(text.contains("难度：中等（DC 11）"));
        let roll = text
            .lines()
            .find_map(|line| line.strip_prefix("投掷："))
            .and_then(|value| value.parse::<u8>().ok())
            .expect("roll DM reply should contain a local D20 value");
        assert!((1..=20).contains(&roll));
        match roll {
            20 => assert!(text.contains("✨ Natural 20！大成功")),
            1 => assert!(text.contains("💀 Natural 1！大失败")),
            11..=19 => assert!(text.contains("✅ 成功")),
            _ => assert!(text.contains("❌ 失败")),
        }
        assert_eq!(response.command.as_deref(), Some("roll"));
        let diagnostics = response
            .diagnostics
            .as_ref()
            .expect("roll DM should expose diagnostics");
        assert_eq!(diagnostics["roll_execution_kind"], "ai_dm_success");
        assert_eq!(diagnostics["roll_provider"], "test-provider");
        assert_eq!(diagnostics["roll_model"], "test-model");
        assert_eq!(diagnostics["roll_total_latency_ms"], 1);
        assert_eq!(diagnostics["roll_fallback_used"], false);
    };

    let CoreRespondOutput::Complete(private_response) = service
        .respond(private_request("/roll 晚上要不要出门"))
        .await
        .unwrap()
    else {
        panic!("private roll DM command should complete synchronously");
    };
    assert_dm_reply(&private_response);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].metadata["purpose"], "roll_dm_check");
    let dm_context = &requests[0].messages.last().unwrap().content;
    assert!(dm_context.contains("用户问题：晚上要不要出门"));
    assert!(dm_context.contains("骰式：1d20"));
    assert!(dm_context.contains("最小总值：1"));
    assert!(dm_context.contains("最大总值：20"));
    assert!(
        requests[0]
            .messages
            .iter()
            .all(|message| !message.content.contains("投掷："))
    );
    assert!(!dm_context.contains("Natural 20"));

    let CoreRespondOutput::Complete(group_response) = service
        .respond(group_request("/roll 能不能说服老板让我早点下班"))
        .await
        .unwrap()
    else {
        panic!("group roll DM command should complete synchronously");
    };
    assert_dm_reply(&group_response);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);

    let private_meta = SessionMeta::new_with_account(
        private_scope(),
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
        Some("app-1".to_owned()),
    );
    let group_meta = SessionMeta::new_with_account(
        "platform:qq_official:account:app-1:group:g1",
        Some("u1".to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
        Some("app-1".to_owned()),
    );
    assert!(session_store.get_active(&private_meta).unwrap().is_none());
    assert!(session_store.get_active(&group_meta).unwrap().is_none());
}

#[tokio::test]
async fn core_roll_dm_custom_expression_uses_core_entertainment_dc() {
    let provider = TestProvider::replying(
        r#"{"type":"fortune","check_name":"是否吃夜宵检定","difficulty":"easy","success_meaning":"吃夜宵","failure_meaning":"今晚不吃"}"#,
    );
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Complete(response) = service
        .respond(private_request("/roll 2d20+4 我想要不要吃夜宵"))
        .await
        .unwrap()
    else {
        panic!("custom roll DM command should complete synchronously");
    };

    let text = response
        .text_content()
        .expect("custom roll DM should return text");
    assert!(text.contains("难度：容易（DC 20）"));
    let diagnostics = response
        .diagnostics
        .as_ref()
        .expect("custom roll DM should expose diagnostics");
    assert_eq!(diagnostics["dice_expression"], "2d20+4");
    assert_eq!(diagnostics["dice_minimum"], 6);
    assert_eq!(diagnostics["dice_maximum"], 44);
    assert_eq!(diagnostics["difficulty"], "easy");
    assert_eq!(diagnostics["computed_dc"], 20);
    assert_eq!(diagnostics["dc_strategy"], "entertainment_range");

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].metadata["dice_expression"], "2d20+4");
    assert_eq!(requests[0].metadata["dice_minimum"], "6");
    assert_eq!(requests[0].metadata["dice_maximum"], "44");
    assert!(
        requests[0]
            .messages
            .iter()
            .all(|message| !message.content.contains("投掷：")),
        "actual roll result must not enter the AI request"
    );
}

#[tokio::test]
async fn core_roll_dm_short_request_timeout_keeps_local_fallback() {
    let provider = TestProvider::delayed(
        r#"{"type":"fortune","check_name":"命运检定","difficulty":"medium","success_meaning":"出门","failure_meaning":"宅家"}"#,
        Duration::from_secs(2),
    );
    let state = test_state(provider.clone(), 1);
    let service = CoreHandle::new(state);
    let started_at = std::time::Instant::now();

    let CoreRespondOutput::Complete(response) = service
        .respond(private_request("/roll 今晚出门吗"))
        .await
        .unwrap()
    else {
        panic!("timed out roll DM should complete with a local fallback");
    };

    assert!(started_at.elapsed() < Duration::from_millis(1500));
    assert!(
        response
            .text_content()
            .is_some_and(|text| text.contains("本次仅进行普通 D20 投掷"))
    );
    let diagnostics = response
        .diagnostics
        .as_ref()
        .expect("fallback should expose diagnostics");
    assert_eq!(diagnostics["roll_execution_kind"], "ai_dm_fallback");
    assert_eq!(diagnostics["roll_provider"], "test-provider");
    assert_eq!(diagnostics["roll_model"], "test-model");
    assert!(diagnostics["roll_total_latency_ms"].as_u64().unwrap() >= 700);
    assert_eq!(diagnostics["roll_fallback_reason"], "timeout");
    assert_eq!(diagnostics["roll_fallback_stage"], "roll_dm");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_roll_dm_coc_provider_error_keeps_fallback_copy_on_d100() {
    let provider = TestProvider::failing(LlmError::provider("roll unavailable", "provider"));
    let state = test_state(provider.clone(), 5);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Complete(set_response) =
        service.respond(private_request("/set coc")).await.unwrap()
    else {
        panic!("setting the CoC rule should complete synchronously");
    };
    assert_eq!(set_response.command.as_deref(), Some("set"));

    let CoreRespondOutput::Complete(response) = service
        .respond(private_request("/roll 今晚出门吗"))
        .await
        .unwrap()
    else {
        panic!("failed CoC roll DM should complete with a local fallback");
    };

    let text = response
        .text_content()
        .expect("CoC fallback should return text");
    assert!(text.contains("本次仅进行普通 D100 投掷"), "{text}");
    assert!(!text.contains("D20"), "{text}");
    assert!(text.contains(" / 100"), "{text}");
    assert_eq!(response.command.as_deref(), Some("roll"));
    let diagnostics = response
        .diagnostics
        .as_ref()
        .expect("CoC fallback should expose diagnostics");
    assert_eq!(diagnostics["roll_execution_kind"], "ai_dm_fallback");
    assert_eq!(diagnostics["roll_fallback_reason"], "provider_error");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
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
