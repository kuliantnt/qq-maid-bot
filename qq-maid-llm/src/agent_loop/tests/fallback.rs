use super::*;

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

    let advance = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
        0,
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
