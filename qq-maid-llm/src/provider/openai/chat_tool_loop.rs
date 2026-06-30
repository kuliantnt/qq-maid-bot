//! OpenAI 兼容 Chat Completions Tool Loop。
//!
//! DeepSeek 和 BigModel 都通过 `/chat/completions` 暴露 `tools` / `tool_calls`
//! 协议，这里统一处理协议层的多轮往返，避免 provider 侧重复维护同一套工具循环。

use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::{
    context_budget::{
        BudgetItemKind, ContextBudgetConfig, ensure_required_budget, estimated_json_chars,
        log_budget_report,
    },
    error::LlmError,
    metrics::MetricsRecorder,
    provider::{
        ChatOutcome, ToolCallingProtocol, ToolChatRequest, ToolExecutionResult,
        types::{ChatMessage, TokenUsage},
    },
    tool::{PreparedToolCall, ToolCallDependency, ToolContext, ToolMetadata, ToolRegistry},
};

use super::chat::{
    ChatCompletionsClient, chat_completions_messages, extract_chat_completion_text,
    extract_chat_completion_usage, send_chat_completions_request,
};

/// Chat Completions Tool Loop 请求上下文。
pub(crate) struct ChatCompletionsToolLoopRequest<'a> {
    pub(crate) client: &'a ChatCompletionsClient,
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) max_output_tokens: u64,
    pub(crate) messages: &'a [ChatMessage],
    pub(crate) context_budget: Option<ContextBudgetConfig>,
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

struct ToolCallRound {
    assistant_message: Value,
    calls: Vec<FunctionCall>,
}

struct PreparedFunctionCall {
    call_id: String,
    prepared: Result<PreparedToolCall, LlmError>,
}

pub(crate) async fn chat_completions_tool_loop(
    req: ChatCompletionsToolLoopRequest<'_>,
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
    let mut messages = chat_completions_messages(req.messages)?;
    let tools = chat_completions_tool_defs(req.tools.metadata());
    let mut usage = None;
    let mut executed_tools = Vec::new();
    let mut tool_results = Vec::new();

    for round in 0..=req.max_rounds {
        let payload =
            chat_completions_tool_loop_payload(&messages, &tools, req.model, req.max_output_tokens);
        enforce_tool_loop_budget(req.context_budget, &payload)?;
        let response = send_chat_completions_request(req.client, &payload, false).await?;
        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid Chat Completions tool loop JSON: {err}"),
                "json",
            )
        })?;
        usage = merge_usage(usage, extract_chat_completion_usage(&body));
        let tool_rounds = extract_tool_call_rounds(&body)?;
        if tool_rounds.is_empty() {
            let reply = extract_chat_completion_text(&body).ok_or_else(|| {
                LlmError::provider(
                    "Chat Completions tool loop returned empty final text output",
                    "provider",
                )
            })?;
            debug!(
                provider = req.provider,
                model = req.model,
                tool_loop_used = true,
                tool_loop_rounds = round,
                "chat completions tool loop completed with final reply"
            );
            return Ok(ChatOutcome {
                reply,
                metrics: recorder.finish(req.provider, req.model, false),
                usage,
                fallback_used: false,
                executed_tools,
                tool_results,
            });
        }
        if round >= req.max_rounds {
            warn!(
                provider = req.provider,
                model = req.model,
                tool_loop_used = true,
                tool_loop_rounds = round,
                max_rounds = req.max_rounds,
                "chat completions tool loop exceeded maximum rounds"
            );
            return Err(LlmError::new(
                "tool_loop_limit",
                "tool loop exceeded maximum rounds",
                "tool_loop",
            ));
        }

        let mut previous_call_succeeded = true;
        for tool_round in tool_rounds {
            messages.push(tool_round.assistant_message);
            let prepared_calls = prepare_function_calls(&req, &tool_round.calls, round)?;
            for call in prepared_calls {
                let (tool_name, output, succeeded) = match call.prepared {
                    Ok(prepared) => {
                        let tool_name = prepared.name.clone();
                        executed_tools.push(tool_name.clone());
                        if prepared.dependency == ToolCallDependency::PreviousCallSuccess
                            && !previous_call_succeeded
                        {
                            (
                                tool_name,
                                tool_skip_output("dependency_previous_call_failed"),
                                false,
                            )
                        } else {
                            match req.tools.execute_prepared(prepared).await {
                                Ok(output) => {
                                    let succeeded = tool_output_indicates_success(&output);
                                    (tool_name, output, succeeded)
                                }
                                Err(err) => (tool_name, tool_error_output(&err), false),
                            }
                        }
                    }
                    Err(err) => ("unknown".to_owned(), tool_error_output(&err), false),
                };
                previous_call_succeeded = succeeded;
                if tool_name != "unknown" {
                    tool_results.push(tool_execution_result(&tool_name, &output, succeeded));
                }
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call.call_id,
                    "content": output,
                }));
            }
        }
    }

    Err(LlmError::new(
        "tool_loop_limit",
        "tool loop exceeded maximum rounds",
        "tool_loop",
    ))
}

fn enforce_tool_loop_budget(
    context_budget: Option<ContextBudgetConfig>,
    payload: &Value,
) -> Result<(), LlmError> {
    let Some(config) = context_budget else {
        return Ok(());
    };
    // Chat Completions 的 assistant tool_calls 与对应 tool messages 必须成组保留；
    // 首期只做完整 payload 检查，不静默删除任何工具轮次。
    let report = ensure_required_budget(
        config,
        BudgetItemKind::ToolLoopAtomicTurn,
        estimated_json_chars(payload, "tool_loop")?,
        "tool_loop",
    )?;
    log_budget_report("chat_completions_tool_loop", &report);
    Ok(())
}

/// 把“OpenAI 兼容 Chat Completions provider 的工具调用接线”收敛成公共 helper。
///
/// DeepSeek / BigModel 的差异主要在模型前缀校验和默认 base URL，不值得各自复制
/// 一整段 tool loop 入口逻辑。
pub(crate) async fn provider_chat_with_chat_completions_tools<F>(
    client: &ChatCompletionsClient,
    provider: &'static str,
    default_model: &str,
    max_output_tokens: u64,
    req: ToolChatRequest,
    resolve_model: F,
) -> Result<ChatOutcome, LlmError>
where
    F: FnOnce(Option<&str>, &str) -> Result<String, LlmError>,
{
    let effective_model = resolve_model(req.chat.model.as_deref(), default_model)?;
    chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
        client,
        provider,
        model: &effective_model,
        max_output_tokens,
        messages: &req.chat.messages,
        context_budget: req.chat.context_budget,
        tools: req.tools,
        tool_context: req.tool_context,
        max_rounds: req.max_rounds,
    })
    .await
}

pub(crate) fn provider_chat_completions_tool_calling_protocol<F>(
    model: Option<&str>,
    default_model: &str,
    resolve_model: F,
) -> Option<ToolCallingProtocol>
where
    F: FnOnce(Option<&str>, &str) -> Result<String, LlmError>,
{
    resolve_model(model, default_model)
        .ok()
        .map(|_| ToolCallingProtocol::ChatCompletionsToolCalls)
}

fn chat_completions_tool_defs(metadata: Vec<ToolMetadata>) -> Vec<Value> {
    metadata
        .into_iter()
        .map(|item| {
            json!({
                "type": "function",
                "function": {
                    "name": item.name,
                    "description": item.description,
                    "parameters": item.parameters,
                }
            })
        })
        .collect()
}

fn chat_completions_tool_loop_payload(
    messages: &[Value],
    tools: &[Value],
    model: &str,
    max_output_tokens: u64,
) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "tools": tools,
        // BigModel 文档当前写明仅支持 auto，这里统一固定成兼容交集。
        "tool_choice": "auto",
    })
}

fn extract_tool_call_rounds(body: &Value) -> Result<Vec<ToolCallRound>, LlmError> {
    let Some(choices) = body.get("choices").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut rounds = Vec::new();
    for choice in choices {
        let Some(message) = choice.get("message") else {
            continue;
        };
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        if tool_calls.is_empty() {
            continue;
        }
        let mut calls = Vec::new();
        for call in tool_calls {
            let call_type = call
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function");
            if call_type != "function" {
                continue;
            }
            let function = call.get("function").ok_or_else(|| {
                LlmError::provider(
                    "Chat Completions tool call item missing `function`",
                    "provider",
                )
            })?;
            calls.push(FunctionCall {
                name: required_string(function, "name", "Chat Completions function")?,
                call_id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| call.get("call_id").and_then(Value::as_str))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        LlmError::provider(
                            "Chat Completions tool call item missing `id`",
                            "provider",
                        )
                    })?,
                arguments: required_string(function, "arguments", "Chat Completions function")?,
            });
        }
        if calls.is_empty() {
            continue;
        }
        let mut assistant_message = message.clone();
        if assistant_message
            .get("role")
            .and_then(Value::as_str)
            .is_none()
        {
            assistant_message["role"] = json!("assistant");
        }
        rounds.push(ToolCallRound {
            assistant_message,
            calls,
        });
    }
    Ok(rounds)
}

fn prepare_function_calls(
    req: &ChatCompletionsToolLoopRequest<'_>,
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
        prepared_calls.push(PreparedFunctionCall {
            call_id: call.call_id.clone(),
            prepared: req
                .tools
                .prepare_json(&context, &call.name, &call.arguments),
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

fn tool_output_indicates_success(output: &str) -> bool {
    // 约定俗成的业务工具失败通常会返回 {"ok":false,...}，这里把它视为失败，
    // 这样同轮里依赖前一项成功的调用不会在业务失败后继续误执行。
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        .unwrap_or(true)
}

fn tool_execution_result(name: &str, output: &str, succeeded: bool) -> ToolExecutionResult {
    let output = serde_json::from_str::<Value>(output).unwrap_or_else(|_| json!(output));
    ToolExecutionResult {
        name: name.to_owned(),
        output,
        succeeded,
    }
}

fn required_string(item: &Value, key: &str, label: &str) -> Result<String, LlmError> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| LlmError::provider(format!("{label} missing `{key}`"), "provider"))
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
    use crate::provider::test_support::{WeatherToolStub, test_tool_context};
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockToolLoopState {
        bodies: Vec<Value>,
        requests: Vec<Value>,
    }

    async fn mock_tool_loop_handler(
        State(state): State<Arc<Mutex<MockToolLoopState>>>,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let mut state = state.lock().await;
        state.requests.push(request);
        let body = state.bodies.remove(0);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Json(body),
        )
    }

    async fn spawn_mock_tool_loop(bodies: Vec<Value>) -> (String, Arc<Mutex<MockToolLoopState>>) {
        let state = Arc::new(Mutex::new(MockToolLoopState {
            bodies,
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/chat/completions", post(mock_tool_loop_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    #[tokio::test]
    async fn tool_loop_executes_function_call_and_returns_output_to_model() {
        let (base_url, state) = spawn_mock_tool_loop(vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": r#"{"city":"杭州"}"#
                            }
                        }]
                    }
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "杭州今天小雨。"
                    }
                }],
                "usage": {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12}
            }),
        ])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let outcome = chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
            client: &client,
            provider: "deepseek",
            model: "deepseek-chat",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州天气怎么样")],
            context_budget: None,
            tools,
            tool_context: test_tool_context(),
            max_rounds: 2,
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "杭州今天小雨。");
        assert_eq!(outcome.executed_tools, vec!["get_weather"]);
        assert_eq!(outcome.usage.unwrap().total_tokens, Some(25));

        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], "auto");
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(requests[1]["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(requests[1]["messages"][2]["role"], "tool");
        assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn tool_loop_returns_limit_error_after_exceeding_max_rounds() {
        let (base_url, _state) = spawn_mock_tool_loop(vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": r#"{"city":"杭州"}"#
                            }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": r#"{"city":"杭州"}"#
                            }
                        }]
                    }
                }]
            }),
        ])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let err = chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
            client: &client,
            provider: "bigmodel",
            model: "glm-5.2",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州天气怎么样")],
            context_budget: None,
            tools,
            tool_context: test_tool_context(),
            max_rounds: 1,
        })
        .await
        .unwrap_err();

        assert_eq!(err.code, "tool_loop_limit");
        assert_eq!(err.stage, "tool_loop");
    }

    #[tokio::test]
    async fn tool_loop_budget_exceeded_before_first_provider_request() {
        let (base_url, state) = spawn_mock_tool_loop(vec![json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "should not be requested"
                }
            }]
        })])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let err = chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
            client: &client,
            provider: "deepseek",
            model: "deepseek-chat",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州天气怎么样")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 120,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools,
            tool_context: test_tool_context(),
            max_rounds: 2,
        })
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "tool_loop");
        assert!(state.lock().await.requests.is_empty());
    }

    #[tokio::test]
    async fn tool_loop_budget_exceeded_after_tool_result_skips_next_provider_request() {
        let (base_url, state) = spawn_mock_tool_loop(vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": r#"{"city":"杭州"}"#
                            }
                        }]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "should not be requested"
                    }
                }]
            }),
        ])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let err = chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
            client: &client,
            provider: "deepseek",
            model: "deepseek-chat",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州天气怎么样")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 500,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools,
            tool_context: test_tool_context(),
            max_rounds: 2,
        })
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "tool_loop");
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["tools"][0]["function"]["name"], "get_weather");
    }

    #[tokio::test]
    async fn tool_loop_budget_estimate_error_skips_provider_request() {
        let (base_url, state) = spawn_mock_tool_loop(vec![json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "should not be requested"
                }
            }]
        })])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let err = chat_completions_tool_loop(ChatCompletionsToolLoopRequest {
            client: &client,
            provider: "deepseek",
            model: "deepseek-chat",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("__force_json_estimate_error__")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 10_000,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools,
            tool_context: test_tool_context(),
            max_rounds: 2,
        })
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_estimate_error");
        assert_eq!(err.stage, "tool_loop");
        assert!(state.lock().await.requests.is_empty());
    }
}
