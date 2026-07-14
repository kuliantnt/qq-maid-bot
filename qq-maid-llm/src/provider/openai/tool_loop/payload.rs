//! Responses Tool Loop 的 input、工具定义、请求 payload 与上下文预算。

use serde_json::{Value, json};

use crate::{
    context_budget::{
        BudgetItemKind, ContextBudgetConfig, ensure_required_budget, estimated_json_chars,
        log_budget_report,
    },
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    tool::ToolMetadata,
};

use crate::provider::openai::payload::{openai_model_supports_reasoning, openai_responses_message};

pub(super) fn enforce_tool_loop_budget(
    context_budget: Option<ContextBudgetConfig>,
    payload: &Value,
) -> Result<(), LlmError> {
    let Some(config) = context_budget else {
        return Ok(());
    };
    // Responses Tool Loop 首期不拆分、不淘汰已进入循环的结构化轮次；
    // 工具结果增长依靠单项结果上限和 max_rounds 控制，超预算时显式失败。
    // 只估算模型实际可见的 input 与 tools；model、stream、输出上限等 HTTP
    // 传输字段不占模型上下文，计入它们会在预算边界产生几十字符的误判。
    let model_context = json!({
        "input": payload.get("input"),
        "tools": payload.get("tools"),
    });
    let report = ensure_required_budget(
        config,
        BudgetItemKind::ToolLoopAtomicTurn,
        estimated_json_chars(&model_context, "tool_loop")?,
        "tool_loop",
    )?;
    log_budget_report("responses_tool_loop", &report);
    Ok(())
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
