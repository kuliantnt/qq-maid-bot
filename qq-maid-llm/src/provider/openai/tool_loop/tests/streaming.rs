use super::*;

fn test_streaming_session(base_url: String) -> ResponsesAgentSession {
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    ResponsesAgentSession::new(
        qq_maid_common::http_client::client(),
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
    .unwrap()
}

#[tokio::test]
async fn streaming_tool_call_does_not_release_buffered_text_delta() {
    let mut input = Vec::new();
    let deltas = Arc::new(StdMutex::new(Vec::new()));
    let step = finalize_responses_tool_loop_stream(
        &mut input,
        recording_delta_sink(deltas.clone()),
        StreamFinalization {
            allow_tool_calls: true,
            answer: "草稿".to_owned(),
            buffered_deltas: vec!["草稿".to_owned()],
            completed_response: Some(json!({
                "output": [{
                    "type": "function_call",
                    "name": "get_weather",
                    "call_id": "call_weather_1",
                    "arguments": "{\"city\":\"杭州\"}"
                }]
            })),
            completion_confirmed: true,
            diagnostics: Arc::new(StdMutex::new(Default::default())),
        },
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
        qq_maid_common::http_client::client(),
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
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("response_completed")
    );
    assert!(!diagnostics.saw_done);
}

#[tokio::test]
async fn agent_stream_finishes_on_done_without_waiting_for_http_eof() {
    let base_url = spawn_never_closing_done_stream().await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let mut session = ResponsesAgentSession::new(
        qq_maid_common::http_client::client(),
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
    assert!(!diagnostics.saw_completed);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("done_sentinel")
    );
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

    assert_eq!(active, HashSet::from(["output_index:0".to_owned()]));
    assert!(completed.is_empty());
}

#[tokio::test]
async fn normal_eof_with_text_and_no_function_call_is_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"好\"}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let step = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap()
        .unwrap();

    let AgentStep::FinalAnswer { reply, .. } = step else {
        panic!("expected compatible EOF final answer");
    };
    assert_eq!(reply, "你好");
    assert_eq!(*deltas.lock().unwrap(), ["你", "好"]);
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert!(!diagnostics.saw_completed);
    assert!(!diagnostics.saw_done);
    assert!(diagnostics.saw_text_delta);
    assert_eq!(diagnostics.buffered_delta_count, 2);
    assert_eq!(diagnostics.buffered_text_chars, 2);
    assert_eq!(diagnostics.visible_text_chars, 2);
    assert_eq!(diagnostics.active_function_call_count, 0);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_compatible_completion")
    );
}

#[tokio::test]
async fn normal_eof_with_text_and_completed_function_call_is_not_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"name\":\"get_weather\",\"call_id\":\"call_weather_1\",\"arguments\":\"{\\\"city\\\":\\\"杭州\\\"}\"}}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    // 返回错误意味着既不会产出 FinalAnswer，也不会把 ToolCalls 交给执行层。
    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert!(deltas.lock().unwrap().is_empty());
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert!(!diagnostics.saw_completed);
    assert!(!diagnostics.saw_done);
    assert_eq!(diagnostics.active_function_call_count, 0);
    assert_eq!(diagnostics.visible_text_chars, 0);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_completed_function_call_without_terminal_event")
    );
    assert_eq!(
        diagnostics.fallback_reason.as_deref(),
        Some("normal_eof_completed_function_call_without_terminal_event")
    );
}

#[tokio::test]
async fn normal_eof_with_only_completed_function_call_is_not_completion() {
    let base_url = spawn_static_sse_stream(
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"name\":\"get_weather\",\"call_id\":\"call_weather_1\",\"arguments\":\"{\\\"city\\\":\\\"杭州\\\"}\"}}\n\n",
    )
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    // 缺少标准终止事件时，完整 function call 也不能进入工具执行层。
    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "stream");
    assert!(deltas.lock().unwrap().is_empty());
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert!(!diagnostics.saw_completed);
    assert!(!diagnostics.saw_done);
    assert!(!diagnostics.saw_text_delta);
    assert_eq!(diagnostics.active_function_call_count, 0);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_completed_function_call_without_terminal_event")
    );
    assert_ne!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_compatible_completion")
    );
}

#[tokio::test]
async fn normal_eof_with_unclosed_function_call_is_not_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"city\\\":\"}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert!(deltas.lock().unwrap().is_empty());
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert_eq!(diagnostics.active_function_call_count, 1);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_active_function_call")
    );
}

#[tokio::test]
async fn sse_parse_error_after_text_is_not_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.output_text.delta\ndata: {not-json}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "sse");
    assert!(deltas.lock().unwrap().is_empty());
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.parse_error);
    assert!(!diagnostics.normal_eof);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("sse_parse_error")
    );
}

#[tokio::test]
async fn incomplete_final_sse_frame_after_text_is_not_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.output_text.delta",
    ))
    .await;
    let mut session = test_streaming_session(base_url);

    let err = session
        .advance_streaming(
            &[],
            true,
            recording_delta_sink(Arc::new(StdMutex::new(Vec::new()))),
        )
        .await
        .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert!(diagnostics.parse_error);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("sse_incomplete_frame")
    );
}

#[tokio::test]
async fn explicit_failure_after_text_is_not_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"upstream failed\"}}}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);

    let err = session
        .advance_streaming(
            &[],
            true,
            recording_delta_sink(Arc::new(StdMutex::new(Vec::new()))),
        )
        .await
        .unwrap_err();

    assert_eq!(err.stage, "sse");
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.explicit_failure_event);
    assert!(!diagnostics.normal_eof);
    assert_eq!(
        diagnostics.stream_end_kind.as_deref(),
        Some("explicit_failure_event")
    );
}

#[tokio::test]
async fn connection_reset_after_text_is_not_compatible_completion() {
    let base_url = spawn_reset_sse_stream().await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert!(matches!(
        err.code.as_str(),
        "network_error" | "provider_error"
    ));
    assert!(deltas.lock().unwrap().is_empty());
    let diagnostics = session.streaming_diagnostics();
    assert!(!diagnostics.normal_eof);
    assert!(diagnostics.connection_reset);
    assert_ne!(
        diagnostics.stream_end_kind.as_deref(),
        Some("normal_eof_compatible_completion")
    );
}
