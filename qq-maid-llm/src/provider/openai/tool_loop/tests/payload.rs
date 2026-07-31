use super::super::response::append_tool_results;
use super::*;
use crate::{
    agent_loop::AgentToolResult, context_budget::estimated_json_chars,
    provider::openai::tool_loop::payload::responses_input_size_estimate,
};

#[test]
fn input_size_estimate_counts_items_and_tool_result_chars() {
    let input = vec![
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "完成"}]}),
        json!({"type": "function_call", "call_id": "call-1", "name": "complete_todos", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "call-1", "output": "结果正文".repeat(10)}),
    ];

    let estimate = responses_input_size_estimate(&input);

    assert_eq!(estimate.item_count, 3);
    assert_eq!(
        estimate.tool_result_chars,
        "结果正文".repeat(10).chars().count()
    );
}

#[test]
fn input_size_estimate_after_append_reflects_appended_tool_results() {
    // Issue #361 诊断口径：`append_tool_results` 之后、payload 构造之前的
    // 输入尺寸才包含本轮结果；验证 append 前后估算的变化语义，避免把
    // “未追加”的会话状态误报为发送尺寸。
    let mut input = vec![json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "完成待办"}],
    })];
    let before = responses_input_size_estimate(&input);
    assert_eq!(before.item_count, 1);
    assert_eq!(before.tool_result_chars, 0);

    append_tool_results(
        &mut input,
        &[AgentToolResult {
            call_id: "call-1".to_owned(),
            output: "{\"ok\":true}".repeat(20),
        }],
    );

    let after = responses_input_size_estimate(&input);
    assert_eq!(after.item_count, 2);
    assert_eq!(
        after.tool_result_chars,
        "{\"ok\":true}".repeat(20).chars().count()
    );
    assert!(after.tool_result_chars > before.tool_result_chars);
}

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
        payload,
    )
    .unwrap();
}

#[test]
fn responses_tool_loop_budget_keeps_large_structured_image_payload() {
    let data_url = format!("data:image/png;base64,{}", "a".repeat(100_000));
    let input = vec![json!({
        "type": "message",
        "role": "user",
        "content": [
            {"type": "input_text", "text": "帮我看看这个"},
            {"type": "input_image", "image_url": data_url}
        ],
    })];
    let tools = vec![json!({
        "type": "function",
        "name": "inspect",
        "parameters": {"type": "object", "properties": {}},
    })];
    let payload = openai_tool_loop_payload(&input, &tools, "gpt-test", 1200, None, true, false);

    let (fitted, disabled) = enforce_tool_loop_budget(
        Some(ContextBudgetConfig {
            context_window_chars: 2_500,
            output_reserve_chars: 200,
            protected_recent_turns: 0,
        }),
        payload.clone(),
    )
    .unwrap();

    assert!(!disabled);
    assert_eq!(fitted, payload);
    assert!(
        fitted["input"][0]["content"][1]["image_url"]
            .as_str()
            .is_some_and(|url| url.len() > 100_000)
    );
}
