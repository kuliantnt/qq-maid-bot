use super::*;

#[tokio::test]
async fn soft_business_failure_marks_unsucceeded() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "soft",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: true,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("soft", "c1", r#"{"value":"a"}"#)]),
            final_reply("noted"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "noted");
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert_eq!(
        outcome.agent.tool_results[0].output["error_code"],
        "soft_failure"
    );
}

#[tokio::test]
async fn clarification_tool_result_sets_clarify_stop_reason() {
    let registry = registry_with(vec![Arc::new(ClarificationTool) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("clarify", "c1", "{}")]),
            final_reply("请补充具体目标。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Clarify));
    assert_eq!(outcome.agent.executed_tools, vec!["clarify"]);
    assert!(!outcome.agent.tool_results[0].succeeded);
}

#[tokio::test]
async fn unknown_tool_is_emitted_and_attempted_but_rejected() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("unknown_tool", "c1", r#"{"value":"a"}"#)]),
            final_reply("无法执行该工具。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(outcome.agent.emitted_tools, vec!["unknown_tool"]);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert!(outcome.agent.tool_execution_attempted);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Rejected));
    assert!(outcome.agent.executed_tools.is_empty());
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert_eq!(outcome.agent.tool_results[0].name, "unknown_tool");
    assert!(!outcome.agent.tool_results[0].succeeded);
}

#[tokio::test]
async fn invalid_tool_arguments_are_emitted_and_attempted_but_not_executed() {
    let calls = Arc::new(StdMutex::new(0));
    let events = Arc::new(StdMutex::new(Vec::new()));
    let progress_sink = {
        let events = events.clone();
        Arc::new(move |event: ToolLoopProgressEvent| {
            let events = events.clone();
            Box::pin(async move {
                events.lock().unwrap().push(event);
                Ok(())
            }) as ToolLoopProgressFuture
        })
    };
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", "not-json")]),
            final_reply("参数无效，未执行。"),
        ],
    ));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.agent.emitted_tools, vec!["echo"]);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert!(outcome.agent.tool_execution_attempted);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Rejected));
    assert!(outcome.agent.executed_tools.is_empty());
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert_eq!(outcome.agent.tool_results[0].name, "echo");
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(
        *events.lock().unwrap(),
        vec![ToolLoopProgressEvent::ToolCallFailed {
            tool_name: "echo".to_owned()
        }]
    );
}

#[tokio::test]
async fn repeated_identical_invalid_arguments_are_suppressed_for_opt_in_tool() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let progress_sink = {
        let events = events.clone();
        Arc::new(move |event: ToolLoopProgressEvent| {
            let events = events.clone();
            Box::pin(async move {
                events.lock().unwrap().push(event);
                Ok(())
            }) as ToolLoopProgressFuture
        })
    };
    let registry = registry_with(vec![Arc::new(TerminalFailureCachingTool)]);
    let session = Box::new(ScriptedSession::new(
        "deepseek",
        "deepseek-v4-flash",
        vec![
            tool_calls(vec![tool_call("cacheable_read", "c1", "not-json")]),
            tool_calls(vec![tool_call("cacheable_read", "c2", "not-json")]),
            final_reply("参数无效，停止重试。"),
        ],
    ));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.agent.tool_results.len(), 2);
    assert_eq!(
        outcome.agent.tool_results[0].output["error"]["kind"],
        "invalid_arguments"
    );
    assert_eq!(
        outcome.agent.tool_results[1].output["retry_suppressed"],
        true
    );
    assert_eq!(
        *events.lock().unwrap(),
        vec![ToolLoopProgressEvent::ToolCallFailed {
            tool_name: "cacheable_read".to_owned()
        }]
    );
}

#[tokio::test]
async fn read_only_failure_is_retried_after_write_changes_state() {
    let state = Arc::new(StdMutex::new(false));
    let read_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(MutableStateReadTool {
            state: state.clone(),
            calls: read_calls.clone(),
        }),
        Arc::new(MutableStateWriteTool {
            state: state.clone(),
        }),
    ]);
    let session = Box::new(ScriptedSession::new(
        "deepseek",
        "deepseek-v4-flash",
        vec![
            tool_calls(vec![tool_call("state_read", "c1", "{}")]),
            tool_calls(vec![tool_call("state_write", "c2", "{}")]),
            tool_calls(vec![tool_call("state_read", "c3", "{}")]),
            final_reply("状态已就绪。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 4, None, None)
        .await
        .unwrap();

    assert_eq!(*read_calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.tool_results.len(), 3);
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert!(outcome.agent.tool_results[1].succeeded);
    assert!(outcome.agent.tool_results[2].succeeded);
    assert_eq!(outcome.agent.tool_results[2].output["ready"], true);
    assert_eq!(
        outcome.agent.tool_results[2].output["retry_suppressed"],
        Value::Null
    );
}

#[tokio::test]
async fn dependency_skip_after_failure() {
    let fail_calls = Arc::new(StdMutex::new(0));
    let ok_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(CountingTool {
            name: "fail_tool",
            calls: fail_calls.clone(),
            fail: true,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _,
        Arc::new(CountingTool {
            name: "ok_tool",
            calls: ok_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::PreviousCallSuccess,
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("fail_tool", "c1", r#"{"value":"a"}"#),
                tool_call("ok_tool", "c2", r#"{"value":"b"}"#),
            ]),
            final_reply("done"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*fail_calls.lock().unwrap(), 1);
    assert_eq!(*ok_calls.lock().unwrap(), 0);
    // ok_tool 因依赖跳过，仍计入轨迹且 succeeded=false。
    let ok_result = outcome
        .agent
        .tool_results
        .iter()
        .find(|r| r.name == "ok_tool")
        .unwrap();
    assert!(!ok_result.succeeded);
    assert_eq!(ok_result.output["skipped"], true);
}
