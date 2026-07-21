use super::*;

#[test]
fn payload_disables_parallel_tool_calls() {
    let payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "杭州今天要带伞吗"})],
        &[json!({"type": "function", "name": "get_weather"})],
        "gpt-test",
        1200,
        None,
        true,
        false,
    );

    assert_eq!(payload["parallel_tool_calls"], false);
    assert!(payload.get("tool_choice").is_none());
    assert!(payload.get("stream").is_none());
}

#[test]
fn payload_disables_tool_calls_explicitly() {
    let payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "总结已有结果"})],
        &[json!({"type": "function", "name": "search"})],
        "gpt-test",
        1200,
        None,
        false,
        false,
    );

    assert!(payload.get("tools").is_none());
    assert!(payload.get("tool_choice").is_none());
    assert!(payload.get("parallel_tool_calls").is_none());

    let streaming_payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "总结已有结果"})],
        &[json!({"type": "function", "name": "search"})],
        "gpt-test",
        1200,
        None,
        false,
        true,
    );
    assert!(streaming_payload.get("tools").is_none());
    assert!(streaming_payload.get("tool_choice").is_none());
    assert!(streaming_payload.get("parallel_tool_calls").is_none());
    assert_eq!(streaming_payload["stream"], true);
}

#[test]
fn streaming_payload_enables_responses_stream() {
    let payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "test"})],
        &[json!({"type": "function", "name": "get_weather"})],
        "gpt-test",
        1200,
        None,
        true,
        true,
    );

    assert_eq!(payload["stream"], true);
}

#[test]
fn later_tool_round_only_appends_to_existing_input_prefix() {
    let first_input = vec![
        json!({"type": "message", "role": "system", "content": [{"type": "input_text", "text": "固定前缀"}]}),
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "当前问题"}]}),
    ];
    let mut later_input = first_input.clone();
    later_input.extend([
        json!({"type": "function_call", "name": "get_weather", "call_id": "call-1", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "call-1", "output": "{\"ok\":true}"}),
    ]);

    let first_bytes = serde_json::to_vec(&first_input).unwrap();
    let later_bytes = serde_json::to_vec(&later_input).unwrap();
    assert_eq!(
        &first_bytes[..first_bytes.len() - 1],
        &later_bytes[..first_bytes.len() - 1]
    );
}

#[test]
fn payload_includes_reasoning_effort_for_reasoning_models() {
    let payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "复杂问题"})],
        &[json!({"type": "function", "name": "get_weather"})],
        "gpt-5.5",
        1200,
        Some(ReasoningEffort::High),
        true,
        false,
    );

    assert_eq!(payload["reasoning"]["effort"], "high");
}

#[test]
fn payload_omits_reasoning_effort_for_non_reasoning_models() {
    let payload = openai_tool_loop_payload(
        &[json!({"role": "user", "content": "复杂问题"})],
        &[json!({"type": "function", "name": "get_weather"})],
        "gpt-4.1",
        1200,
        Some(ReasoningEffort::High),
        true,
        false,
    );

    assert!(payload.get("reasoning").is_none());
}

#[test]
fn tool_loop_budget_ignores_transport_only_payload_fields() {
    let input = vec![json!({
        "role": "user",
        "content": [{"type": "input_text", "text": "完成待办"}],
    })];
    let tools = vec![json!({
        "type": "function",
        "name": "list_todos",
        "description": "列出待办",
        "parameters": {"type": "object", "properties": {}},
    })];
    let payload = openai_tool_loop_payload(
        &input,
        &tools,
        &"model-name-that-must-not-count".repeat(20),
        1200,
        None,
        true,
        true,
    );
    let model_context = json!({"input": input, "tools": tools});
    let model_context_chars = estimated_json_chars(&model_context, "tool_loop").unwrap();
    assert!(estimated_json_chars(&payload, "tool_loop").unwrap() > model_context_chars);

    enforce_tool_loop_budget(
        Some(ContextBudgetConfig {
            context_window_chars: model_context_chars + 20,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        }),
        &payload,
    )
    .unwrap();
}
