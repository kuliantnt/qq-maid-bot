use super::*;

#[tokio::test]
async fn tool_loop_executes_function_call_and_returns_output_to_model() {
    let (base_url, state) = spawn_tool_loop_mock().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("杭州今天需要带伞吗？")],
                &registry,
                None,
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "杭州今天有小雨，建议带伞。");
    let state = state.lock().await;
    assert_eq!(state.requests.len(), 2);
    assert_eq!(state.requests[0]["tools"][0]["name"], "get_weather");
    assert_eq!(state.requests[0]["parallel_tool_calls"], false);
    let second_input = state.requests[1]["input"].as_array().unwrap();
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_weather_1"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"weather\":\"小雨\""))
    }));
}

#[tokio::test]
async fn tool_loop_budget_before_first_request_disables_tools_for_answer() {
    let (base_url, state) = spawn_tool_loop_mock().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("杭州今天需要带伞吗？")],
                &registry,
                Some(crate::context_budget::ContextBudgetConfig {
                    context_window_chars: 150,
                    output_reserve_chars: 20,
                    protected_recent_turns: 0,
                }),
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "杭州今天有小雨，建议带伞。");
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["tool_choice"], "none");
    assert!(requests[0]["tools"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn tool_loop_budget_after_tool_result_disables_tools_for_final_answer() {
    let (base_url, state) = spawn_tool_loop_mock().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("杭州今天需要带伞吗？")],
                &registry,
                Some(crate::context_budget::ContextBudgetConfig {
                    context_window_chars: 420,
                    output_reserve_chars: 20,
                    protected_recent_turns: 0,
                }),
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "杭州今天有小雨，建议带伞。");
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["tools"][0]["name"], "get_weather");
    assert_eq!(requests[1]["tool_choice"], "none");
    assert!(requests[1]["tools"].as_array().is_some_and(Vec::is_empty));
}

#[tokio::test]
async fn tool_loop_budget_estimate_error_skips_provider_request() {
    let (base_url, state) = spawn_tool_loop_mock().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let client = qq_maid_common::http_client::client();

    let err = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("__force_json_estimate_error__")],
                &registry,
                Some(crate::context_budget::ContextBudgetConfig {
                    context_window_chars: 10_000,
                    output_reserve_chars: 20,
                    protected_recent_turns: 0,
                }),
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "context_budget_estimate_error");
    assert_eq!(err.stage, "tool_loop");
    assert!(state.lock().await.requests.is_empty());
}

#[tokio::test]
async fn tool_loop_serializes_multiple_calls_and_skips_dependent_call_after_failure() {
    let (base_url, state) = spawn_multi_tool_mock().await;
    let fail_calls = Arc::new(AtomicUsize::new(0));
    let ok_calls = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new()
        .register(SequenceToolStub {
            fail: true,
            calls: fail_calls.clone(),
        })
        .unwrap()
        .register(SequenceToolStub {
            fail: false,
            calls: ok_calls.clone(),
        })
        .unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("连续执行两个工具")],
                &registry,
                None,
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "已经汇总结果。");
    assert_eq!(fail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ok_calls.load(Ordering::SeqCst), 0);
    let state = state.lock().await;
    assert_eq!(state.requests.len(), 2);
    let second_input = state.requests[1]["input"].as_array().unwrap();
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_fail_1"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"tool_failed\""))
    }));
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_ok_1"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"skipped\":true"))
    }));
}

#[tokio::test]
async fn tool_loop_prepares_same_round_calls_before_executing_any_tool() {
    let (base_url, _state) = spawn_prepare_order_mock().await;
    let sequence = Arc::new(StdMutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(PrepareOrderToolStub {
            name: "first_order_tool",
            sequence: sequence.clone(),
        }))
        .unwrap();
    registry
        .insert(Arc::new(PrepareOrderToolStub {
            name: "second_order_tool",
            sequence: sequence.clone(),
        }))
        .unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("同轮调用两个工具")],
                &registry,
                None,
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "顺序已记录。");
    assert_eq!(
        *sequence.lock().unwrap(),
        vec![
            "prepare:first_order_tool",
            "prepare:second_order_tool",
            "execute:first_order_tool",
            "execute:second_order_tool",
        ]
    );
}

#[tokio::test]
async fn tool_loop_keeps_independent_calls_after_prepare_failure() {
    let (base_url, state) = spawn_prepare_failure_mock().await;
    let registry = ToolRegistry::new()
        .register(PrepareFailToolStub)
        .unwrap()
        .register(WeatherToolStub)
        .unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("先失败再查天气")],
                &registry,
                None,
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "准备失败已汇总。");
    let state = state.lock().await;
    assert_eq!(state.requests.len(), 2);
    let second_input = state.requests[1]["input"].as_array().unwrap();
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_prepare_fail_1"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"prepare failed\""))
    }));
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_weather_2"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"weather\":\"小雨\""))
    }));
}

#[tokio::test]
async fn tool_loop_skips_dependent_call_after_structured_tool_failure() {
    let (base_url, state) = spawn_soft_failure_mock().await;
    let soft_fail_calls = Arc::new(AtomicUsize::new(0));
    let ok_calls = Arc::new(AtomicUsize::new(0));
    let registry = ToolRegistry::new()
        .register(SoftFailToolStub {
            calls: soft_fail_calls.clone(),
        })
        .unwrap()
        .register(SequenceToolStub {
            fail: false,
            calls: ok_calls.clone(),
        })
        .unwrap();
    let client = qq_maid_common::http_client::client();

    let outcome = run_agent_loop(
        Box::new(
            ResponsesAgentSession::new(
                client,
                "test-key".to_owned(),
                Some(base_url),
                "openai",
                "gpt-test".to_owned(),
                10 * 1024 * 1024,
                1200,
                None,
                &[ChatMessage::user("先返回业务失败，再尝试依赖调用")],
                &registry,
                None,
            )
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "业务失败已汇总。");
    assert_eq!(soft_fail_calls.load(Ordering::SeqCst), 1);
    assert_eq!(ok_calls.load(Ordering::SeqCst), 0);
    let state = state.lock().await;
    assert_eq!(state.requests.len(), 2);
    let second_input = state.requests[1]["input"].as_array().unwrap();
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_soft_fail_1"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"soft_failure\""))
    }));
    assert!(second_input.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_ok_2"
            && item["output"]
                .as_str()
                .is_some_and(|output| output.contains("\"skipped\":true"))
    }));
}
