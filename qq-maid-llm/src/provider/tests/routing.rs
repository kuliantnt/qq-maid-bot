use super::*;

#[test]
fn provider_errors_are_fallback_eligible() {
    assert!(should_try_next_model(&LlmError::provider(
        "upstream failed",
        "provider"
    )));
    assert!(should_try_next_model(&LlmError::provider(
        "provider missing key",
        "provider_unavailable"
    )));
    assert!(should_try_next_model(&LlmError::timeout("request")));
    assert!(!should_try_next_model(&LlmError::config("missing key")));
    assert!(!should_try_next_model(&LlmError::new(
        "bad_request",
        "bad local request",
        "request"
    )));
}

#[tokio::test]
async fn model_route_provider_uses_first_successful_candidate() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Ok(outcome("primary"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "primary");
    assert!(!result.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(openai.requests()[0].model.as_deref(), Some("openai:gpt-a"));
}

#[tokio::test]
async fn model_route_provider_skips_unavailable_candidate_provider() {
    let (provider, openai, deepseek) = route_provider(
        "bigmodel:glm-5.2,deepseek:deepseek-chat",
        vec![Ok(outcome("should not be used"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        deepseek.requests()[0].model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_tool_calling_uses_declared_protocol() {
    let openai = Arc::new(
        MockProvider::new("openai", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_results(vec![Ok(outcome("tool reply"))]),
    );
    let deepseek = Arc::new(MockProvider::new(
        "deepseek",
        vec![Ok(outcome("should not be used"))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool reply");
    assert_eq!(openai.calls(), 0);
    assert_eq!(openai.tool_calls(), 1);
    assert_eq!(
        openai.tool_requests()[0].chat.model.as_deref(),
        Some("openai:gpt-a")
    );
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 0);
}

#[tokio::test]
async fn model_route_tool_calling_skips_unavailable_candidate_provider() {
    let openai = Arc::new(MockProvider::new(
        "openai",
        vec![Ok(outcome("should not be used"))],
    ));
    let deepseek = Arc::new(
        MockProvider::new("deepseek", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
            .with_tool_results(vec![Ok(outcome("tool fallback"))]),
    );
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("bigmodel:glm-5.2,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool fallback");
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 1);
    assert_eq!(
        deepseek.tool_requests()[0].chat.model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_tool_calling_falls_back_after_eligible_candidate_error() {
    let openai = Arc::new(
        MockProvider::new("openai", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_results(vec![Err(LlmError::new(
                "http_error",
                "upstream unavailable",
                "http",
            ))]),
    );
    let deepseek = Arc::new(
        MockProvider::new("deepseek", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
            .with_tool_results(vec![Ok(outcome("tool fallback"))]),
    );
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.tool_calls(), 1);
    assert_eq!(deepseek.tool_calls(), 1);
    assert_eq!(
        deepseek.tool_requests()[0].chat.model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn tool_candidate_failure_then_timeout_uses_latest_terminal_reason() {
    let (provider, first, second) =
        handle_route_provider(HandleBehavior::Failed, HandleBehavior::Timeout);
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let err = provider.chat_with_tools(req).await.unwrap_err();

    assert_eq!(
        err.agent.unwrap().stop_reason,
        Some(AgentStopReason::Timeout)
    );
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn tool_candidate_failure_then_cancelled_uses_latest_terminal_reason() {
    let (provider, first, second) =
        handle_route_provider(HandleBehavior::Failed, HandleBehavior::Cancelled);
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let err = provider.chat_with_tools(req).await.unwrap_err();

    assert_eq!(
        err.agent.unwrap().stop_reason,
        Some(AgentStopReason::Cancelled)
    );
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn tool_candidate_failure_then_success_keeps_request_counters() {
    let (provider, first, second) =
        handle_route_provider(HandleBehavior::Failed, HandleBehavior::Success);
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let outcome = provider.chat_with_tools(req).await.unwrap();

    assert_eq!(
        outcome.agent.stop_reason,
        Some(AgentStopReason::DirectAnswer)
    );
    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 1);
}

#[tokio::test]
async fn tool_candidate_does_not_fallback_after_tool_side_effect_started() {
    let (provider, first, second) = handle_route_provider(
        HandleBehavior::FailedAfterToolStart,
        HandleBehavior::Success,
    );
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let err = provider.chat_with_tools(req).await.unwrap_err();

    let diagnostics = err.agent.unwrap();
    assert_eq!(diagnostics.executed_tools, ["write_tool"]);
    assert_eq!(diagnostics.tools_with_unknown_result, ["write_tool"]);
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 0);
}

#[tokio::test]
async fn model_route_tool_calling_error_after_final_delta_does_not_fallback() {
    let openai = Arc::new(
        MockProvider::new("openai", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_deltas(vec!["partial"])
            .with_tool_results(vec![Err(LlmError::provider(
                "stream failed after visible delta",
                "stream_after_delta",
            ))]),
    );
    let deepseek = Arc::new(
        MockProvider::new("deepseek", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
            .with_tool_results(vec![Ok(outcome("tool fallback"))]),
    );
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let collected = deltas.clone();
    let mut req = tool_request();
    req.final_delta_sink = Some(Arc::new(move |delta| {
        let collected = collected.clone();
        Box::pin(async move {
            collected.lock().unwrap().push(delta);
            Ok(())
        }) as crate::agent_loop::AgentTextDeltaFuture
    }) as crate::agent_loop::AgentTextDeltaSink);

    let err = provider.chat_with_tools(req).await.unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert_eq!(deltas.lock().unwrap().as_slice(), ["partial"]);
    assert_eq!(openai.tool_calls(), 1);
    assert_eq!(deepseek.tool_calls(), 0);
}

#[tokio::test]
async fn model_route_tool_calling_falls_back_to_first_candidate_chat_when_unsupported() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Ok(outcome("plain reply"))],
        vec![Ok(outcome("fallback should not be used"))],
    );

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "plain reply");
    assert_eq!(openai.calls(), 1);
    assert_eq!(openai.tool_calls(), 0);
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 0);
    assert_eq!(openai.requests()[0].model.as_deref(), Some("openai:gpt-a"));
}

#[tokio::test]
async fn agent_candidate_failure_then_plain_candidate_keeps_shared_diagnostics() {
    let first = Arc::new(HandleAwareProvider::new("openai", HandleBehavior::Failed));
    let second = Arc::new(MockProvider::new(
        "deepseek",
        vec![Ok(outcome("plain fallback"))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, first.clone()),
            (ModelProvider::DeepSeek, second.clone()),
        ],
    )
    .unwrap();
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let result = provider.chat_with_tools(req).await.unwrap();

    assert_eq!(result.reply, "plain fallback");
    assert!(result.fallback_used);
    assert_eq!(result.agent.model_rounds, 2);
    assert_eq!(result.agent.emitted_tools, ["lookup_tool"]);
    assert_eq!(
        result.agent.stop_reason,
        Some(AgentStopReason::DirectAnswer)
    );
    assert_eq!(first.calls(), 1);
    assert_eq!(second.calls(), 1);
    assert_eq!(second.tool_calls(), 0);
}

#[tokio::test]
async fn default_plain_chat_fallback_uses_shared_agent_diagnostics() {
    let handle = AgentRunHandle::default();
    handle.update(|diagnostics| {
        diagnostics.model_rounds = 1;
        diagnostics.emitted_tools.push("earlier_tool".to_owned());
        diagnostics.stop_reason = Some(AgentStopReason::Failed);
    });
    let mut req = tool_request();
    req.run_handle = Some(handle);

    let result = PlainDefaultProvider.chat_with_tools(req).await.unwrap();

    assert_eq!(result.reply, "plain default reply");
    assert_eq!(result.agent.model_rounds, 2);
    assert_eq!(result.agent.emitted_tools, ["earlier_tool"]);
    assert_eq!(
        result.agent.stop_reason,
        Some(AgentStopReason::DirectAnswer)
    );
}

#[tokio::test]
async fn cancellation_during_default_plain_chat_cannot_return_agent_success() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = BlockingPlainProvider {
        started: started.clone(),
        release: release.clone(),
    };
    let handle = AgentRunHandle::default();
    let mut req = tool_request();
    req.run_handle = Some(handle.clone());
    let task = tokio::spawn(async move { provider.chat_with_tools(req).await });

    started.notified().await;
    handle.cancel(AgentStopReason::Timeout);
    release.notify_one();

    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
}

#[tokio::test]
async fn agent_session_creation_failure_preserves_error_and_adds_diagnostics() {
    let mut req = tool_request();
    req.run_handle = Some(AgentRunHandle::default());

    let err = SessionCreationFailureProvider
        .chat_with_tools(req)
        .await
        .unwrap_err();

    assert_eq!(err.code, "provider_error");
    assert_eq!(err.stage, "agent_session");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 0);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Failed));
}

#[test]
fn final_agent_error_does_not_overwrite_external_termination() {
    let handle = AgentRunHandle::default();
    handle.cancel(AgentStopReason::Timeout);

    let err = finish_agent_error(
        LlmError::new("provider_error", "failed", "agent_session"),
        &handle,
        AgentStopReason::Failed,
    );

    assert_eq!(err.code, "provider_error");
    assert_eq!(err.stage, "agent_session");
    assert_eq!(
        err.agent.unwrap().stop_reason,
        Some(AgentStopReason::Timeout)
    );

    let handle = AgentRunHandle::default();
    handle.set_stop_reason(AgentStopReason::MaxRounds);
    let err = finish_agent_error(
        LlmError::new("tool_loop_limit", "exhausted", "tool_loop"),
        &handle,
        AgentStopReason::Failed,
    );
    assert_eq!(
        err.agent.unwrap().stop_reason,
        Some(AgentStopReason::MaxRounds)
    );
}

#[tokio::test]
async fn model_route_provider_falls_back_on_eligible_error() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::timeout("provider"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        deepseek.requests()[0].model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_provider_falls_back_after_mimo_rate_limit() {
    let mimo_id = ModelProvider::Custom("mimo".to_owned());
    let mimo = Arc::new(MockProvider::new(
        "mimo",
        vec![Err(LlmError::new(
            "rate_limited",
            "too many requests",
            "http",
        ))],
    ));
    let deepseek = Arc::new(MockProvider::new(
        "deepseek",
        vec![Ok(outcome("deepseek fallback"))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("mimo:mimo-v2.5-pro,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (mimo_id, mimo.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "deepseek fallback");
    assert!(result.fallback_used);
    assert_eq!(mimo.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        mimo.requests()[0].model.as_deref(),
        Some("mimo:mimo-v2.5-pro")
    );
}

#[tokio::test]
async fn model_route_stream_falls_back_before_delta_only() {
    let openai = Arc::new(MockProvider::with_streams(
        "openai",
        vec![Ok(stream_events(vec![Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        })]))],
    ));
    let deepseek = Arc::new(MockProvider::with_streams(
        "deepseek",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("fallback".to_owned())),
            Ok(LlmStreamEvent::Completed {
                usage: None,
                finish_reason: None,
                fallback_used: false,
            }),
        ]))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let outcome = collect_llm_stream(
        provider.stream_chat(request()).await.unwrap(),
        provider.name(),
        provider.model(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback");
    assert!(outcome.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
}

#[tokio::test]
async fn model_route_stream_error_after_delta_does_not_fallback() {
    let openai = Arc::new(MockProvider::with_streams(
        "openai",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("partial".to_owned())),
            Err(LlmError::provider("broken", "stream")),
        ]))],
    ));
    let deepseek = Arc::new(MockProvider::with_streams(
        "deepseek",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("fallback".to_owned())),
            Ok(LlmStreamEvent::Completed {
                usage: None,
                finish_reason: None,
                fallback_used: false,
            }),
        ]))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let err = collect_llm_stream(
        provider.stream_chat(request()).await.unwrap(),
        provider.name(),
        provider.model(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
}

#[tokio::test]
async fn model_route_provider_keeps_permanent_error() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::config("missing key"))],
        vec![Ok(outcome("fallback"))],
    );

    let err = provider.chat(request()).await.unwrap_err();

    assert_eq!(err.code, "config");
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
}

#[tokio::test]
async fn model_route_provider_aggregates_all_candidate_failures() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::timeout("provider"))],
        vec![Err(LlmError::provider("empty response", "provider"))],
    );

    let err = provider.chat(request()).await.unwrap_err();

    assert_eq!(err.code, "provider_error");
    assert_eq!(err.stage, "provider_route");
    assert!(err.message.contains("#0 openai:gpt-a -> timeout@provider"));
    assert!(
        err.message
            .contains("#1 deepseek:deepseek-chat -> provider_error@provider")
    );
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
}

#[tokio::test]
async fn model_route_provider_uses_request_route_override() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a",
        vec![Ok(outcome("primary"))],
        vec![Ok(outcome("deepseek"))],
    );
    let mut req = request();
    req.model = Some("deepseek:deepseek-chat".to_owned());

    let result = provider.chat(req).await.unwrap();

    assert_eq!(result.reply, "deepseek");
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.calls(), 1);
}
