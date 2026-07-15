use super::*;

#[tokio::test]
async fn core_private_weather_chat_with_tool_capability_completes_without_synthetic_delta() {
    let provider = TestProvider::replying("工具完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );
    assert_eq!(
        stream.output_policy(),
        CoreOutputPolicy::ProgressThenComplete
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::AgentStarted);
    assert_eq!(status.text, "小女仆正在查天气…");

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool call started status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::ToolCallStarted);
    assert_eq!(status.text, "小女仆正在查天气…");

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool call finished status");
    };
    assert_eq!(status.kind, CoreResponseStatusKind::ToolCallFinished);
    assert_eq!(status.text, "小女仆正在确认结果…");

    let response = collect_completed_without_text_delta(&mut stream).await;

    assert_eq!(response.text_content(), Some("工具完整回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_private_tool_status_uses_configured_display_name() {
    let provider = TestProvider::replying("工具完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let mut state = test_state_with_tool_calling(provider.clone(), 5, true);
    state.config.bot_display_name = "助手".to_owned();
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };

    assert_eq!(status.kind, CoreResponseStatusKind::AgentStarted);
    assert_eq!(status.text, "助手正在查天气…");
}

#[tokio::test]
async fn core_group_tool_status_uses_short_hint() {
    let provider = TestProvider::replying("群聊工具回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_group_tool_calling(provider.clone(), 5, true, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(group_request("杭州明天要带伞吗"))
            .await
            .unwrap(),
    );

    let Some(CoreResponseEvent::Status(status)) = stream.recv().await else {
        panic!("expected tool loop started status");
    };

    assert_eq!(status.kind, CoreResponseStatusKind::AgentStarted);
    assert_eq!(status.text, "正在查…");
}

#[tokio::test]
async fn core_tool_loop_streams_only_final_answer_after_tool_status() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("最终".to_owned())),
        Ok(LlmStreamEvent::TextDelta("回答".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ])
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("帮我新增待办"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::ProgressThenStream);

    let mut status_kinds = Vec::new();
    let mut deltas = Vec::new();
    let response = loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before completed response");
        };
        match event {
            CoreResponseEvent::Status(status) => status_kinds.push(status.kind),
            CoreResponseEvent::Completed(response) => break response,
            CoreResponseEvent::TextDelta(delta) => deltas.push(delta),
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    };

    assert_eq!(deltas, vec!["最终".to_owned(), "回答".to_owned()]);
    assert_eq!(response.text_content(), Some("最终回答"));
    assert!(status_kinds.contains(&CoreResponseStatusKind::AgentStarted));
    assert!(status_kinds.contains(&CoreResponseStatusKind::AgentFinalizing));
    assert!(
        status_kinds
            .iter()
            .position(|kind| *kind == CoreResponseStatusKind::AgentFinalizing)
            .is_some_and(|index| index > 0)
    );
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_loop_stream_failure_after_delta_does_not_complete_or_replay() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("部分回答".to_owned())),
        Err(LlmError::provider(
            "stream failed after visible delta",
            "stream_after_delta",
        )),
    ])
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("帮我查一下天气"))
            .await
            .unwrap(),
    );

    let mut saw_delta = false;
    loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before failure");
        };
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::TextDelta(delta) => {
                assert_eq!(delta, "部分回答");
                saw_delta = true;
            }
            CoreResponseEvent::Failed(failure) => {
                assert!(saw_delta);
                assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
                break;
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response after partial delta: {response:?}");
            }
        }
    }
}

#[tokio::test]
async fn core_tool_loop_timeout_after_delta_appends_visible_termination_notice() {
    let provider = TestProvider::partial_then_delayed("部分回答", Duration::from_secs(2))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let mut state = test_state_with_tool_calling(provider, 1, true);
    state.config.request_timeout_seconds = 1;
    let service = CoreHandle::new(state);
    let started_at = std::time::Instant::now();
    let mut stream = expect_stream(
        service
            .respond(private_request("帮我处理一下"))
            .await
            .unwrap(),
    );

    let mut deltas = Vec::new();
    let failure = loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before failure");
        };
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::TextDelta(delta) => deltas.push(delta),
            CoreResponseEvent::Failed(failure) => break failure,
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response after timeout: {response:?}");
            }
        }
    };

    assert_eq!(failure.kind, CoreFailureKind::LlmTimeout);
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0], "部分回答");
    assert!(deltas[1].contains("本次回答未完整完成"));
    assert!(started_at.elapsed() < Duration::from_secs(3));
    assert!(stream.recv().await.is_none());
}

#[tokio::test]
async fn core_tool_loop_failure_is_reported_as_stream_failure_without_delta() {
    let provider = TestProvider::failing(LlmError::new(
        "tool_loop_limit",
        "tool loop exceeded maximum rounds",
        "tool_intent",
    ))
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let CoreRespondOutput::Stream(mut stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };
    let failure = collect_failure_without_text_delta(&mut stream).await;

    assert_eq!(failure.kind, CoreFailureKind::Internal);
    assert!(!failure.retryable);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_loop_stream_preserves_request_timeout_for_background_complete_path() {
    let provider = TestProvider::delayed("迟到回复", Duration::from_millis(80))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 0, true);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };

    let failure = collect_failure_without_text_delta(&mut stream).await;

    assert_eq!(failure.kind, CoreFailureKind::LlmTimeout);
    assert!(failure.retryable);
    let diagnostics = failure.agent.expect("missing agent diagnostics");
    assert_eq!(
        diagnostics.stop_reason,
        Some(qq_maid_llm::agent_loop::AgentStopReason::Timeout)
    );
}

#[tokio::test]
async fn core_timeout_during_read_only_tool_aborts_quickly_and_stops_next_model_round() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = TestProvider::agent_weather_then_final()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let mut state = test_state_with_tool_calling(provider, 1, true);
    state.executors.weather_executor = Arc::new(BlockingWeatherExecutor {
        started: started.clone(),
        release: release.clone(),
        calls: calls.clone(),
    });
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(mut stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };

    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("tool did not start");
    let timeout_started = std::time::Instant::now();
    let failure = tokio::time::timeout(
        Duration::from_secs(2),
        collect_failure_without_text_delta(&mut stream),
    )
    .await
    .expect("read-only timeout cleanup must not wait for the tool timeout");

    assert_eq!(failure.kind, CoreFailureKind::LlmTimeout);
    let diagnostics = failure.agent.expect("missing agent diagnostics");
    assert_eq!(
        diagnostics.stop_reason,
        Some(qq_maid_llm::agent_loop::AgentStopReason::Timeout)
    );
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.executed_tools, ["get_weather"]);
    assert!(diagnostics.tool_results.is_empty());
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(timeout_started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn core_agent_stream_cancel_marks_shared_agent_diagnostics() {
    let provider = TestProvider::delayed("不应完成", Duration::from_secs(1))
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider, 5, true);
    let service = CoreHandle::new(state);
    let CoreRespondOutput::Stream(stream) = service
        .respond(private_request("杭州明天要带伞吗"))
        .await
        .unwrap()
    else {
        panic!("expected stream output");
    };

    stream.cancel();

    assert!(stream.is_cancelled());
    let diagnostics = stream
        .agent_diagnostics()
        .expect("missing agent diagnostics");
    assert_eq!(
        diagnostics.stop_reason,
        Some(qq_maid_llm::agent_loop::AgentStopReason::Cancelled)
    );
}

#[tokio::test]
async fn core_private_simple_todo_queries_use_deterministic_path() {
    let provider = TestProvider::replying("不应调用模型")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let owner = TodoStore::owner(Some("u1"), private_scope());
    let todo_store = state.stores.todo_store.clone();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "待查看项目".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let service = CoreHandle::new(state);

    let mut responses = Vec::new();
    for input in ["看一下待办", "看一下代办"] {
        let output = service.respond(private_request(input)).await.unwrap();
        let CoreRespondOutput::Complete(response) = output else {
            panic!("expected complete output for deterministic todo query");
        };
        let response = *response;
        assert_eq!(response.command.as_deref(), Some("todo_list"), "{input}");
        assert!(response.text_content().unwrap().contains("待查看项目"));
        responses.push(response.text_content().map(str::to_owned));
    }

    assert_eq!(responses[0], responses[1]);
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_private_general_chat_with_tool_capability_uses_agent_runtime() {
    let provider = TestProvider::replying("普通完整回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("晚上好")).await).await;

    assert_eq!(response.text_content(), Some("普通完整回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_private_general_chat_agent_direct_answer_streams_text_deltas() {
    let provider = TestProvider::streaming(vec![
        Ok(LlmStreamEvent::TextDelta("普通".to_owned())),
        Ok(LlmStreamEvent::TextDelta("回复".to_owned())),
        Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        }),
    ])
    .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
    .without_tool_progress();
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let mut stream = expect_stream(
        service
            .respond(private_request("聊聊 Rust 的所有权"))
            .await
            .unwrap(),
    );
    assert_eq!(stream.output_policy(), CoreOutputPolicy::ProgressThenStream);

    let mut statuses = Vec::new();
    let mut deltas = Vec::new();
    let response = loop {
        let Some(event) = stream.recv().await else {
            panic!("stream ended before completed response");
        };
        match event {
            CoreResponseEvent::Status(status) => statuses.push(status.kind),
            CoreResponseEvent::TextDelta(delta) => deltas.push(delta),
            CoreResponseEvent::Completed(response) => break response,
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    };

    assert!(statuses.is_empty());
    assert_eq!(deltas, vec!["普通".to_owned(), "回复".to_owned()]);
    assert_eq!(response.text_content(), Some("普通回复"));
    let diagnostics = response.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics["tool_calling_available"], true);
    assert_eq!(diagnostics["tool_calling_used"], false);
    assert_eq!(diagnostics["agent_result"], "direct_answer");
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_group_chat_uses_memory_only_agent_path_when_full_loop_is_disabled() {
    let provider = TestProvider::replying("群聊普通回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response =
        collect_stream_completed(service.respond(group_request("群里问天气")).await).await;

    assert_eq!(response.text_content(), Some("群聊普通回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_slash_command_does_not_enter_tool_loop() {
    let provider =
        TestProvider::replying("unused").with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let output = service
        .respond(private_request("/天气 杭州"))
        .await
        .unwrap();

    assert_eq!(output.output_policy(), CoreOutputPolicy::CompleteThenSend);
    assert!(matches!(output, CoreRespondOutput::Complete(_)));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn core_tool_calling_disabled_keeps_plain_stream_path() {
    let provider = TestProvider::replying("普通流式回复")
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let state = test_state_with_tool_calling(provider.clone(), 5, false);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("普通流式回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn core_unsupported_provider_capability_keeps_plain_stream_path() {
    let provider = TestProvider::replying("未适配 provider 普通回复");
    let state = test_state_with_tool_calling(provider.clone(), 5, true);
    let service = CoreHandle::new(state);

    let response = collect_stream_completed(service.respond(private_request("hello")).await).await;

    assert_eq!(response.text_content(), Some("未适配 provider 普通回复"));
    assert_eq!(provider.tool_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
