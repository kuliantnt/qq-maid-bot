use super::*;

#[tokio::test]
async fn streaming_tool_call_does_not_release_buffered_text_delta() {
    let mut input = Vec::new();
    let deltas = Arc::new(StdMutex::new(Vec::new()));
    let step = finalize_responses_tool_loop_stream(
        &mut input,
        true,
        recording_delta_sink(deltas.clone()),
        "草稿".to_owned(),
        vec!["草稿".to_owned()],
        Some(json!({
            "output": [{
                "type": "function_call",
                "name": "get_weather",
                "call_id": "call_weather_1",
                "arguments": "{\"city\":\"杭州\"}"
            }]
        })),
        true,
    )
    .await
    .unwrap();

    let AgentStep::ToolCalls { calls, .. } = step else {
        panic!("expected tool calls");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "get_weather");
    assert!(deltas.lock().unwrap().is_empty());
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "function_call");
}

#[tokio::test]
async fn agent_stream_finishes_on_completed_without_waiting_for_http_eof() {
    let base_url = spawn_never_closing_completed_stream().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let mut session = ResponsesAgentSession::new(
        reqwest::Client::new(),
        "test-key".to_owned(),
        Some(base_url),
        "openai",
        "gpt-test".to_owned(),
        10 * 1024 * 1024,
        1200,
        None,
        &[ChatMessage::user("小女仆测试一下")],
        &registry,
        None,
    )
    .unwrap();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let step = tokio::time::timeout(
        Duration::from_millis(300),
        session.advance_streaming(&[], true, recording_delta_sink(deltas.clone())),
    )
    .await
    .expect("agent step must finish from response.completed without EOF")
    .unwrap()
    .unwrap();

    let AgentStep::FinalAnswer { reply, .. } = step else {
        panic!("expected direct final answer");
    };
    assert_eq!(reply, "direct answer");
    assert_eq!(*deltas.lock().unwrap(), vec!["direct answer".to_owned()]);
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.chunk_count >= 1);
    assert!(diagnostics.sse_event_count >= 1);
    assert!(diagnostics.saw_completed);
    assert!(!diagnostics.saw_done);
}

#[tokio::test]
async fn agent_stream_finishes_on_done_without_waiting_for_http_eof() {
    let base_url = spawn_never_closing_done_stream().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let mut session = ResponsesAgentSession::new(
        reqwest::Client::new(),
        "test-key".to_owned(),
        Some(base_url),
        "openai",
        "gpt-test".to_owned(),
        10 * 1024 * 1024,
        1200,
        None,
        &[ChatMessage::user("小女仆测试一下")],
        &registry,
        None,
    )
    .unwrap();

    let step = tokio::time::timeout(
        Duration::from_millis(300),
        session.advance_streaming(
            &[],
            true,
            recording_delta_sink(Arc::new(StdMutex::new(Vec::new()))),
        ),
    )
    .await
    .expect("agent step must finish from [DONE] without EOF")
    .unwrap()
    .unwrap();

    let AgentStep::FinalAnswer { reply, .. } = step else {
        panic!("expected direct final answer");
    };
    assert_eq!(reply, "done answer");
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.chunk_count >= 1);
    assert_eq!(diagnostics.sse_event_count, 2);
    assert!(diagnostics.saw_done);
    assert!(diagnostics.saw_completed);
}

#[test]
fn done_does_not_complete_an_unfinished_function_call() {
    let mut active = HashSet::new();
    let mut completed = Vec::new();
    observe_responses_function_call_event(
        &SseFrame {
            event: Some("response.function_call_arguments.delta".to_owned()),
            data: json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "{\"city\":"
            })
            .to_string(),
        },
        &mut active,
        &mut completed,
    )
    .unwrap();

    assert_eq!(active, HashSet::from([0]));
    assert!(completed.is_empty());
}
