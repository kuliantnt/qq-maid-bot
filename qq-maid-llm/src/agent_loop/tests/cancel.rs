use super::*;

#[tokio::test]
async fn shared_handle_cancel_interrupts_inflight_model_round() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        super::runner::run_agent_loop_with_timeouts(
            Box::new(HangingSession),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        )
        .await
    });
    while handle.snapshot().model_rounds == 0 {
        tokio::task::yield_now().await;
    }
    handle.cancel(AgentStopReason::Cancelled);

    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code, "cancelled");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Cancelled));
}

#[tokio::test]
async fn cancellation_while_tool_started_progress_is_blocked_prevents_tool_start() {
    let progress_started = Arc::new(Notify::new());
    let progress_release = Arc::new(Notify::new());
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let sink_started = progress_started.clone();
    let sink_release = progress_release.clone();
    let progress_sink = Arc::new(move |event| {
        let sink_started = sink_started.clone();
        let sink_release = sink_release.clone();
        Box::pin(async move {
            if matches!(event, ToolLoopProgressEvent::ToolCallStarted { .. }) {
                sink_started.notify_one();
                sink_release.notified().await;
            }
            Ok(())
        }) as ToolLoopProgressFuture
    }) as ToolLoopProgressSink;
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![
                    tool_calls(vec![tool_call("echo", "c1", r#"{"value":"x"}"#)]),
                    final_reply("must not run"),
                ],
            )),
            registry,
            test_context(),
            3,
            Some(progress_sink),
            None,
            Some(task_handle),
        )
        .await
    });

    progress_started.notified().await;
    handle.cancel(AgentStopReason::Timeout);
    progress_release.notify_one();

    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(diagnostics.executed_tools.is_empty());
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn cancellation_during_tool_waits_for_result_and_stops_remaining_work() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let controlled_calls = Arc::new(StdMutex::new(0));
    let later_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(ControlledTool {
            started: started.clone(),
            release: release.clone(),
            calls: controlled_calls.clone(),
        }) as _,
        Arc::new(CountingTool {
            name: "later",
            calls: later_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _,
    ]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![
                    tool_calls(vec![
                        tool_call("controlled", "c1", "{}"),
                        tool_call("later", "c2", r#"{"value":"b"}"#),
                    ]),
                    final_reply("must not run"),
                ],
            )),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
        )
        .await
    });

    started.notified().await;
    let inflight = handle.snapshot();
    assert_eq!(inflight.executed_tools, ["controlled"]);
    assert_eq!(inflight.tools_with_unknown_result, ["controlled"]);
    assert!(inflight.tool_results.is_empty());
    handle.cancel(AgentStopReason::Timeout);
    release.notify_one();

    let err = task.await.unwrap().unwrap_err();
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.executed_tools, ["controlled"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*controlled_calls.lock().unwrap(), 1);
    assert_eq!(*later_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn cleanup_abort_after_started_tool_preserves_unknown_result() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(ControlledTool {
        started: started.clone(),
        release,
        calls: calls.clone(),
    }) as _]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![tool_calls(vec![tool_call("controlled", "c1", "{}")])],
            )),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
        )
        .await
    });

    started.notified().await;
    handle.cancel(AgentStopReason::Timeout);
    task.abort();
    let _ = task.await;

    let diagnostics = handle.snapshot();
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert_eq!(diagnostics.executed_tools, ["controlled"]);
    assert!(diagnostics.tool_results.is_empty());
    assert_eq!(diagnostics.tools_with_unknown_result, ["controlled"]);
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn new_candidate_attempt_clears_failed_but_external_termination_wins() {
    let handle = AgentRunHandle::default();
    handle.set_stop_reason(AgentStopReason::Failed);
    handle.begin_candidate_attempt().unwrap();
    assert_eq!(handle.snapshot().stop_reason, None);

    handle.cancel(AgentStopReason::Timeout);
    handle.set_stop_reason(AgentStopReason::Failed);
    assert_eq!(
        handle.snapshot().stop_reason,
        Some(AgentStopReason::Timeout)
    );
}

#[test]
fn cancel_and_begin_candidate_are_linearized_by_one_lifecycle_lock() {
    for reason in [AgentStopReason::Timeout, AgentStopReason::Cancelled] {
        for _ in 0..128 {
            let handle = AgentRunHandle::default();
            handle.set_stop_reason(AgentStopReason::Failed);
            let barrier = Arc::new(Barrier::new(3));
            let cancel_handle = handle.clone();
            let cancel_barrier = barrier.clone();
            let cancel = std::thread::spawn(move || {
                cancel_barrier.wait();
                cancel_handle.cancel(reason);
            });
            let attempt_handle = handle.clone();
            let attempt_barrier = barrier.clone();
            let begin = std::thread::spawn(move || {
                attempt_barrier.wait();
                let _ = attempt_handle.begin_candidate_attempt();
            });

            barrier.wait();
            cancel.join().unwrap();
            begin.join().unwrap();

            assert!(handle.is_cancelled());
            assert_eq!(handle.snapshot().stop_reason, Some(reason));
        }
    }
}
