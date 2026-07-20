//! Responses Tool Loop 的 input、工具定义、请求 payload 与上下文预算。

use std::borrow::Borrow;

use serde_json::{Value, json};

use crate::{
    context_budget::{ContextBudgetConfig, fit_tool_loop_payload},
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    tool::ToolMetadata,
};

use crate::provider::openai::payload::{openai_model_supports_reasoning, openai_responses_message};

pub(super) fn enforce_tool_loop_budget<P: Borrow<Value>>(
    context_budget: Option<ContextBudgetConfig>,
    payload: P,
) -> Result<(Value, bool), LlmError> {
    let payload = payload.borrow().clone();
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
        "tools": tools,
        // 首期只支持串行工具循环；后续多工具并行需要结果聚合和更细的权限审计。
        "parallel_tool_calls": false,
    });
    if let Some(effort) = reasoning_effort.filter(|_| openai_model_supports_reasoning(model)) {
        payload["reasoning"] = json!({ "effort": effort.as_str() });
    }
    if !allow_tool_calls {
        payload["tool_choice"] = json!("none");
    }
    if stream {
        payload["stream"] = json!(true);
    }
    payload
}
