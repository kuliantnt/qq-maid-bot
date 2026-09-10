//! 冗余续调的通用边界，不依赖具体业务工具名或用户文案。
use super::*;

struct ValidatedQuery {
    effect: ToolEffect,
}
#[async_trait]
impl crate::tool::Tool for ValidatedQuery {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "query".into(),
            description: "查询测试".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn effect(&self) -> ToolEffect {
        self.effect
    }
    async fn execute(&self, _: ToolContext, args: Value) -> Result<ToolOutput, LlmError> {
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| LlmError::new("bad_tool_arguments", "target required", "tool"))?;
        if target == "timeout" {
            return Err(LlmError::timeout("tool"));
        }
        Ok(ToolOutput::json(json!({"ok":true,"target":target})))
    }
}

#[tokio::test]
async fn degraded_continuation_requires_success_readonly_and_no_new_arguments() {
    for (effect, first, second, expected) in [
        (ToolEffect::ReadOnly, r#"{"target":"a"}"#, "{}", Some(0)),
        (
            ToolEffect::ReadOnly,
            r#"{"target":"a"}"#,
            r#"{"target":" "}"#,
            Some(0),
        ),
        (
            ToolEffect::ReadOnly,
            r#"{"target":"a"}"#,
            r#"{"target":null}"#,
            Some(0),
        ),
        (
            ToolEffect::ReadOnly,
            r#"{"target":"a"}"#,
            r#"{"new_option":"b"}"#,
            None,
        ),
        (
            ToolEffect::ReadOnly,
            r#"{"target":"a"}"#,
            r#"{"target":"timeout"}"#,
            None,
        ),
        (ToolEffect::ReadOnly, "{}", "{}", None),
        (ToolEffect::SideEffecting, r#"{"target":"a"}"#, "{}", None),
        (ToolEffect::ReadOnly, r#"{"target":"a"}"#, "not json", None),
    ] {
        let session = Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![
                tool_calls(vec![tool_call("query", "c1", first)]),
                tool_calls(vec![tool_call("query", "c2", second)]),
                final_reply("done"),
            ],
        ));
        let outcome = run_agent_loop(
            session,
            registry_with(vec![Arc::new(ValidatedQuery { effect })]),
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(outcome.agent.model_rounds, 3);
        assert_eq!(
            outcome.agent.tool_attempts[1].redundant_of, expected,
            "{effect:?} {first} -> {second}"
        );
        assert!(!outcome.agent.tool_results[1].succeeded);
    }
}

#[tokio::test]
async fn multi_call_batches_do_not_infer_redundant_missing_targets() {
    for batch in [
        vec![
            tool_call("query", "c1", r#"{"target":"a"}"#),
            tool_call("query", "c2", "{}"),
        ],
        vec![
            tool_call("query", "c1", "{}"),
            tool_call("query", "c2", r#"{"target":"b"}"#),
        ],
    ] {
        let session = Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![
                tool_calls(vec![tool_call("query", "initial", r#"{"target":"a"}"#)]),
                tool_calls(batch),
                final_reply("done"),
            ],
        ));
        let outcome = run_agent_loop(
            session,
            registry_with(vec![Arc::new(ValidatedQuery {
                effect: ToolEffect::ReadOnly,
            })]),
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap();
        for attempt in &outcome.agent.tool_attempts {
            if !outcome.agent.tool_results[attempt.result_index].succeeded {
                assert_eq!(attempt.redundant_of, None);
            }
        }
    }
}

#[tokio::test]
async fn degraded_call_does_not_prevent_a_corrected_necessary_step() {
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("query", "c1", r#"{"target":"a"}"#)]),
            tool_calls(vec![tool_call("query", "c2", "{}")]),
            tool_calls(vec![tool_call("query", "c3", r#"{"target":"timeout"}"#)]),
            final_reply("partial"),
        ],
    ));
    let observed = session.observed.clone();
    let outcome = run_agent_loop(
        session,
        registry_with(vec![Arc::new(ValidatedQuery {
            effect: ToolEffect::ReadOnly,
        })]),
        test_context(),
        4,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.agent.model_rounds, 4);
    assert_eq!(outcome.agent.tool_attempts[1].redundant_of, Some(0));
    assert_eq!(outcome.agent.tool_attempts[2].redundant_of, None);
    assert_eq!(
        outcome.agent.tool_results[2].output["error"]["code"],
        "timeout"
    );
    assert!(observed.lock().unwrap()[2].1, "仍允许模型补全新步骤");
}

#[tokio::test]
async fn consecutive_degraded_calls_keep_the_original_success_anchor() {
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("query", "c1", r#"{"target":"a"}"#)]),
            tool_calls(vec![tool_call("query", "c2", "{}")]),
            tool_calls(vec![tool_call("query", "c3", r#"{"target":null}"#)]),
            final_reply("done"),
        ],
    ));
    let outcome = run_agent_loop(
        session,
        registry_with(vec![Arc::new(ValidatedQuery {
            effect: ToolEffect::ReadOnly,
        })]),
        test_context(),
        4,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.agent.tool_attempts[1].redundant_of, Some(0));
    assert_eq!(outcome.agent.tool_attempts[2].redundant_of, Some(0));
    assert!(!outcome.agent.tool_results[2].succeeded);
}

#[tokio::test]
async fn continuation_indexes_are_local_until_candidate_diagnostics_are_merged() {
    let handle = AgentRunHandle::default();
    let registry = registry_with(vec![Arc::new(ValidatedQuery {
        effect: ToolEffect::ReadOnly,
    })]);
    handle.begin_candidate_attempt().unwrap();
    run_agent_loop_with_handle(
        Box::new(ErrorScriptSession {
            script: VecDeque::from([
                Ok(tool_calls(vec![tool_call(
                    "query",
                    "a1",
                    r#"{"target":"a"}"#,
                )])),
                Err(LlmError::provider("candidate failed", "provider")),
            ]),
        }),
        registry.clone(),
        test_context(),
        3,
        None,
        None,
        Some(handle.clone()),
    )
    .await
    .unwrap_err();
    handle.begin_candidate_attempt().unwrap();
    let outcome = run_agent_loop_with_handle(
        Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![
                // 候选 A 的成功不能覆盖候选 B 的首次缺参。
                tool_calls(vec![tool_call("query", "b1", "{}")]),
                tool_calls(vec![tool_call("query", "b2", r#"{"target":"b"}"#)]),
                tool_calls(vec![tool_call("query", "b3", "{}")]),
                final_reply("done"),
            ],
        )),
        registry,
        test_context(),
        4,
        None,
        None,
        Some(handle),
    )
    .await
    .unwrap();
    assert_eq!(outcome.agent.tool_attempts[1].redundant_of, None);
    assert_eq!(outcome.agent.tool_attempts[3].redundant_of, Some(2));
    assert_eq!(outcome.agent.final_candidate_tool_result_start, Some(1));
}

struct PrepareValidatedQuery;

#[async_trait]
impl crate::tool::Tool for PrepareValidatedQuery {
    fn metadata(&self) -> ToolMetadata {
        ValidatedQuery {
            effect: ToolEffect::ReadOnly,
        }
        .metadata()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn prepare(
        &self,
        _: &ToolContext,
        arguments: Value,
    ) -> Result<crate::tool::ToolPreparation, LlmError> {
        if arguments.get("target").is_none() {
            return Err(LlmError::new(
                "bad_tool_arguments",
                "target required in prepare",
                "tool",
            ));
        }
        Ok(crate::tool::ToolPreparation::ready(arguments))
    }

    async fn execute(&self, context: ToolContext, args: Value) -> Result<ToolOutput, LlmError> {
        ValidatedQuery {
            effect: ToolEffect::ReadOnly,
        }
        .execute(context, args)
        .await
    }
}

#[tokio::test]
async fn prepare_argument_rejection_conservatively_keeps_independent_failure() {
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("query", "c1", r#"{"target":"a"}"#)]),
            tool_calls(vec![tool_call("query", "c2", "{}")]),
            final_reply("done"),
        ],
    ));
    let observed = session.observed.clone();
    let outcome = run_agent_loop(
        session,
        registry_with(vec![Arc::new(PrepareValidatedQuery)]),
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.agent.model_rounds, 3);
    assert_eq!(outcome.agent.tool_results.len(), 2);
    assert!(outcome.agent.tool_results[0].succeeded);
    let failure = &outcome.agent.tool_results[1];
    assert!(!failure.succeeded);
    assert_eq!(failure.output["error"]["code"], "bad_tool_arguments");
    assert!(
        failure
            .output
            .to_string()
            .contains("target required in prepare")
    );
    // prepare 未成功时不推断退化关系，即便工具声明只读且上轮已有成功。
    assert_eq!(outcome.agent.tool_attempts[1].redundant_of, None);
    let inputs = observed.lock().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&inputs[2].0[0].output).unwrap()["ok"],
        false
    );
    assert!(!inputs[2].0[0].output.contains("continuation_hint"));
}
