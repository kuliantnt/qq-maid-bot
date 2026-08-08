use super::*;
use crate::agent_loop::AgentToolResult;

#[derive(Debug, Default)]
struct TimeoutFallbackState {
    requests: Vec<Value>,
}

async fn timeout_then_non_stream_handler(
    State(state): State<Arc<Mutex<TimeoutFallbackState>>>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    state.lock().await.requests.push(body);
    if streaming {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(stream::pending::<
                Result<Bytes, Infallible>,
            >()))
            .unwrap();
    }
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "output_text": "fallback answer",
                "output": [{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "fallback answer"}]
                }]
            })
            .to_string(),
        ))
        .unwrap()
}

async fn spawn_timeout_then_non_stream_mock() -> (String, Arc<Mutex<TimeoutFallbackState>>) {
    let state = Arc::new(Mutex::new(TimeoutFallbackState::default()));
    let app = Router::new()
        .route("/v1/responses", post(timeout_then_non_stream_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), state)
}

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
async fn cancelled_streaming_attempt_does_not_duplicate_fallback_input() {
    let (base_url, state) = spawn_timeout_then_non_stream_mock().await;
    let mut session = test_streaming_session(base_url);
    let results = vec![AgentToolResult {
        call_id: "call_weather_1".to_owned(),
        output: r#"{"weather":"rain"}"#.to_owned(),
    }];

    // 流式首活动超时会丢弃在 clone 上构造的 input；随后发起第二个完整的
    // 非流式兼容请求，但同一批工具结果在协议 input 中只能出现一次。
    let timed_out = tokio::time::timeout(
        Duration::from_millis(20),
        session.advance_streaming(
            &results,
            true,
            recording_delta_sink(Arc::new(StdMutex::new(Vec::new()))),
        ),
    )
    .await;
    assert!(timed_out.is_err());

    let step = session.advance(&results, true).await.unwrap();
    assert!(matches!(step, AgentStep::FinalAnswer { .. }));

    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["stream"], true);
    assert!(requests[1].get("stream").is_none());
    assert_eq!(requests[0]["input"], requests[1]["input"]);
    for request in requests {
        let outputs = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "call_weather_1"
            })
            .count();
        assert_eq!(outputs, 1);
    }
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
async fn responses_tool_events_never_become_visible_text() {
    let arguments = r#"{"query":"华为手机价格","rawquestion":"继续查具体型号","maxresults":5,"contextsize":"medium","topic":"general","timerange":"month","time_range":"month"}"#;
    let function_call = json!({
        "type": "function_call",
        "id": "fc_1",
        "name": "web_search",
        "call_id": "call_search_1",
        "arguments": arguments
    });
    let events = [
        (
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "delta": "再查一下",
                "sequence_number": 0,
                "output_index": 0,
                "content_index": 0
            }),
        ),
        (
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "sequence_number": 1,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "name": "web_search",
                    "call_id": "call_search_1",
                    "arguments": ""
                }
            }),
        ),
        (
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 1,
                "sequence_number": 2,
                "delta": arguments
            }),
        ),
        (
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 3,
                "part": {"type": "output_text", "text": "再查一下"}
            }),
        ),
        (
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "sequence_number": 4,
                "item": function_call.clone()
            }),
        ),
        (
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {"output": [function_call]},
                "sequence_number": 5
            }),
        ),
    ];
    let body = events
        .into_iter()
        .map(|(event_type, value)| format!("event: {event_type}\ndata: {value}\n\n"))
        .collect::<String>();
    let base_url = spawn_static_sse_stream(body).await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let step = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap()
        .unwrap();

    let AgentStep::ToolCalls { calls, .. } = step else {
        panic!("expected tool calls");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "web_search");
    assert_eq!(calls[0].arguments, arguments);
    assert!(deltas.lock().unwrap().is_empty());
}

#[tokio::test]
async fn parallel_function_argument_deltas_stay_isolated_and_invisible() {
    let calls = (0..3)
        .map(|index| {
            json!({
                "type": "function_call",
                "name": "web_search",
                "call_id": format!("call_{index}"),
                "arguments": format!(r#"{{"query":"phone-{index}","time_range":"month"}}"#)
            })
        })
        .collect::<Vec<_>>();
    let mut frames = String::new();
    for index in 0..3 {
        frames.push_str(&format!(
            "event: response.output_item.added\ndata: {}\n\n",
            json!({
                "type": "response.output_item.added",
                "output_index": index,
                "sequence_number": index * 3,
                "item": {"type": "function_call", "id": format!("fc_{index}")}
            })
        ));
    }
    for index in 0..3 {
        frames.push_str(&format!(
            "event: response.function_call_arguments.delta\ndata: {}\n\n",
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": index,
                "sequence_number": index * 3 + 1,
                "delta": format!(r#"{{"query":"phone-{index}","time_range":"month"}}"#)
            })
        ));
    }
    for (index, call) in calls.iter().enumerate() {
        frames.push_str(&format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "output_index": index,
                "sequence_number": index * 3 + 2,
                "item": call
            })
        ));
    }
    frames.push_str(&format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": {"output": calls},
            "sequence_number": 10
        })
    ));
    let base_url = spawn_static_sse_stream(frames).await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let step = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap()
        .unwrap();

    let AgentStep::ToolCalls { calls, .. } = step else {
        panic!("expected parallel tool calls");
    };
    assert_eq!(calls.len(), 3);
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call.call_id, format!("call_{index}"));
        assert_eq!(
            call.arguments,
            format!(r#"{{"query":"phone-{index}","time_range":"month"}}"#)
        );
    }
    assert!(deltas.lock().unwrap().is_empty());
}

#[tokio::test]
async fn response_incomplete_records_structured_reason_without_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.incomplete\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[]},\"sequence_number\":7}\n\n",
    ))
    .await;
    let mut session = test_streaming_session(base_url);
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = session
        .advance_streaming(&[], true, recording_delta_sink(deltas.clone()))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "sse");
    assert_eq!(err.incomplete_reason(), Some("max_output_tokens"));
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.explicit_failure_event);
    assert_eq!(
        diagnostics.incomplete_reason.as_deref(),
        Some("max_output_tokens")
    );
    assert!(deltas.lock().unwrap().is_empty());
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
async fn normal_eof_with_text_and_extra_newline_is_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你\"}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"好\"}\n\n\n",
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
async fn normal_eof_with_text_and_keep_alive_comment_is_compatible_completion() {
    let base_url = spawn_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
        ": keep-alive",
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
    assert_eq!(*deltas.lock().unwrap(), ["你好"]);
    let diagnostics = session.streaming_diagnostics();
    assert!(diagnostics.normal_eof);
    assert!(!diagnostics.parse_error);
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
    let (base_url, calls) = spawn_counted_static_sse_stream(concat!(
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"草稿\"}\n\n",
        "event: response.output_text.delta",
    ))
    .await;
    let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = run_agent_loop(
        Box::new(
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
            .unwrap(),
        ),
        registry,
        test_context(),
        3,
        None,
        Some(recording_delta_sink(deltas.clone())),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "sse_incomplete_frame");
    assert_eq!(err.stage, "stream_after_delta");
    assert!(deltas.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
