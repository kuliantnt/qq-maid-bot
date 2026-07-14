use super::*;

#[tokio::test]
async fn no_tool_answer_completes_immediately() {
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
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![final_reply("你好呀")],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "你好呀");
    assert_eq!(outcome.agent.model_rounds, 1);
    assert_eq!(
        outcome.agent.stop_reason,
        Some(AgentStopReason::DirectAnswer)
    );
    assert!(outcome.agent.emitted_tools.is_empty());
    assert!(outcome.agent.executed_tools.is_empty());
    assert!(outcome.agent.tool_results.is_empty());
}

#[tokio::test]
async fn streaming_advance_final_answer_emits_real_deltas() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::Final {
            deltas: vec!["你", "好"],
            reply: "你好",
        },
        Vec::new(),
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

    assert_eq!(outcome.reply, "你好");
    assert_eq!(
        *deltas.lock().unwrap(),
        vec!["你".to_owned(), "好".to_owned()]
    );
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn streaming_tool_round_suppresses_draft_then_streams_final_answer() {
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
                draft_delta: "草稿不外显",
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
            },
            StreamingAction::Final {
                deltas: vec!["最终", "回答"],
                reply: "最终回答",
            },
        ],
        Vec::new(),
    ));
    let advance_calls = session.advance_calls.clone();
    let buffered_drafts = session.buffered_drafts.clone();
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

    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, vec!["echo".to_owned()]);
    assert_eq!(outcome.reply, "最终回答");
    assert_eq!(
        *buffered_drafts.lock().unwrap(),
        vec!["草稿不外显".to_owned()]
    );
    assert_eq!(
        *deltas.lock().unwrap(),
        vec!["最终".to_owned(), "回答".to_owned()]
    );
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}
