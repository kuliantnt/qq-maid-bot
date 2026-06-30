//! OpenAI Responses 原生 Function Tool Loop。
//!
//! 本模块只处理 Responses 协议层的 function call / function_call_output 往返。
//! 具体业务能力由上层 crate 通过 `ToolRegistry` 注册，避免 LLM crate 反向依赖 Core。

use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::{
    error::LlmError,
    metrics::MetricsRecorder,
    provider::{
        ChatOutcome,
        types::{ChatMessage, TokenUsage},
    },
    tool::{PreparedToolCall, ToolCallDependency, ToolContext, ToolMetadata, ToolRegistry},
};

use super::{
    extract::{extract_response_output_text, extract_response_usage},
    payload::openai_responses_message,
    transport::send_openai_responses_request,
};

/// OpenAI Tool Loop 请求上下文。
pub(crate) struct OpenAiToolLoopRequest<'a> {
    pub(crate) client: &'a reqwest::Client,
    pub(crate) api_key: &'a str,
    pub(crate) base_url: Option<&'a str>,
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) max_output_tokens: u64,
    pub(crate) messages: &'a [ChatMessage],
    pub(crate) tools: ToolRegistry,
    pub(crate) tool_context: ToolContext,
    pub(crate) max_rounds: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionCall {
    name: String,
    call_id: String,
    arguments: String,
}

struct PreparedFunctionCall {
    call_id: String,
    prepared: PreparedToolCall,
}

pub(crate) async fn openai_responses_tool_loop(
    req: OpenAiToolLoopRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    if req.tools.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "tool loop requires at least one registered tool",
            "tool_loop",
        ));
    }
    if req.max_rounds == 0 {
        return Err(LlmError::new(
            "bad_request",
            "tool loop max_rounds must be positive",
            "tool_loop",
        ));
    }

    let recorder = MetricsRecorder::start();
    let mut input = openai_tool_loop_input(req.messages)?;
    let tools = openai_tool_defs(req.tools.metadata());
    let mut usage = None;
    let mut executed_tools = Vec::new();

    for round in 0..=req.max_rounds {
        let payload = openai_tool_loop_payload(
            &input,
            &tools,
            req.model,
            req.max_output_tokens,
            round < req.max_rounds,
        );
        let response =
            send_openai_responses_request(req.client, req.api_key, req.base_url, &payload, false)
                .await?;
        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid OpenAI tool loop JSON: {err}"), "json")
        })?;
        usage = merge_usage(usage, extract_response_usage(&body));
        let calls = extract_function_calls(&body)?;
        if calls.is_empty() {
            let reply = extract_response_output_text(&body).ok_or_else(|| {
                LlmError::provider(
                    "OpenAI tool loop returned empty final text output",
                    "provider",
                )
            })?;
            debug!(
                provider = req.provider,
                model = req.model,
                tool_loop_used = true,
                tool_loop_rounds = round,
                "openai tool loop completed with final reply"
            );
            return Ok(ChatOutcome {
                reply,
                metrics: recorder.finish(req.provider, req.model, false),
                usage,
                fallback_used: false,
                executed_tools,
            });
        }
        if round >= req.max_rounds {
            warn!(
                provider = req.provider,
                model = req.model,
                tool_loop_used = true,
                tool_loop_rounds = round,
                max_rounds = req.max_rounds,
                "openai tool loop exceeded maximum rounds"
            );
            return Err(LlmError::new(
                "tool_loop_limit",
                "tool loop exceeded maximum rounds",
                "tool_loop",
            ));
        }
        append_response_output_items(&mut input, &body)?;
        let prepared_calls = prepare_function_calls(&req, &calls, round)?;
        let mut previous_call_succeeded = true;
        for call in prepared_calls {
            executed_tools.push(call.prepared.name.clone());
            let (output, succeeded) = if call.prepared.dependency
                == ToolCallDependency::PreviousCallSuccess
                && !previous_call_succeeded
            {
                (tool_skip_output("dependency_previous_call_failed"), false)
            } else {
                match req.tools.execute_prepared(call.prepared).await {
                    Ok(output) => (output, true),
                    Err(err) => (tool_error_output(&err), false),
                }
            };
            previous_call_succeeded = succeeded;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": output,
            }));
        }
    }

    Err(LlmError::new(
        "tool_loop_limit",
        "tool loop exceeded maximum rounds",
        "tool_loop",
    ))
}

fn prepare_function_calls(
    req: &OpenAiToolLoopRequest<'_>,
    calls: &[FunctionCall],
    round: usize,
) -> Result<Vec<PreparedFunctionCall>, LlmError> {
    let mut prepared_calls = Vec::with_capacity(calls.len());
    for (index, call) in calls.iter().enumerate() {
        let mut context = req.tool_context.clone();
        context.tool_call_id = Some(stable_tool_call_id(
            &context.task_id,
            &call.call_id,
            round,
            index,
        ));
        let prepared = req
            .tools
            .prepare_json(&context, &call.name, &call.arguments)?;
        prepared_calls.push(PreparedFunctionCall {
            call_id: call.call_id.clone(),
            prepared,
        });
    }
    Ok(prepared_calls)
}

fn stable_tool_call_id(task_id: &str, call_id: &str, round: usize, index: usize) -> String {
    let call_id = call_id.trim();
    if !call_id.is_empty() {
        format!("{task_id}:{call_id}")
    } else {
        // 兼容上游未返回稳定 call_id 的场景，回退到 request + round + index。
        format!("{task_id}:round-{round}:call-{index}")
    }
}

fn tool_error_output(err: &LlmError) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "error": {
            "code": err.code,
            "message": err.message,
            "stage": err.stage,
        }
    }))
    .unwrap_or_else(|_| r#"{"ok":false,"error":{"code":"tool_output_error","message":"failed to serialize tool error","stage":"tool_loop"}}"#.to_owned())
}

fn tool_skip_output(reason: &str) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "skipped": true,
        "reason": reason,
    }))
    .unwrap_or_else(|_| {
        r#"{"ok":false,"skipped":true,"reason":"dependency_previous_call_failed"}"#.to_owned()
    })
}

fn openai_tool_loop_input(messages: &[ChatMessage]) -> Result<Vec<Value>, LlmError> {
    let input = messages
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .map(openai_responses_message)
        .collect::<Vec<_>>();
    if input.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must not be empty",
            "request",
        ));
    }
    Ok(input)
}

fn openai_tool_defs(metadata: Vec<ToolMetadata>) -> Vec<Value> {
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

fn openai_tool_loop_payload(
    input: &[Value],
    tools: &[Value],
    model: &str,
    max_output_tokens: u64,
    allow_tool_calls: bool,
) -> Value {
    let mut payload = json!({
        "model": model,
        "input": input,
        "max_output_tokens": max_output_tokens,
        "tools": tools,
        // 首期只支持串行工具循环；后续多工具并行需要结果聚合和更细的权限审计。
        "parallel_tool_calls": false,
    });
    if !allow_tool_calls {
        payload["tool_choice"] = json!("none");
    }
    payload
}

fn extract_function_calls(body: &Value) -> Result<Vec<FunctionCall>, LlmError> {
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

fn append_response_output_items(input: &mut Vec<Value>, body: &Value) -> Result<(), LlmError> {
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

fn merge_usage(current: Option<TokenUsage>, next: Option<TokenUsage>) -> Option<TokenUsage> {
    match (current, next) {
        (None, next) => next,
        (current, None) => current,
        (Some(left), Some(right)) => Some(TokenUsage {
            input_tokens: add_optional(left.input_tokens, right.input_tokens),
            cached_input_tokens: add_optional(left.cached_input_tokens, right.cached_input_tokens),
            output_tokens: add_optional(left.output_tokens, right.output_tokens),
            total_tokens: add_optional(left.total_tokens, right.total_tokens),
        }),
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use axum::{Json, Router, extract::State, routing::post};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{net::TcpListener, sync::Mutex};

    struct WeatherToolStub;

    #[async_trait]
    impl Tool for WeatherToolStub {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "get_weather".to_owned(),
                description: "get weather".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    },
                    "required": ["city"],
                    "additionalProperties": false
                }),
            }
        }

        async fn execute(
            &self,
            _context: ToolContext,
            arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            Ok(ToolOutput::json(json!({
                "city": arguments["city"],
                "weather": "小雨"
            })))
        }
    }

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            tool_call_id: None,
        }
    }

    struct SequenceToolStub {
        fail: bool,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SequenceToolStub {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: if self.fail {
                    "fail_tool".to_owned()
                } else {
                    "ok_tool".to_owned()
                },
                description: "sequence test".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "value": {"type": "string"}
                    },
                    "required": ["value"],
                    "additionalProperties": false
                }),
            }
        }

        fn prepare(
            &self,
            _context: &ToolContext,
            arguments: Value,
        ) -> Result<crate::tool::ToolPreparation, LlmError> {
            let mut prepared = crate::tool::ToolPreparation::ready(arguments);
            if !self.fail {
                prepared = prepared.with_dependency(ToolCallDependency::PreviousCallSuccess);
            }
            Ok(prepared)
        }

        async fn execute(
            &self,
            _context: ToolContext,
            arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(LlmError::new("tool_failed", "simulated failure", "tool"));
            }
            Ok(ToolOutput::json(json!({
                "ok": true,
                "value": arguments["value"],
            })))
        }
    }

    #[derive(Debug)]
    struct ToolLoopMockState {
        requests: Vec<Value>,
    }

    async fn mock_tool_loop_handler(
        State(state): State<Arc<Mutex<ToolLoopMockState>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut state = state.lock().await;
        state.requests.push(body);
        if state.requests.len() == 1 {
            return Json(json!({
                "output": [{
                    "type": "function_call",
                    "name": "get_weather",
                    "call_id": "call_weather_1",
                    "arguments": "{\"city\":\"杭州\"}"
                }]
            }));
        }
        Json(json!({
            "output_text": "杭州今天有小雨，建议带伞。",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "杭州今天有小雨，建议带伞。"}]
            }]
        }))
    }

    async fn mock_multi_tool_handler(
        State(state): State<Arc<Mutex<ToolLoopMockState>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut state = state.lock().await;
        state.requests.push(body);
        if state.requests.len() == 1 {
            return Json(json!({
                "output": [
                    {
                        "type": "function_call",
                        "name": "fail_tool",
                        "call_id": "call_fail_1",
                        "arguments": "{\"value\":\"first\"}"
                    },
                    {
                        "type": "function_call",
                        "name": "ok_tool",
                        "call_id": "call_ok_1",
                        "arguments": "{\"value\":\"second\"}"
                    }
                ]
            }));
        }
        Json(json!({
            "output_text": "已经汇总结果。",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "已经汇总结果。"}]
            }]
        }))
    }

    async fn spawn_tool_loop_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
        let state = Arc::new(Mutex::new(ToolLoopMockState {
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_tool_loop_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    async fn spawn_multi_tool_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
        let state = Arc::new(Mutex::new(ToolLoopMockState {
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_multi_tool_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    #[test]
    fn extract_function_calls_reads_native_responses_items() {
        let body = json!({
            "output": [{
                "type": "function_call",
                "name": "get_weather",
                "call_id": "call_1",
                "arguments": "{\"city\":\"杭州\"}"
            }]
        });

        let calls = extract_function_calls(&body).unwrap();

        assert_eq!(
            calls,
            vec![FunctionCall {
                name: "get_weather".to_owned(),
                call_id: "call_1".to_owned(),
                arguments: "{\"city\":\"杭州\"}".to_owned(),
            }]
        );
    }

    #[test]
    fn payload_disables_parallel_tool_calls() {
        let payload = openai_tool_loop_payload(
            &[json!({"role": "user", "content": "杭州今天要带伞吗"})],
            &[json!({"type": "function", "name": "get_weather"})],
            "gpt-test",
            1200,
            true,
        );

        assert_eq!(payload["parallel_tool_calls"], false);
        assert!(payload.get("tool_choice").is_none());
    }

    #[tokio::test]
    async fn tool_loop_executes_function_call_and_returns_output_to_model() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州今天需要带伞吗？")],
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "杭州今天有小雨，建议带伞。");
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        assert_eq!(state.requests[0]["tools"][0]["name"], "get_weather");
        assert_eq!(state.requests[0]["parallel_tool_calls"], false);
        let second_input = state.requests[1]["input"].as_array().unwrap();
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_weather_1"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"weather\":\"小雨\""))
        }));
    }

    #[tokio::test]
    async fn tool_loop_serializes_multiple_calls_and_skips_dependent_call_after_failure() {
        let (base_url, state) = spawn_multi_tool_mock().await;
        let fail_calls = Arc::new(AtomicUsize::new(0));
        let ok_calls = Arc::new(AtomicUsize::new(0));
        let registry = ToolRegistry::new()
            .register(SequenceToolStub {
                fail: true,
                calls: fail_calls.clone(),
            })
            .unwrap()
            .register(SequenceToolStub {
                fail: false,
                calls: ok_calls.clone(),
            })
            .unwrap();
        let client = reqwest::Client::new();

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("连续执行两个工具")],
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "已经汇总结果。");
        assert_eq!(fail_calls.load(Ordering::SeqCst), 1);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 0);
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        let second_input = state.requests[1]["input"].as_array().unwrap();
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_fail_1"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"tool_failed\""))
        }));
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_ok_1"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"skipped\":true"))
        }));
    }
}
