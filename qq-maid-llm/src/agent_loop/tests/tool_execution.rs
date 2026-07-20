use super::*;

#[tokio::test]
async fn request_limited_read_only_tool_forces_final_answer_after_two_calls() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(LimitedReadOnlyTool {
        calls: calls.clone(),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call(
                "knowledge_search",
                "c1",
                r#"{"query":"a"}"#,
            )]),
            tool_calls(vec![tool_call(
                "knowledge_search",
                "c2",
                r#"{"query":"b"}"#,
            )]),
            tool_calls(vec![tool_call(
                "knowledge_search",
                "c3",
                r#"{"query":"c"}"#,
            )]),
            final_reply("based on the available evidence"),
        ],
    ));
    let observed = session.observed.clone();
    let outcome = run_agent_loop(session, registry, test_context(), 5, None, None)
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.reply, "based on the available evidence");
    let observed = observed.lock().unwrap();
    assert!(!observed[3].1, "final answer round must disable tool calls");
    assert!(outcome.agent.tool_results[2].output["error_code"] == "tool_call_limit");
}

#[tokio::test]
async fn request_limit_only_rejects_knowledge_call_in_same_batch() {
    let knowledge_calls = Arc::new(StdMutex::new(0));
    let other_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(LimitedReadOnlyTool {
            calls: knowledge_calls.clone(),
        }) as _,
        Arc::new(CountingTool {
            name: "web_search",
            calls: other_calls.clone(),
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
                tool_call("knowledge_search", "k1", r#"{"query":"a"}"#),
                tool_call("knowledge_search", "k2", r#"{"query":"b"}"#),
            ]),
            tool_calls(vec![
                tool_call("knowledge_search", "k3", r#"{"query":"c"}"#),
                tool_call("web_search", "w1", r#"{"value":"d"}"#),
            ]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 5, None, None)
        .await
        .unwrap();

    assert_eq!(*knowledge_calls.lock().unwrap(), 2);
    assert_eq!(*other_calls.lock().unwrap(), 1);
    assert_eq!(outcome.reply, "done");
    assert_eq!(
        outcome.agent.executed_tools,
        ["knowledge_search", "knowledge_search", "web_search"]
    );
    assert_eq!(outcome.agent.tool_results.len(), 4);
    assert_eq!(
        outcome.agent.tool_results[2].output["error_code"],
        "tool_call_limit"
    );
    assert!(outcome.agent.tool_results[3].succeeded);
}

#[tokio::test]
async fn request_limit_does_not_skip_same_batch_side_effect_tool() {
    let knowledge_calls = Arc::new(StdMutex::new(0));
    let todo_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(LimitedReadOnlyTool {
            calls: knowledge_calls.clone(),
        }) as _,
        Arc::new(CountingTool {
            name: "add_todo",
            calls: todo_calls.clone(),
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
                tool_call("knowledge_search", "k1", r#"{"query":"a"}"#),
                tool_call("knowledge_search", "k2", r#"{"query":"b"}"#),
            ]),
            tool_calls(vec![
                tool_call("knowledge_search", "k3", r#"{"query":"c"}"#),
                tool_call("add_todo", "t1", r#"{"value":"write"}"#),
            ]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 5, None, None)
        .await
        .unwrap();

    assert_eq!(*knowledge_calls.lock().unwrap(), 2);
    assert_eq!(*todo_calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.side_effecting_tools_started, ["add_todo"]);
    assert!(outcome.agent.tool_results[3].succeeded);
    assert_eq!(outcome.reply, "done");
}

#[tokio::test]
async fn read_only_cache_hit_does_not_consume_execution_limit() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(LimitedReadOnlyTool {
        calls: calls.clone(),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call(
                "knowledge_search",
                "k1",
                r#"{"query":"a"}"#,
            )]),
            tool_calls(vec![
                tool_call("knowledge_search", "k2", r#"{"query":"a"}"#),
                tool_call("knowledge_search", "k3", r#"{"query":"b"}"#),
            ]),
            tool_calls(vec![tool_call(
                "knowledge_search",
                "k4",
                r#"{"query":"c"}"#,
            )]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 5, None, None)
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.tool_results[1].output["deduplicated"], true);
    assert_eq!(
        outcome.agent.tool_results[3].output["error_code"],
        "tool_call_limit"
    );
    assert_eq!(outcome.reply, "done");
}

#[tokio::test]
async fn duplicate_read_only_tool_call_replays_success_and_keeps_dependency_chain() {
    let calls = Arc::new(StdMutex::new(0));
    let dependent_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(SlowReadOnlyTool {
            calls: calls.clone(),
            delay: std::time::Duration::ZERO,
        }) as _,
        Arc::new(CountingTool {
            name: "dependent",
            calls: dependent_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::PreviousCallSuccess,
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            tool_calls(vec![
                tool_call("search", "c2", r#"{"value":"rust"}"#),
                tool_call("dependent", "c3", r#"{"value":"continue"}"#),
            ]),
            final_reply("done"),
        ],
    ));
    let observed = session.observed.clone();
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(*dependent_calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, ["search", "dependent"]);
    assert_eq!(outcome.agent.side_effecting_tools_started, ["dependent"]);
    assert!(outcome.agent.tools_with_unknown_result.is_empty());
    assert_eq!(outcome.agent.tool_results.len(), 3);
    assert!(
        outcome
            .agent
            .tool_results
            .iter()
            .all(|result| result.succeeded)
    );
    let observed = observed.lock().unwrap();
    assert_ne!(observed[1].0[0].output, observed[2].0[0].output);
    assert!(observed[2].0[0].output.contains("deduplicated"));
    assert!(observed[2].0[1].output.contains("continue"));
}

#[tokio::test]
async fn failed_tool_followed_by_same_singleton_call_is_recorded_as_retry() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(FailOnceReadOnlyTool {
        calls: calls.clone(),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            tool_calls(vec![tool_call("search", "c2", r#"{"value":"rust"}"#)]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.tool_results.len(), 2);
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert!(outcome.agent.tool_results[1].succeeded);
    assert_eq!(outcome.agent.tool_attempts.len(), 2);
    assert_eq!(outcome.agent.tool_attempts[0].retry_of, None);
    assert_eq!(outcome.agent.tool_attempts[1].retry_of, Some(0));
}

#[tokio::test]
async fn cross_candidate_retry_indexes_are_offset_to_global() {
    let handle = AgentRunHandle::default();

    // 候选 A：先产生一个成功工具结果，再因模型错误退出。
    handle.begin_candidate_attempt().unwrap();
    let calls_a = Arc::new(StdMutex::new(0));
    let registry_a = registry_with(vec![Arc::new(SlowReadOnlyTool {
        calls: calls_a.clone(),
        delay: std::time::Duration::ZERO,
    }) as _]);
    let err = run_agent_loop_with_handle(
        Box::new(ErrorScriptSession {
            script: VecDeque::from([
                Ok(tool_calls(vec![tool_call(
                    "search",
                    "a1",
                    r#"{"value":"a"}"#,
                )])),
                Err(LlmError::provider("candidate a failed", "provider")),
            ]),
        }),
        registry_a,
        test_context(),
        3,
        None,
        None,
        Some(handle.clone()),
    )
    .await
    .unwrap_err();
    let after_a = err.agent.expect("candidate a diagnostics");
    assert_eq!(after_a.tool_results.len(), 1);
    assert!(after_a.tool_results[0].succeeded);
    assert_eq!(after_a.tool_attempts.len(), 1);
    assert_eq!(after_a.tool_attempts[0].result_index, 0);
    assert_eq!(after_a.tool_attempts[0].retry_of, None);

    // 候选 B：同一 AgentRunHandle 上失败后重试成功。
    handle.begin_candidate_attempt().unwrap();
    let calls_b = Arc::new(StdMutex::new(0));
    let registry_b = registry_with(vec![Arc::new(FailOnceReadOnlyTool {
        calls: calls_b.clone(),
    }) as _]);
    let outcome = run_agent_loop_with_handle(
        Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![
                tool_calls(vec![tool_call("search", "b1", r#"{"value":"rust"}"#)]),
                tool_calls(vec![tool_call("search", "b2", r#"{"value":"rust"}"#)]),
                final_reply("done"),
            ],
        )),
        registry_b,
        test_context(),
        3,
        None,
        None,
        Some(handle.clone()),
    )
    .await
    .unwrap();

    assert_eq!(*calls_a.lock().unwrap(), 1);
    assert_eq!(*calls_b.lock().unwrap(), 2);
    assert_eq!(outcome.agent.tool_results.len(), 3);
    assert_eq!(outcome.agent.tool_attempts.len(), 3);

    // 候选 A 的全局下标保持不变。
    assert_eq!(outcome.agent.tool_attempts[0].result_index, 0);
    assert_eq!(outcome.agent.tool_attempts[0].retry_of, None);
    assert!(outcome.agent.tool_results[0].succeeded);

    // 候选 B 的局部 0/1 应偏移为全局 1/2，retry_of 指向 B 的失败结果而非 A。
    assert_eq!(outcome.agent.tool_attempts[1].result_index, 1);
    assert_eq!(outcome.agent.tool_attempts[1].retry_of, None);
    assert!(!outcome.agent.tool_results[1].succeeded);
    assert_eq!(outcome.agent.tool_attempts[2].result_index, 2);
    assert_eq!(outcome.agent.tool_attempts[2].retry_of, Some(1));
    assert!(outcome.agent.tool_results[2].succeeded);
}

#[tokio::test]
async fn independent_same_round_calls_are_not_recorded_as_retry() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(FailOnceReadOnlyTool {
        calls: calls.clone(),
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("search", "c1", r#"{"value":"rust"}"#),
                tool_call("search", "c2", r#"{"value":"rust"}"#),
            ]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.tool_attempts.len(), 2);
    assert!(
        outcome
            .agent
            .tool_attempts
            .iter()
            .all(|attempt| attempt.retry_of.is_none())
    );
}

#[tokio::test]
async fn side_effecting_tool_invalidates_read_only_deduplication() {
    let search_calls = Arc::new(StdMutex::new(0));
    let write_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(SlowReadOnlyTool {
            calls: search_calls.clone(),
            delay: std::time::Duration::ZERO,
        }) as _,
        Arc::new(CountingTool {
            name: "echo",
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
            tool_calls(vec![tool_call("search", "c1", r#"{"value":"rust"}"#)]),
            tool_calls(vec![tool_call("echo", "c2", r#"{"value":"write"}"#)]),
            tool_calls(vec![tool_call("search", "c3", r#"{"value":"rust"}"#)]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 4, None, None)
        .await
        .unwrap();

    assert_eq!(*search_calls.lock().unwrap(), 2);
    assert_eq!(*write_calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.side_effecting_tools_started, ["echo"]);
}

#[tokio::test]
async fn single_tool_then_final_answer() {
    let calls = Arc::new(StdMutex::new(0));
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
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::ToolUsed));
    assert_eq!(outcome.agent.executed_tools, vec!["echo".to_owned()]);
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert!(outcome.agent.tool_results[0].succeeded);
}

#[tokio::test]
async fn progress_sink_reports_tool_start_and_finish() {
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
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
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

    assert_eq!(outcome.reply, "done");
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            ToolLoopProgressEvent::ToolCallStarted {
                tool_name: "echo".to_owned()
            },
            ToolLoopProgressEvent::ToolCallFinished {
                tool_name: "echo".to_owned()
            }
        ]
    );
}

#[tokio::test]
async fn progress_sink_reports_tool_failure() {
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
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: true,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
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

    assert_eq!(outcome.reply, "done");
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            ToolLoopProgressEvent::ToolCallStarted {
                tool_name: "echo".to_owned()
            },
            ToolLoopProgressEvent::ToolCallFailed {
                tool_name: "echo".to_owned()
            }
        ]
    );
}

#[tokio::test]
async fn progress_sink_error_interrupts_before_tool_execution() {
    let calls = Arc::new(StdMutex::new(0));
    let progress_sink = Arc::new(move |event: ToolLoopProgressEvent| {
        Box::pin(async move {
            assert_eq!(
                event,
                ToolLoopProgressEvent::ToolCallStarted {
                    tool_name: "echo".to_owned()
                }
            );
            Err(LlmError::new(
                "cancelled",
                "stream receiver dropped",
                "stream",
            ))
        }) as ToolLoopProgressFuture
    });
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
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));

    let err = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "cancelled");
    assert_eq!(err.stage, "stream");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Cancelled));
    assert_eq!(diagnostics.emitted_tools, vec!["echo"]);
    assert!(diagnostics.tool_execution_attempted);
    assert!(diagnostics.executed_tools.is_empty());
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn progress_sink_error_after_tool_completion_keeps_real_result() {
    let calls = Arc::new(StdMutex::new(0));
    let progress_sink = Arc::new(move |event: ToolLoopProgressEvent| {
        Box::pin(async move {
            match event {
                ToolLoopProgressEvent::ToolCallStarted { .. } => Ok(()),
                ToolLoopProgressEvent::ToolCallFinished { .. } => Err(LlmError::new(
                    "cancelled",
                    "stream receiver dropped after tool completion",
                    "stream",
                )),
                ToolLoopProgressEvent::ToolCallFailed { .. } => {
                    panic!("successful tool must not emit failed progress")
                }
            }
        }) as ToolLoopProgressFuture
    });
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let handle = AgentRunHandle::default();

    let err = run_agent_loop_with_handle(
        Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![tool_calls(vec![tool_call(
                "echo",
                "c1",
                r#"{"value":"a"}"#,
            )])],
        )),
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
        Some(handle),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "cancelled");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.executed_tools, ["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn same_round_multiple_tools_prepare_before_execute() {
    let sequence = Arc::new(StdMutex::new(Vec::new()));
    let registry = registry_with(vec![
        Arc::new(OrderTool {
            name: "first",
            sequence: sequence.clone(),
        }) as _,
        Arc::new(OrderTool {
            name: "second",
            sequence: sequence.clone(),
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("first", "c1", r#"{"value":"a"}"#),
                tool_call("second", "c2", r#"{"value":"b"}"#),
            ]),
            final_reply("ok"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "ok");
    assert_eq!(
        *sequence.lock().unwrap(),
        vec![
            "prepare:first".to_owned(),
            "prepare:second".to_owned(),
            "execute:first".to_owned(),
            "execute:second".to_owned(),
        ]
    );
}

#[tokio::test]
async fn multi_round_continues_after_tool_result() {
    let calls = Arc::new(StdMutex::new(0));
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
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            tool_calls(vec![tool_call("echo", "c2", r#"{"value":"b"}"#)]),
            final_reply("merged"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "merged");
    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.model_rounds, 3);
    assert_eq!(
        outcome.agent.executed_tools,
        vec!["echo".to_owned(), "echo".to_owned()]
    );
}

#[tokio::test]
async fn execution_exception_still_records_result_and_continues() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "boom",
        calls: Arc::new(StdMutex::new(0)),
        fail: true,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("boom", "c1", r#"{"value":"a"}"#)]),
            final_reply("recovered"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "recovered");
    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Failed));
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert!(outcome.agent.tool_results[0].output["error"]["code"] == "tool_failed");
}

#[tokio::test]
async fn model_failure_after_tool_execution_keeps_partial_trace() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ErrorScriptSession {
        script: VecDeque::from([
            Ok(tool_calls(vec![tool_call(
                "echo",
                "c1",
                r#"{"value":"a"}"#,
            )])),
            Err(LlmError::provider("second round failed", "provider")),
        ]),
    });

    let err = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap_err();

    assert_eq!(*calls.lock().unwrap(), 1);
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 2);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Failed));
    assert_eq!(diagnostics.emitted_tools, vec!["echo"]);
    assert_eq!(diagnostics.executed_tools, vec!["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tool_results[0].succeeded);
}

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

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
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
