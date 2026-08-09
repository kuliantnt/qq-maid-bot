//! Responses Tool Loop 的 input、工具定义、请求 payload 与上下文预算。

use serde_json::{Value, json};

use crate::{
    agent_loop::AgentInputSizeEstimate,
    context_budget::{ContextBudgetConfig, estimated_json_chars_counting, fit_tool_loop_payload},
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    tool::ToolMetadata,
};

use crate::provider::openai::payload::{openai_model_supports_reasoning, openai_responses_message};

/// 对 Tool Loop payload 应用上下文预算。
///
/// 直接按值接收 payload 并在预算校验时原地裁剪，避免 `fit_tool_loop_payload`
/// 之外再保留一份完整 payload 副本（Issue #361 请求期内存放大点）。
pub(super) fn enforce_tool_loop_budget(
    context_budget: Option<ContextBudgetConfig>,
    payload: Value,
) -> Result<(Value, bool), LlmError> {
    let Some(config) = context_budget else {
        return Ok((payload, false));
    };
    fit_tool_loop_payload(config, payload, "tool_loop")
}

pub(super) fn openai_tool_loop_input(
    messages: &[ChatMessage],
    media_max_bytes: u64,
) -> Result<Vec<Value>, LlmError> {
    let input = messages
        .iter()
        .filter(|message| !message.content.trim().is_empty() || !message.content_parts.is_empty())
        .map(|message| openai_responses_message(message, media_max_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    if input.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must not be empty",
            "request",
        ));
    }
    Ok(input)
}

/// 估算 Responses 会话 `input` 的尺寸；只用于 Issue #361 诊断，不参与预算。
///
/// `estimated_chars` 只在 DEBUG 级别开启时计算，避免每轮为诊断额外序列化
/// 整个上下文；序列化估算走不保留正文的 counting writer，不会在堆上生成
/// 完整 String 副本。`tool_result_chars` 只统计 `function_call_output` 的输出字符数。
pub(super) fn responses_input_size_estimate(input: &[Value]) -> AgentInputSizeEstimate {
    let mut estimate = AgentInputSizeEstimate {
        item_count: input.len(),
        ..Default::default()
    };
    for item in input {
        if item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && let Some(output) = item.get("output").and_then(Value::as_str)
        {
            estimate.tool_result_chars = estimate
                .tool_result_chars
                .saturating_add(output.chars().count());
        }
    }
    if tracing::enabled!(tracing::Level::DEBUG)
        && let Ok(chars) = estimated_json_chars_counting(input, "tool_loop_diagnostics")
    {
        estimate.estimated_chars = chars;
    }
    estimate
}

pub(super) fn openai_tool_defs(metadata: Vec<ToolMetadata>) -> Vec<Value> {
    metadata
        .into_iter()
        .map(|item| {
            json!({
                "type": "function",
                "name": item.name,
                "description": item.description,
                "parameters": item.parameters,
                "strict": true,
            })
        })
        .collect()
}

pub(super) fn openai_tool_loop_payload(
    input: &[Value],
    tools: &[Value],
    model: &str,
    max_output_tokens: u64,
    reasoning_effort: Option<ReasoningEffort>,
    allow_tool_calls: bool,
    stream: bool,
) -> Value {
    let mut payload = json!({
        "model": model,
        "input": input,
        "max_output_tokens": max_output_tokens,
    });
    if allow_tool_calls {
        payload["tools"] = json!(tools);
        // 首期只支持串行工具循环；后续多工具并行需要结果聚合和更细的权限审计。
        payload["parallel_tool_calls"] = json!(false);
    }
    if let Some(effort) = reasoning_effort.filter(|_| openai_model_supports_reasoning(model)) {
        payload["reasoning"] = json!({ "effort": effort.as_str() });
    }
    if stream {
        payload["stream"] = json!(true);
    }
    payload
}
