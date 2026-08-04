use super::*;

struct BufferedTextErrorSession {
    advance_calls: Arc<StdMutex<usize>>,
}

#[async_trait]
impl AgentStepSession for BufferedTextErrorSession {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "m"
    }

    fn streaming_diagnostics(&self) -> AgentStreamingDiagnostics {
        AgentStreamingDiagnostics {
            fallback_reason: Some("sse_parse_error".to_owned()),
            stream_end_kind: Some("sse_parse_error".to_owned()),
            parse_error: true,
            saw_text_delta: true,
            buffered_delta_count: 2,
            buffered_text_chars: 4,
            ..Default::default()
        }
    }

    async fn advance(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        *self.advance_calls.lock().unwrap() += 1;
        Ok(final_reply("must not regenerate"))
    }

    async fn advance_streaming(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
        _text_delta_sink: AgentTextDeltaSink,
    ) -> Result<Option<AgentStep>, LlmError> {
        Err(LlmError::provider("invalid SSE after text", "sse"))
    }
}

#[tokio::test]
async fn fallback_after_tool_result_does_not_repeat_tool_side_effect() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::scripted(
        vec![
            StreamingAction::ToolCallsWithBufferedDraft {
                draft_delta: "不外显",
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
            },
            StreamingAction::ErrorBeforeDelta,
        ],
        vec![final_reply("fallback summary")],
    ));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback summary");
    assert!(outcome.fallback_used);
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, vec!["echo"]);
}

#[tokio::test]
async fn streaming_advance_error_before_visible_delta_falls_back() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::ErrorBeforeDelta,
        vec![final_reply("fallback")],
    ));
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback");
    assert!(deltas.lock().unwrap().is_empty());
    assert_eq!(*advance_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn unsupported_streaming_advance_falls_back_without_marking_failure() {
    let mut session = ScriptedSession::new("mock", "m", vec![final_reply("fallback")]);

    let advance = crate::agent_loop::streaming::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        crate::agent_loop::streaming::StreamingAdvanceOptions {
            final_delta_sink: Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
            streaming_timeout: std::time::Duration::from_millis(50),
            non_stream_timeout: std::time::Duration::from_millis(50),
            round: 0,
        },
        &AgentRunHandle::default(),
    )
    .await
    .unwrap();

    assert!(!advance.fallback_used);
    assert!(matches!(advance.step, AgentStep::FinalAnswer { .. }));
}

#[tokio::test]
async fn streaming_advance_error_after_visible_delta_does_not_fallback() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::ErrorAfterDelta { delta: "半句" },
        vec![final_reply("fallback must not run")],
    ));
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert_eq!(*deltas.lock().unwrap(), vec!["半句".to_owned()]);
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn buffered_text_error_does_not_regenerate_same_provider_non_stream() {
    let advance_calls = Arc::new(StdMutex::new(0));
    let mut session = BufferedTextErrorSession {
        advance_calls: advance_calls.clone(),
    };

    let err = crate::agent_loop::streaming::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        crate::agent_loop::streaming::StreamingAdvanceOptions {
            final_delta_sink: Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
            streaming_timeout: std::time::Duration::from_millis(50),
            non_stream_timeout: std::time::Duration::from_millis(50),
            round: 0,
        },
        &AgentRunHandle::default(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "sse");
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}
