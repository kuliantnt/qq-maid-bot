//! Responses function call 提取及 input/output 回填。

use serde_json::{Value, json};

use crate::{agent_loop::AgentToolResult, error::LlmError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FunctionCall {
    pub(super) name: String,
    pub(super) call_id: String,
    pub(super) arguments: String,
}

pub(super) fn append_tool_results(input: &mut Vec<Value>, results: &[AgentToolResult]) {
    for result in results {
        input.push(json!({
            "type": "function_call_output",
            "call_id": result.call_id,
            "output": result.output,
        }));
    }
}

pub(super) fn extract_function_calls(body: &Value) -> Result<Vec<FunctionCall>, LlmError> {
    let Some(output) = body.get("output").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let name = required_string(item, "name")?;
        let call_id = required_string(item, "call_id")?;
        let arguments = required_string(item, "arguments")?;
        calls.push(FunctionCall {
            name,
            call_id,
            arguments,
        });
    }
    Ok(calls)
}

pub(super) fn append_response_output_items(
    input: &mut Vec<Value>,
    body: &Value,
) -> Result<(), LlmError> {
    let Some(output) = body.get("output").and_then(Value::as_array) else {
        return Err(LlmError::provider(
            "OpenAI tool response missing output items",
            "provider",
        ));
    };
    for item in output {
        input.push(item.clone());
    }
    Ok(())
}

fn required_string(item: &Value, key: &str) -> Result<String, LlmError> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            LlmError::provider(
                format!("OpenAI function_call item missing `{key}`"),
                "provider",
            )
        })
}
