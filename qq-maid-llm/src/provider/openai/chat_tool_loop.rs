//! OpenAI 兼容 Chat Completions Tool Loop 的协议适配层。
//!
//! DeepSeek 和 BigModel 都通过 `/chat/completions` 暴露 `tools` / `tool_calls`
//! 协议，这里统一把一次模型请求转换为 [`AgentStep`]。轮次推进、最大轮数、
//! 工具执行和退出条件由 `qq_maid_llm::agent_loop::run_agent_loop` 统一控制；
//! 本模块不再维护自己的循环，避免 provider 侧重复维护同一套退出逻辑。

use serde_json::{Value, json};

use crate::{
    agent_loop::{
        AgentSessionRequest, AgentStep, AgentStepSession, AgentTextDeltaSink, AgentToolCall,
        AgentToolResult,
    },
    context_budget::{
        BudgetItemKind, ContextBudgetConfig, ensure_required_budget, estimated_json_chars,
        log_budget_report,
    },
    error::LlmError,
    metrics::MetricsRecorder,
    provider::types::{ChatMessage, TokenUsage},
    sse::{parse_sse_frame, take_sse_frame},
    tool::{ToolMetadata, ToolRegistry},
};

use super::chat::{
    ChatCompletionsClient, chat_completions_messages, extract_chat_completion_text,
    extract_chat_completion_usage, send_chat_completions_request,
};
use super::responses::{incomplete_stream_eof_error, stream_transport_error};

/// Chat Completions 协议的 Agent Loop 单步会话。
///
/// 持有 Chat Completions 形态的 `messages`（含历史、assistant `tool_calls` 与
/// `role:tool` 消息），每次 `advance` 做一次 `/chat/completions` 请求并把结果
/// 归一为 [`AgentStep`]。最大轮数与退出条件由 `run_agent_loop` 决定。
pub(crate) struct ChatCompletionsAgentSession {
    client: ChatCompletionsClient,
    provider: String,
    model: String,
    max_output_tokens: u64,
    messages: Vec<Value>,
    tool_defs: Vec<Value>,
    context_budget: Option<ContextBudgetConfig>,
}

impl ChatCompletionsAgentSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        client: ChatCompletionsClient,
        provider: &str,
        model: String,
        media_max_bytes: u64,
        max_output_tokens: u64,
        messages: &[ChatMessage],
        tools: &ToolRegistry,
        context_budget: Option<ContextBudgetConfig>,
    ) -> Result<Self, LlmError> {
        let messages = chat_completions_messages(messages, media_max_bytes)?;
        let tool_defs = chat_completions_tool_defs(tools.metadata());
        Ok(Self {
            client,
            provider: provider.to_owned(),
            model,
            max_output_tokens,
            messages,
            tool_defs,
            context_budget,
        })
    }
}

#[async_trait::async_trait]
impl AgentStepSession for ChatCompletionsAgentSession {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        // Chat Completions 不支持显式 tool_choice=none 的兼容交集，忽略
        // allow_tool_calls；最大轮数由 run_agent_loop 统一兜底。
        // 回填上一轮工具执行结果（首轮 results 为空，跳过）。
        append_tool_results(&mut self.messages, results);

        let payload = chat_completions_tool_loop_payload(
            &self.messages,
            &self.tool_defs,
            &self.model,
            self.max_output_tokens,
            false,
        );
        enforce_tool_loop_budget(self.context_budget, &payload)?;
        let response = send_chat_completions_request(&self.client, &payload, false).await?;
        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid Chat Completions tool loop JSON: {err}"),
                "json",
            )
        })?;
        let step_usage = extract_chat_completion_usage(&body);
        let tool_rounds = extract_tool_call_rounds(&body)?;
        if tool_rounds.is_empty() {
            let reply = extract_chat_completion_text(&body).ok_or_else(|| {
                LlmError::provider(
                    "Chat Completions tool loop returned empty final text output",
                    "provider",
                )
            })?;
            Ok(AgentStep::FinalAnswer {
                reply,
                usage: step_usage,
            })
        } else {
            // 把本轮所有 assistant tool_calls 批次回填到 messages，并收集全部
            // 待执行调用。工具结果在下一轮 advance 由 run_agent_loop 传入。
            let mut calls = Vec::new();
            for tool_round in tool_rounds {
                self.messages.push(tool_round.assistant_message);
                for call in tool_round.calls {
                    calls.push(AgentToolCall {
                        name: call.name,
                        call_id: call.call_id,
                        arguments: call.arguments,
                    });
                }
            }
            Ok(AgentStep::ToolCalls {
                calls,
                usage: step_usage,
            })
        }
    }

    async fn advance_streaming(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
        text_delta_sink: AgentTextDeltaSink,
    ) -> Result<Option<AgentStep>, LlmError> {
        let mut messages = self.messages.clone();
        append_tool_results(&mut messages, results);
        let payload = chat_completions_tool_loop_payload(
            &messages,
            &self.tool_defs,
            &self.model,
            self.max_output_tokens,
            true,
        );
        enforce_tool_loop_budget(self.context_budget, &payload)?;
        let response = send_chat_completions_request(&self.client, &payload, true).await?;
        let step = collect_chat_completions_tool_loop_stream(
            response,
            &mut messages,
            allow_tool_calls,
            text_delta_sink,
        )
        .await?;
        self.messages = messages;
        Ok(Some(step))
    }
}

/// 把“OpenAI 兼容 Chat Completions provider 的 Agent 会话接线”收敛成公共 helper。
///
/// DeepSeek / BigModel 的差异主要在模型前缀校验和默认 base URL，由 `resolve_model`
/// 闭合；会话构造本身完全一致，不值得各自复制一份。
pub(crate) async fn begin_chat_completions_session<F>(
    req: AgentSessionRequest<'_>,
    client: ChatCompletionsClient,
    provider: &str,
    default_model: &str,
    media_max_bytes: u64,
    max_output_tokens: u64,
    resolve_model: F,
) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError>
where
    F: FnOnce(Option<&str>, &str) -> Result<String, LlmError>,
{
    let effective_model = resolve_model(req.chat.model.as_deref(), default_model)?;
    Ok(Some(Box::new(ChatCompletionsAgentSession::new(
        client,
        provider,
        effective_model,
        media_max_bytes,
        max_output_tokens,
        &req.chat.messages,
        req.tools,
        req.chat.context_budget,
    )?)))
}

/// Chat Completions provider 的 `tool_calling_protocol` 公共实现。
///
/// 保留旧入口名以减小 DeepSeek / BigModel 改动面；内部只做模型解析 + 协议判定。
pub(crate) fn provider_chat_completions_tool_calling_protocol<F>(
    model: Option<&str>,
    default_model: &str,
    resolve_model: F,
) -> Option<crate::provider::ToolCallingProtocol>
where
    F: FnOnce(Option<&str>, &str) -> Result<String, LlmError>,
{
    resolve_model(model, default_model)
        .ok()
        .map(|_| crate::provider::ToolCallingProtocol::ChatCompletionsToolCalls)
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
    stream: bool,
) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
        "tools": tools,
        // BigModel 文档当前写明仅支持 auto，这里统一固定成兼容交集。
        "tool_choice": "auto",
        "stream": stream,
    })
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

#[derive(Debug, Clone, Default)]
struct StreamingFunctionCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

fn append_tool_results(messages: &mut Vec<Value>, results: &[AgentToolResult]) {
    for result in results {
        messages.push(json!({
            "role": "tool",
            "tool_call_id": result.call_id,
            "content": result.output,
        }));
    }
}

async fn collect_chat_completions_tool_loop_stream(
    mut response: reqwest::Response,
    messages: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
) -> Result<AgentStep, LlmError> {
    let mut frame_buffer = Vec::new();
    let mut recorder = MetricsRecorder::start();
    let mut answer = String::new();
    let mut final_message = String::new();
    let mut buffered_deltas = Vec::new();
    let mut usage = None;
    let mut finish_reason = None;
    let mut saw_done = false;
    let mut tool_calls: Vec<StreamingFunctionCall> = Vec::new();

    loop {
        while let Some(frame) = take_sse_frame(&mut frame_buffer) {
            let Some(event) = parse_sse_frame(&frame)? else {
                continue;
            };
            if event.data.trim() == "[DONE]" {
                saw_done = true;
                continue;
            }
            recorder.mark_event();
            let events = handle_chat_tool_loop_stream_event(
                &event.data,
                &mut recorder,
                &mut answer,
                &mut final_message,
                &mut usage,
                &mut tool_calls,
            )?;
            if let Some(reason) = events.finish_reason {
                finish_reason = Some(reason);
            }
            for delta in events.text_deltas {
                // Chat Completions 兼容交集无法可靠关闭 tool calls；即使
                // allow_tool_calls=false，也必须先缓存文本，确认本轮没有 tool call
                // 后再释放，避免协议异常时外显模型草稿。
                buffered_deltas.push(delta);
            }
        }

        match response.chunk().await {
            Ok(Some(chunk)) => frame_buffer.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(err) => {
                return Err(stream_transport_error(
                    format!("Chat Completions tool loop stream failed: {err}"),
                    &answer,
                ));
            }
        }
    }

    if !frame_buffer.is_empty() {
        let Some(event) = parse_sse_frame(&frame_buffer)? else {
            frame_buffer.clear();
            return finalize_chat_completions_tool_loop_stream(
                messages,
                allow_tool_calls,
                text_delta_sink,
                answer,
                final_message,
                buffered_deltas,
                usage,
                finish_reason,
                saw_done,
                tool_calls,
            )
            .await;
        };
        if event.data.trim() == "[DONE]" {
            saw_done = true;
        } else {
            recorder.mark_event();
            let events = handle_chat_tool_loop_stream_event(
                &event.data,
                &mut recorder,
                &mut answer,
                &mut final_message,
                &mut usage,
                &mut tool_calls,
            )?;
            if let Some(reason) = events.finish_reason {
                finish_reason = Some(reason);
            }
            for delta in events.text_deltas {
                buffered_deltas.push(delta);
            }
        }
    }

    finalize_chat_completions_tool_loop_stream(
        messages,
        allow_tool_calls,
        text_delta_sink,
        answer,
        final_message,
        buffered_deltas,
        usage,
        finish_reason,
        saw_done,
        tool_calls,
    )
    .await
}

struct ChatToolLoopStreamEvents {
    text_deltas: Vec<String>,
    finish_reason: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn handle_chat_tool_loop_stream_event(
    data: &str,
    recorder: &mut MetricsRecorder,
    answer: &mut String,
    final_message: &mut String,
    usage: &mut Option<TokenUsage>,
    tool_calls: &mut Vec<StreamingFunctionCall>,
) -> Result<ChatToolLoopStreamEvents, LlmError> {
    let value = serde_json::from_str::<Value>(data).map_err(|err| {
        LlmError::provider(
            format!("invalid Chat Completions tool loop stream JSON: {err}"),
            "sse",
        )
    })?;
    if let Some(event_usage) = extract_chat_completion_usage(&value) {
        *usage = Some(event_usage);
    }
    let mut text_deltas = Vec::new();
    let mut finish_reason = None;
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return Ok(ChatToolLoopStreamEvents {
            text_deltas,
            finish_reason,
        });
    };
    for choice in choices {
        if let Some(delta_value) = choice.get("delta") {
            if let Some(content) = delta_value.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                recorder.mark_token();
                answer.push_str(content);
                text_deltas.push(content.to_owned());
            }
            if let Some(delta_tool_calls) = delta_value.get("tool_calls").and_then(Value::as_array)
            {
                merge_streaming_tool_calls(tool_calls, delta_tool_calls)?;
            }
        }
        if let Some(message_value) = choice.get("message") {
            if let Some(content) = message_value.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                final_message.push_str(content);
            }
            if let Some(message_tool_calls) =
                message_value.get("tool_calls").and_then(Value::as_array)
            {
                merge_streaming_tool_calls(tool_calls, message_tool_calls)?;
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
            && !reason.trim().is_empty()
        {
            finish_reason = Some(reason.to_owned());
        }
    }
    Ok(ChatToolLoopStreamEvents {
        text_deltas,
        finish_reason,
    })
}

fn merge_streaming_tool_calls(
    tool_calls: &mut Vec<StreamingFunctionCall>,
    delta_tool_calls: &[Value],
) -> Result<(), LlmError> {
    for item in delta_tool_calls {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(tool_calls.len());
        if tool_calls.len() <= index {
            tool_calls.resize_with(index + 1, StreamingFunctionCall::default);
        }
        let call = &mut tool_calls[index];
        if let Some(id) = item.get("id").and_then(Value::as_str)
            && !id.trim().is_empty()
        {
            call.id = Some(id.to_owned());
        }
        if let Some(function) = item.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str)
                && !name.is_empty()
            {
                call.name.push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                call.arguments.push_str(arguments);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finalize_chat_completions_tool_loop_stream(
    messages: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    mut answer: String,
    final_message: String,
    buffered_deltas: Vec<String>,
    usage: Option<TokenUsage>,
    finish_reason: Option<String>,
    saw_done: bool,
    tool_calls: Vec<StreamingFunctionCall>,
) -> Result<AgentStep, LlmError> {
    let calls = streaming_tool_calls_to_function_calls(tool_calls)?;
    if !calls.is_empty() {
        if !allow_tool_calls {
            return Err(LlmError::new(
                "tool_loop_limit",
                "tool loop returned tool calls when tool calls are disabled",
                "tool_loop",
            ));
        }
        let assistant_message = streaming_assistant_message(&calls);
        messages.push(assistant_message);
        return Ok(AgentStep::ToolCalls {
            calls: calls
                .into_iter()
                .map(|call| AgentToolCall {
                    name: call.name,
                    call_id: call.call_id,
                    arguments: call.arguments,
                })
                .collect(),
            usage,
        });
    }
    if answer.trim().is_empty() && !final_message.trim().is_empty() {
        answer = final_message;
    }
    if !saw_done && finish_reason.is_none() {
        return Err(incomplete_stream_eof_error(
            "Chat Completions tool loop stream ended before [DONE] or finish_reason",
            &answer,
        ));
    }
    if answer.trim().is_empty() {
        return Err(LlmError::provider(
            "Chat Completions tool loop returned empty final text output",
            "provider",
        ));
    }
    if buffered_deltas.is_empty() {
        text_delta_sink(answer.clone()).await?;
    } else {
        for delta in buffered_deltas {
            text_delta_sink(delta).await?;
        }
    }
    Ok(AgentStep::FinalAnswer {
        reply: answer,
        usage,
    })
}

fn streaming_tool_calls_to_function_calls(
    tool_calls: Vec<StreamingFunctionCall>,
) -> Result<Vec<FunctionCall>, LlmError> {
    let mut calls = Vec::new();
    for call in tool_calls {
        if call.name.trim().is_empty() && call.arguments.trim().is_empty() && call.id.is_none() {
            continue;
        }
        let call_id = call.id.ok_or_else(|| {
            LlmError::provider(
                "Chat Completions tool loop stream returned tool call without id",
                "provider",
            )
        })?;
        if call.name.trim().is_empty() {
            return Err(LlmError::provider(
                "Chat Completions tool loop stream returned tool call without function name",
                "provider",
            ));
        }
        calls.push(FunctionCall {
            name: call.name,
            call_id,
            arguments: call.arguments,
        });
    }
    Ok(calls)
}

fn streaming_assistant_message(calls: &[FunctionCall]) -> Value {
    json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": calls.iter().map(|call| json!({
            "id": call.call_id,
            "type": "function",
            "function": {
                "name": call.name,
                "arguments": call.arguments,
            },
        })).collect::<Vec<_>>(),
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

fn required_string(item: &Value, key: &str, label: &str) -> Result<String, LlmError> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| LlmError::provider(format!("{label} missing `{key}`"), "provider"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::{AgentTextDeltaFuture, run_agent_loop};
    use crate::provider::test_support::{WeatherToolStub, test_tool_context};
    use crate::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};
    use async_trait::async_trait;
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::{net::TcpListener, sync::Mutex};

    fn recording_delta_sink(deltas: Arc<StdMutex<Vec<String>>>) -> AgentTextDeltaSink {
        Arc::new(move |delta| {
            let deltas = deltas.clone();
            Box::pin(async move {
                deltas.lock().unwrap().push(delta);
                Ok(())
            }) as AgentTextDeltaFuture
        })
    }

    #[tokio::test]
    async fn streaming_tool_call_does_not_release_buffered_text_delta() {
        let mut messages = Vec::new();
        let deltas = Arc::new(StdMutex::new(Vec::new()));
        let step = finalize_chat_completions_tool_loop_stream(
            &mut messages,
            true,
            recording_delta_sink(deltas.clone()),
            "草稿".to_owned(),
            String::new(),
            vec!["草稿".to_owned()],
            None,
            Some("tool_calls".to_owned()),
            true,
            vec![StreamingFunctionCall {
                id: Some("call_1".to_owned()),
                name: "get_weather".to_owned(),
                arguments: "{\"city\":\"杭州\"}".to_owned(),
            }],
        )
        .await
        .unwrap();

        let AgentStep::ToolCalls { calls, .. } = step else {
            panic!("expected tool calls");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert!(deltas.lock().unwrap().is_empty());
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
    }

    struct PrepareOrderToolStub {
        name: &'static str,
        sequence: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for PrepareOrderToolStub {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: self.name.to_owned(),
                description: "records prepare and execute order".to_owned(),
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
            self.sequence
                .lock()
                .unwrap()
                .push(format!("prepare:{}", self.name));
            Ok(crate::tool::ToolPreparation::ready(arguments))
        }

        async fn execute(
            &self,
            _context: ToolContext,
            arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            self.sequence
                .lock()
                .unwrap()
                .push(format!("execute:{}", self.name));
            Ok(ToolOutput::json(json!({
                "ok": true,
                "value": arguments["value"],
            })))
        }
    }

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

    #[allow(clippy::too_many_arguments)]
    fn run_session(
        client: ChatCompletionsClient,
        provider: &'static str,
        model: &str,
        max_output_tokens: u64,
        messages: &[ChatMessage],
        tools: ToolRegistry,
        context_budget: Option<ContextBudgetConfig>,
        max_rounds: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<crate::provider::ChatOutcome, LlmError>> + Send,
        >,
    > {
        let tool_context = test_tool_context();
        let session = ChatCompletionsAgentSession::new(
            client,
            provider,
            model.to_owned(),
            10 * 1024 * 1024,
            max_output_tokens,
            messages,
            &tools,
            context_budget,
        )
        .unwrap();
        Box::pin(async move {
            run_agent_loop(
                Box::new(session),
                tools,
                tool_context,
                max_rounds,
                None,
                None,
            )
            .await
        })
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

        let outcome = run_session(
            client,
            "deepseek",
            "deepseek-chat",
            1200,
            &[ChatMessage::user("杭州天气怎么样")],
            tools,
            None,
            2,
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "杭州今天小雨。");
        assert_eq!(outcome.agent.executed_tools, vec!["get_weather"]);
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

        let err = run_session(
            client,
            "bigmodel",
            "glm-5.2",
            1200,
            &[ChatMessage::user("杭州天气怎么样")],
            tools,
            None,
            1,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "tool_loop_limit");
        assert_eq!(err.stage, "tool_loop");
    }

    #[tokio::test]
    async fn tool_loop_prepares_same_round_calls_before_executing_any_tool() {
        let (base_url, _state) = spawn_mock_tool_loop(vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_first_order",
                                "type": "function",
                                "function": {
                                    "name": "first_order_tool",
                                    "arguments": r#"{"value":"first"}"#
                                }
                            },
                            {
                                "id": "call_second_order",
                                "type": "function",
                                "function": {
                                    "name": "second_order_tool",
                                    "arguments": r#"{"value":"second"}"#
                                }
                            }
                        ]
                    }
                }]
            }),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "顺序已记录。"
                    }
                }]
            }),
        ])
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());
        let sequence = Arc::new(StdMutex::new(Vec::new()));
        let mut tools = ToolRegistry::new();
        tools
            .insert(Arc::new(PrepareOrderToolStub {
                name: "first_order_tool",
                sequence: sequence.clone(),
            }))
            .unwrap();
        tools
            .insert(Arc::new(PrepareOrderToolStub {
                name: "second_order_tool",
                sequence: sequence.clone(),
            }))
            .unwrap();

        let outcome = run_session(
            client,
            "deepseek",
            "deepseek-chat",
            1200,
            &[ChatMessage::user("同轮调用两个工具")],
            tools,
            None,
            2,
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "顺序已记录。");
        assert_eq!(
            *sequence.lock().unwrap(),
            vec![
                "prepare:first_order_tool",
                "prepare:second_order_tool",
                "execute:first_order_tool",
                "execute:second_order_tool",
            ]
        );
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

        let err = run_session(
            client,
            "deepseek",
            "deepseek-chat",
            1200,
            &[ChatMessage::user("杭州天气怎么样")],
            tools,
            Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 120,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            2,
        )
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

        let err = run_session(
            client,
            "deepseek",
            "deepseek-chat",
            1200,
            &[ChatMessage::user("杭州天气怎么样")],
            tools,
            Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 500,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            2,
        )
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

        let err = run_session(
            client,
            "deepseek",
            "deepseek-chat",
            1200,
            &[ChatMessage::user("__force_json_estimate_error__")],
            tools,
            Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 10_000,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            2,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_estimate_error");
        assert_eq!(err.stage, "tool_loop");
        assert!(state.lock().await.requests.is_empty());
    }
}
