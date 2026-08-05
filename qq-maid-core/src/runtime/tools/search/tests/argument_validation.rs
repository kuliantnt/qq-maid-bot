use super::*;

#[tokio::test]
async fn agent_web_search_invalid_arguments_are_structured_and_skip_executor() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let tool =
        WebSearchTool::new(Arc::new(executor)).with_backend_override(WebSearchBackend::Tavily);
    let registry = ToolRegistry::new().register(tool).unwrap();
    let cases = vec![
        (
            "topic",
            json!({"query": "safe query", "topic": "medical"}),
            "unsupported_value",
            "string",
        ),
        (
            "time_range",
            json!({"query": "safe query", "time_range": "all"}),
            "unsupported_value",
            "string",
        ),
        (
            "context_size",
            json!({"query": "safe query", "context_size": "full"}),
            "unsupported_value",
            "string",
        ),
        (
            "max_results",
            json!({"query": "safe query", "max_results": "5"}),
            "invalid_type",
            "string",
        ),
        (
            "max_results",
            json!({"query": "safe query", "max_results": 20}),
            "out_of_range",
            "integer",
        ),
        ("query", json!({"query": null}), "missing_or_empty", "null"),
        (
            "query",
            json!({"query": "  "}),
            "missing_or_empty",
            "string",
        ),
        (
            "query",
            json!({"query": "q".repeat(WEB_SEARCH_QUERY_MAX_LENGTH + 1)}),
            "too_long",
            "string",
        ),
    ];

    for (index, (argument, arguments, reason, value_kind)) in cases.into_iter().enumerate() {
        let mut context = test_context();
        context.tool_call_id = Some(format!("task-1:invalid-{index}"));
        context.tool_round = Some(2);
        let serialized = registry
            .execute_json(
                &context,
                WEB_SEARCH_TOOL_NAME,
                &serde_json::to_string(&arguments).unwrap(),
            )
            .await
            .unwrap();
        let output: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(output["ok"], false, "{argument}");
        assert_eq!(output["execution_succeeded"], false, "{argument}");
        assert_eq!(output["backend"], "tavily", "{argument}");
        assert_eq!(output["error"]["code"], "invalid_arguments");
        assert_eq!(output["error"]["stage"], "tool");
        assert_eq!(output["error"]["argument"], argument);
        assert_eq!(output["error"]["reason"], reason);
        assert_eq!(output["error"]["value_kind"], value_kind);
        assert_eq!(output["error"]["retryable_by_model"], true);
    }

    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn string_null_time_range_is_treated_as_unset() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let tool =
        WebSearchTool::new(Arc::new(executor)).with_backend_override(WebSearchBackend::Tavily);
    let registry = ToolRegistry::new().register(tool).unwrap();
    let mut context = test_context();
    context.tool_call_id = Some("task-string-null:call-1".to_owned());

    let serialized = registry
        .execute_json(
            &context,
            WEB_SEARCH_TOOL_NAME,
            r#"{"query":"safe query","time_range":"null"}"#,
        )
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["execution_succeeded"], true);
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(requests.lock().unwrap()[0].time_range, None);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_argument_log_has_field_diagnostics_without_search_content() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(LogWriter(bytes.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let tool = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()))
        .with_backend_override(WebSearchBackend::Tavily);
    let mut context = test_context();
    context.task_id = "task-argument-diagnostics".to_owned();
    context.tool_call_id = Some("task-argument-diagnostics:call-1".to_owned());
    context.tool_round = Some(4);

    let output = tool
        .execute(
            context,
            json!({
                "query": "private query content",
                "raw_question": "private prompt body",
                "topic": "health",
            }),
        )
        .await
        .unwrap();
    assert_eq!(output.value["error"]["argument"], "topic");

    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "tool",
        "backend",
        "argument",
        "reason",
        "value_kind",
        "safe_value",
        "task_id",
        "tool_call_id",
        "tool_round",
    ] {
        assert!(logs.contains(field), "missing log field {field}: {logs}");
    }
    assert!(logs.contains("safe_value=Some(\"health\")"));
    assert!(logs.contains("query_chars=None"));
    assert!(!logs.contains("query_chars=0"));
    assert!(logs.contains("task-argument-diagnostics"));
    assert!(!logs.contains("private query content"));
    assert!(!logs.contains("private prompt body"));
}
