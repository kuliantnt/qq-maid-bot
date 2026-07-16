use super::*;

struct DeadlineRecordingTool {
    remaining: Arc<StdMutex<Option<std::time::Duration>>>,
}

#[async_trait]
impl crate::tool::Tool for DeadlineRecordingTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "deadline_probe".to_owned(),
            description: "record tool deadline".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(
        &self,
        context: ToolContext,
        _arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        *self.remaining.lock().unwrap() = context
            .execution_deadline
            .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()));
        Ok(ToolOutput::json(json!({"ok": true})))
    }
}

#[tokio::test]
async fn remaining_budget_forces_final_round_without_more_tools() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(SlowReadOnlyTool {
        calls: calls.clone(),
        delay: std::time::Duration::from_millis(28),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            final_reply("已有结果的简短收尾"),
        ],
    ));
    let observed = session.observed.clone();
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(30));

    let outcome = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "已有结果的简短收尾");
    assert_eq!(*calls.lock().unwrap(), 1);
    let observed = observed.lock().unwrap();
    assert!(observed[0].1);
    assert!(!observed[1].1);
}

#[tokio::test]
async fn tool_context_deadline_excludes_final_answer_reserve() {
    let remaining = Arc::new(StdMutex::new(None));
    let registry = registry_with(vec![Arc::new(DeadlineRecordingTool {
        remaining: remaining.clone(),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("deadline_probe", "c1", "{}")]),
            final_reply("完成收尾"),
        ],
    ));

    let outcome = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(AgentRunHandle::with_timeout(
            std::time::Duration::from_millis(400),
        )),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "完成收尾");
    let remaining = remaining.lock().unwrap().expect("missing tool deadline");
    assert!(remaining <= std::time::Duration::from_millis(300));
    assert!(remaining > std::time::Duration::from_millis(100));
}

#[tokio::test]
async fn failed_tool_entering_finalization_reserve_stops_without_another_model_round() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(SlowFailingReadOnlyTool {
        calls: calls.clone(),
        delay: std::time::Duration::from_millis(320),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            final_reply("must not run"),
        ],
    ));
    let observed = session.observed.clone();
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(400));

    let err = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "request_budget_reserved_for_final_answer");
    assert_eq!(err.stage, "tool_loop");
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(observed.lock().unwrap().len(), 1);
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.executed_tools, ["search"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(
        diagnostics
            .tool_results
            .iter()
            .all(|result| !result.succeeded)
    );
}

#[tokio::test]
async fn finalization_budget_rejects_provider_tool_calls_without_another_round() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(SlowReadOnlyTool {
        calls: calls.clone(),
        delay: std::time::Duration::from_millis(65),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            tool_calls(vec![tool_call("search", "c2", r#"{"value":"again"}"#)]),
            final_reply("must not run"),
        ],
    ));
    let observed = session.observed.clone();
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(80));

    let err = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "tool_calls_disabled");
    assert_eq!(err.stage, "tool_loop");
    assert_eq!(*calls.lock().unwrap(), 1);
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 2);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Failed));
    assert_eq!(diagnostics.executed_tools, ["search"]);
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert!(!observed[1].1);
}

#[tokio::test]
async fn model_round_exhausting_tool_budget_rejects_first_tool_without_waiting_for_it() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(SlowReadOnlyTool {
        calls: calls.clone(),
        delay: std::time::Duration::from_secs(5),
    }) as _]);
    let session = Box::new(ScriptedSession::with_delays(
        "mock",
        "m",
        vec![tool_calls(vec![tool_call(
            "search",
            "c1",
            r#"{"value":"rust"}"#,
        )])],
        vec![std::time::Duration::from_millis(320)],
    ));
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(400));

    let err = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_agent_loop_with_handle(
            session,
            registry,
            test_context(),
            3,
            None,
            None,
            Some(handle),
        ),
    )
    .await
    .expect("tool timeout must not be awaited")
    .unwrap_err();

    assert_eq!(err.code, "request_budget_reserved_for_final_answer");
    assert_eq!(err.stage, "tool_loop");
    assert_eq!(*calls.lock().unwrap(), 0);
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert!(diagnostics.executed_tools.is_empty());
    assert!(diagnostics.tool_results.is_empty());
}

#[tokio::test]
async fn finalization_reserve_between_read_only_tools_skips_rest_and_forces_final_answer() {
    let first_calls = Arc::new(StdMutex::new(0));
    let second_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(NamedSlowReadOnlyTool {
            name: "first_search",
            calls: first_calls.clone(),
            delay: std::time::Duration::from_millis(320),
        }) as _,
        Arc::new(NamedSlowReadOnlyTool {
            name: "second_search",
            calls: second_calls.clone(),
            delay: std::time::Duration::ZERO,
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("first_search", "c1", r#"{"value":"first"}"#),
                tool_call("second_search", "c2", r#"{"value":"second"}"#),
            ]),
            final_reply("基于第一项结果收尾"),
        ],
    ));
    let observed = session.observed.clone();
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(400));

    let outcome = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "基于第一项结果收尾");
    assert_eq!(*first_calls.lock().unwrap(), 1);
    assert_eq!(*second_calls.lock().unwrap(), 0);
    assert_eq!(outcome.agent.executed_tools, ["first_search"]);
    let observed = observed.lock().unwrap();
    assert!(!observed[1].1);
    let skipped: Value = serde_json::from_str(&observed[1].0[1].output).unwrap();
    assert_eq!(skipped["ok"], false);
    assert_eq!(skipped["skipped"], true);
    assert_eq!(
        skipped["reason"],
        "request_budget_reserved_for_final_answer"
    );
}

#[tokio::test]
async fn finalization_reserve_after_query_prevents_side_effecting_tool_start() {
    let query_calls = Arc::new(StdMutex::new(0));
    let write_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(NamedSlowReadOnlyTool {
            name: "query",
            calls: query_calls.clone(),
            delay: std::time::Duration::from_millis(320),
        }) as _,
        Arc::new(CountingTool {
            name: "write",
            calls: write_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("query", "c1", r#"{"value":"read"}"#),
                tool_call("write", "c2", r#"{"value":"must-not-write"}"#),
            ]),
            final_reply("只基于查询结果收尾"),
        ],
    ));
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(400));

    let outcome = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap();

    assert_eq!(*query_calls.lock().unwrap(), 1);
    assert_eq!(*write_calls.lock().unwrap(), 0);
    assert_eq!(outcome.agent.executed_tools, ["query"]);
    assert!(outcome.agent.side_effecting_tools_started.is_empty());
}

#[tokio::test]
async fn read_only_cache_hit_replays_at_budget_boundary_without_real_execution() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(SlowReadOnlyTool {
        calls: calls.clone(),
        delay: std::time::Duration::ZERO,
    }) as _]);
    let session = Box::new(ScriptedSession::with_delays(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            tool_calls(vec![tool_call("search", "c2", r#"{"value":"rust"}"#)]),
            final_reply("使用缓存结果收尾"),
        ],
        vec![
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(320),
            std::time::Duration::ZERO,
        ],
    ));
    let observed = session.observed.clone();
    let handle = AgentRunHandle::with_timeout(std::time::Duration::from_millis(400));

    let outcome = run_agent_loop_with_handle(
        session,
        registry,
        test_context(),
        3,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "使用缓存结果收尾");
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, ["search"]);
    assert_eq!(outcome.agent.tool_results.len(), 2);
    let observed = observed.lock().unwrap();
    assert_eq!(observed[1].0[0].output, observed[2].0[0].output);
    assert!(!observed[2].1);
}

#[tokio::test]
async fn streaming_advance_timeout_before_visible_delta_falls_back_once() {
    let mut session = StreamingSession::new(
        StreamingAction::HangBeforeDelta,
        vec![final_reply("fallback after timeout")],
    );
    let advance_calls = session.advance_calls.clone();

    let advance = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(50),
        0,
    )
    .await
    .unwrap();

    let AgentStep::FinalAnswer { reply, .. } = advance.step else {
        panic!("expected fallback final answer");
    };
    assert_eq!(reply, "fallback after timeout");
    assert!(advance.fallback_used);
    assert_eq!(*advance_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn streaming_advance_timeout_after_visible_delta_does_not_fallback() {
    let mut session = StreamingSession::new(
        StreamingAction::HangAfterDelta { delta: "半句" },
        vec![final_reply("fallback must not run")],
    );
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        false,
        Some(delta_sink(deltas.clone())),
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(50),
        0,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "agent_stream_after_delta");
    assert_eq!(*deltas.lock().unwrap(), vec!["半句".to_owned()]);
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn non_stream_timeout_returns_structured_agent_failure() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);

    let err = super::runner::run_agent_loop_with_timeouts(
        Box::new(HangingSession),
        registry,
        test_context(),
        3,
        None,
        None,
        None,
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(10),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(!diagnostics.streaming_fallback_used);
}

#[tokio::test]
async fn streaming_first_activity_timeout_and_fallback_timeout_keep_diagnostics() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);

    let err = super::runner::run_agent_loop_with_timeouts(
        Box::new(HangingSession),
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        None,
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(10),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(diagnostics.streaming_fallback_used);
}

#[tokio::test]
async fn max_rounds_returns_tool_loop_limit_without_executing_last_batch() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    // max_rounds=1：round 0 执行一次；round 1 仍要求工具调用 → 超限。
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            tool_calls(vec![tool_call("echo", "c2", r#"{"value":"b"}"#)]),
        ],
    ));
    let err = run_agent_loop(session, registry, test_context(), 1, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "tool_loop_limit");
    assert_eq!(err.stage, "tool_loop");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 2);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::MaxRounds));
    assert_eq!(diagnostics.emitted_tools, vec!["echo", "echo"]);
    assert_eq!(diagnostics.executed_tools, vec!["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    // 第二批未执行。
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn last_round_uses_allow_tool_calls_false() {
    // max_rounds=1：round 0 allow=true，round 1 allow=false。
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let session = ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("ok"),
        ],
    );
    let observed_inner = session.observed.clone();
    let outcome = run_agent_loop(Box::new(session), registry, test_context(), 1, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "ok");
    let recorded = observed_inner.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].1); // round 0 allow=true
    assert!(!recorded[1].1); // round 1 allow=false
}

#[tokio::test]
async fn empty_tools_rejected_before_any_request() {
    let session = Box::new(ScriptedSession::new("mock", "m", vec![final_reply("x")]));
    let err = run_agent_loop(session, ToolRegistry::new(), test_context(), 3, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_request");
    assert_eq!(err.stage, "tool_loop");
}

#[tokio::test]
async fn zero_max_rounds_rejected() {
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let session = Box::new(ScriptedSession::new("mock", "m", vec![final_reply("x")]));
    let err = run_agent_loop(session, registry, test_context(), 0, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_request");
    assert_eq!(err.stage, "tool_loop");
}

#[tokio::test]
async fn usage_merges_across_rounds() {
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            AgentStep::ToolCalls {
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    cached_input_tokens: None,
                    output_tokens: Some(3),
                    total_tokens: Some(13),
                }),
            },
            AgentStep::FinalAnswer {
                reply: "ok".to_owned(),
                usage: Some(TokenUsage {
                    input_tokens: Some(8),
                    cached_input_tokens: Some(2),
                    output_tokens: Some(4),
                    total_tokens: Some(12),
                }),
            },
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    let usage = outcome.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(18));
    assert_eq!(usage.cached_input_tokens, Some(2));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(25));
}
