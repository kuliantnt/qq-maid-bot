//! OpenAI Responses 原生 Function Tool Loop 的协议适配层。
//!
//! 本模块只处理 Responses 协议层的 function call / function_call_output 往返，
//! 把一次模型请求转换为统一 [`AgentStep`]。轮次推进、最大轮数、工具执行和
//! 退出条件由 `qq_maid_llm::agent_loop::run_agent_loop` 统一控制；本模块不再
//! 维护自己的循环。具体业务能力由上层 crate 通过 `ToolRegistry` 注册，
//! 避免 LLM crate 反向依赖 Core。

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};

use crate::{
    agent_loop::{
        AgentStep, AgentStepSession, AgentStreamingDiagnostics, AgentTextDeltaSink, AgentToolCall,
        AgentToolResult,
    },
    context_budget::{
        BudgetItemKind, ContextBudgetConfig, ensure_required_budget, estimated_json_chars,
        log_budget_report,
    },
    error::LlmError,
    metrics::MetricsRecorder,
    provider::types::{ChatMessage, ReasoningEffort},
    sse::{SseFrame, parse_sse_frame, take_sse_frame},
    tool::{ToolMetadata, ToolRegistry},
};

use super::{
    extract::{extract_response_output_text, extract_response_usage},
    payload::{openai_model_supports_reasoning, openai_responses_message},
    responses::{incomplete_stream_eof_error, stream_transport_error},
    stream::{
        handle_openai_chat_stream_event, is_openai_responses_done_sentinel,
        responses_stream_is_complete,
    },
    transport::send_openai_responses_request,
};

/// OpenAI Responses 协议的 Agent Loop 单步会话。
///
/// 持有 Responses 形态的 `input`（含历史消息、`function_call` 与
/// `function_call_output` 条目），每次 `advance` 做一次 `/v1/responses` 请求
/// 并把结果归一为 [`AgentStep`]。最大轮数与退出条件由 `run_agent_loop` 决定。
pub(crate) struct ResponsesAgentSession {
    client: reqwest::Client,
    api_key: String,
    base_url: Option<String>,
    provider: String,
    model: String,
    max_output_tokens: u64,
    reasoning_effort: Option<ReasoningEffort>,
    input: Vec<Value>,
    tool_defs: Vec<Value>,
    context_budget: Option<ContextBudgetConfig>,
    streaming_diagnostics: Arc<Mutex<AgentStreamingDiagnostics>>,
    streaming_activity_counter: Arc<AtomicUsize>,
}

impl ResponsesAgentSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        client: reqwest::Client,
        api_key: String,
        base_url: Option<String>,
        provider: &str,
        model: String,
        media_max_bytes: u64,
        max_output_tokens: u64,
        reasoning_effort: Option<ReasoningEffort>,
        messages: &[ChatMessage],
        tools: &ToolRegistry,
        context_budget: Option<ContextBudgetConfig>,
    ) -> Result<Self, LlmError> {
        let input = openai_tool_loop_input(messages, media_max_bytes)?;
        let tool_defs = openai_tool_defs(tools.metadata());
        Ok(Self {
            client,
            api_key,
            base_url,
            provider: provider.to_owned(),
            model,
            max_output_tokens,
            reasoning_effort,
            input,
            tool_defs,
            context_budget,
            streaming_diagnostics: Arc::new(Mutex::new(AgentStreamingDiagnostics::default())),
            streaming_activity_counter: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[async_trait::async_trait]
impl AgentStepSession for ResponsesAgentSession {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn streaming_diagnostics(&self) -> AgentStreamingDiagnostics {
        self.streaming_diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn streaming_activity_counter(&self) -> Option<Arc<AtomicUsize>> {
        Some(self.streaming_activity_counter.clone())
    }

    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        // 回填上一轮工具执行结果（首轮 results 为空，跳过）。
        append_tool_results(&mut self.input, results);

        let payload = openai_tool_loop_payload(
            &self.input,
            &self.tool_defs,
            &self.model,
            self.max_output_tokens,
            self.reasoning_effort,
            allow_tool_calls,
            false,
        );
        enforce_tool_loop_budget(self.context_budget, &payload)?;
        let response = send_openai_responses_request(
            &self.client,
            &self.api_key,
            self.base_url.as_deref(),
            &payload,
            false,
        )
        .await?;
        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid OpenAI tool loop JSON: {err}"), "json")
        })?;
        let step_usage = extract_response_usage(&body);
        let calls = extract_function_calls(&body)?;
        if calls.is_empty() {
            let reply = extract_response_output_text(&body).ok_or_else(|| {
                LlmError::provider(
                    "OpenAI tool loop returned empty final text output",
                    "provider",
                )
            })?;
            Ok(AgentStep::FinalAnswer {
                reply,
                usage: step_usage,
            })
        } else {
            // 把本轮模型输出的原始 items 回填到 input，供下一轮请求使用；
            // 保留 reasoning 等非 function_call 条目，与改造前行为一致。
            append_response_output_items(&mut self.input, &body)?;
            Ok(AgentStep::ToolCalls {
                calls: calls
                    .into_iter()
                    .map(|call| AgentToolCall {
                        name: call.name,
                        call_id: call.call_id,
                        arguments: call.arguments,
                    })
                    .collect(),
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
        replace_streaming_diagnostics(
            &self.streaming_diagnostics,
            AgentStreamingDiagnostics::default(),
        );
        self.streaming_activity_counter.store(0, Ordering::SeqCst);
        let mut input = self.input.clone();
        append_tool_results(&mut input, results);
        let payload = openai_tool_loop_payload(
            &input,
            &self.tool_defs,
            &self.model,
            self.max_output_tokens,
            self.reasoning_effort,
            allow_tool_calls,
            true,
        );
        enforce_tool_loop_budget(self.context_budget, &payload)?;
        let response = send_openai_responses_request(
            &self.client,
            &self.api_key,
            self.base_url.as_deref(),
            &payload,
            true,
        )
        .await?;
        let step = collect_responses_tool_loop_stream(
            response,
            &mut input,
            allow_tool_calls,
            text_delta_sink,
            self.streaming_diagnostics.clone(),
            self.streaming_activity_counter.clone(),
        )
        .await;
        if let Err(err) = &step {
            classify_responses_stream_failure(&self.streaming_diagnostics, err);
        }
        let step = step?;
        self.input = input;
        Ok(Some(step))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionCall {
    name: String,
    call_id: String,
    arguments: String,
}

fn append_tool_results(input: &mut Vec<Value>, results: &[AgentToolResult]) {
    for result in results {
        input.push(json!({
            "type": "function_call_output",
            "call_id": result.call_id,
            "output": result.output,
        }));
    }
}

async fn collect_responses_tool_loop_stream(
    mut response: reqwest::Response,
    input: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    diagnostics: Arc<Mutex<AgentStreamingDiagnostics>>,
    activity_counter: Arc<AtomicUsize>,
) -> Result<AgentStep, LlmError> {
    let mut frame_buffer = Vec::new();
    let mut recorder = MetricsRecorder::start();
    let mut answer = String::new();
    let mut buffered_deltas = Vec::new();
    let mut completed_response = None;
    let mut saw_completed = false;
    let mut active_function_calls = HashSet::new();
    let mut completed_output_items = Vec::new();
    loop {
        while let Some(frame) = take_sse_frame(&mut frame_buffer) {
            let Some(event) = parse_sse_frame(&frame).map_err(|err| {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                err
            })?
            else {
                continue;
            };
            update_streaming_diagnostics(&diagnostics, |item| item.sse_event_count += 1);
            activity_counter.fetch_add(1, Ordering::SeqCst);
            if is_openai_responses_done_sentinel(&event.data) {
                update_streaming_diagnostics(&diagnostics, |item| item.saw_done = true);
                if responses_stream_is_complete(saw_completed, &completed_response) {
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        allow_tool_calls,
                        text_delta_sink,
                        answer,
                        buffered_deltas,
                        completed_response,
                        saw_completed,
                    )
                    .await;
                }
                if active_function_calls.is_empty()
                    && (!completed_output_items.is_empty() || !answer.trim().is_empty())
                {
                    completed_response = Some(json!({
                        "output_text": answer.clone(),
                        "output": completed_output_items.clone(),
                    }));
                    saw_completed = true;
                    sync_responses_stream_diagnostics(
                        &diagnostics,
                        saw_completed,
                        buffered_deltas.len(),
                        active_function_calls.len(),
                    );
                    return finalize_responses_tool_loop_stream(
                        input,
                        allow_tool_calls,
                        text_delta_sink,
                        answer,
                        buffered_deltas,
                        completed_response,
                        saw_completed,
                    )
                    .await;
                }
                continue;
            }
            observe_responses_function_call_event(
                &event,
                &mut active_function_calls,
                &mut completed_output_items,
            )
            .map_err(|err| {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                err
            })?;
            recorder.mark_event();
            match handle_openai_chat_stream_event(
                event,
                &mut recorder,
                &mut answer,
                &mut completed_response,
                &mut saw_completed,
            )
            .map_err(|err| {
                if err.stage == "sse" && err.message.starts_with("invalid ") {
                    set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                }
                err
            })? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => text_delta_sink(delta).await?,
                None => {}
            }
            sync_responses_stream_diagnostics(
                &diagnostics,
                saw_completed,
                buffered_deltas.len(),
                active_function_calls.len(),
            );
            if responses_stream_is_complete(saw_completed, &completed_response) {
                return finalize_responses_tool_loop_stream(
                    input,
                    allow_tool_calls,
                    text_delta_sink,
                    answer,
                    buffered_deltas,
                    completed_response,
                    saw_completed,
                )
                .await;
            }
        }

        match response.chunk().await {
            Ok(Some(chunk)) => {
                update_streaming_diagnostics(&diagnostics, |item| item.chunk_count += 1);
                frame_buffer.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => {
                set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
                return Err(stream_transport_error(
                    format!("OpenAI tool loop stream failed: {err}"),
                    &answer,
                ));
            }
        }
    }

    if !frame_buffer.is_empty() {
        let Some(event) = parse_sse_frame(&frame_buffer).map_err(|err| {
            set_streaming_fallback_reason(&diagnostics, "http_sse_parse_error");
            err
        })?
        else {
            frame_buffer.clear();
            return finalize_responses_tool_loop_stream(
                input,
                allow_tool_calls,
                text_delta_sink,
                answer,
                buffered_deltas,
                completed_response,
                saw_completed,
            )
            .await;
        };
        update_streaming_diagnostics(&diagnostics, |item| item.sse_event_count += 1);
        activity_counter.fetch_add(1, Ordering::SeqCst);
        if is_openai_responses_done_sentinel(&event.data) {
            update_streaming_diagnostics(&diagnostics, |item| item.saw_done = true);
        }
        if !is_openai_responses_done_sentinel(&event.data) {
            recorder.mark_event();
            match handle_openai_chat_stream_event(
                event,
                &mut recorder,
                &mut answer,
                &mut completed_response,
                &mut saw_completed,
            )? {
                Some(delta) if allow_tool_calls => buffered_deltas.push(delta),
                Some(delta) => text_delta_sink(delta).await?,
                None => {}
            }
        }
    }

    sync_responses_stream_diagnostics(
        &diagnostics,
        saw_completed,
        buffered_deltas.len(),
        active_function_calls.len(),
    );

    finalize_responses_tool_loop_stream(
        input,
        allow_tool_calls,
        text_delta_sink,
        answer,
        buffered_deltas,
        completed_response,
        saw_completed,
    )
    .await
}

fn update_streaming_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    update: impl FnOnce(&mut AgentStreamingDiagnostics),
) {
    let mut diagnostics = diagnostics
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    update(&mut diagnostics);
}

fn replace_streaming_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    replacement: AgentStreamingDiagnostics,
) {
    update_streaming_diagnostics(diagnostics, |item| *item = replacement);
}

fn set_streaming_fallback_reason(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    fallback_reason: &str,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        if item.fallback_reason.is_none() {
            item.fallback_reason = Some(fallback_reason.to_owned());
        }
    });
}

fn sync_responses_stream_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    saw_completed: bool,
    buffered_delta_count: usize,
    active_function_call_count: usize,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        item.saw_completed = saw_completed;
        item.buffered_delta_count = buffered_delta_count;
        item.active_function_call_count = active_function_call_count;
    });
}

fn classify_responses_stream_failure(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    err: &LlmError,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        if item.fallback_reason.is_some() {
            return;
        }
        let reason = if item.saw_completed {
            "completed_response_incomplete"
        } else if item.saw_done {
            "done_without_safe_completion"
        } else if err.message.contains("before response.completed") {
            if item.sse_event_count == 0 {
                "sse_early_eof"
            } else {
                "missing_response_completed"
            }
        } else if err.code == "http_error" || err.stage == "http" {
            "http_sse_parse_error"
        } else {
            "provider_error_other"
        };
        item.fallback_reason = Some(reason.to_owned());
    });
}

fn observe_responses_function_call_event(
    event: &SseFrame,
    active_function_calls: &mut HashSet<u64>,
    completed_output_items: &mut Vec<Value>,
) -> Result<(), LlmError> {
    let value = serde_json::from_str::<Value>(&event.data).map_err(|err| {
        LlmError::provider(
            format!("invalid OpenAI tool loop stream JSON: {err}"),
            "sse",
        )
    })?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    let output_index = value.get("output_index").and_then(Value::as_u64);
    match event_type {
        "response.output_item.added" => {
            if value
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(Value::as_str)
                == Some("function_call")
                && let Some(index) = output_index
            {
                active_function_calls.insert(index);
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(index) = output_index {
                active_function_calls.insert(index);
            }
        }
        "response.output_item.done" => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                completed_output_items.push(item.clone());
                if let Some(index) = output_index {
                    active_function_calls.remove(&index);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

async fn finalize_responses_tool_loop_stream(
    input: &mut Vec<Value>,
    allow_tool_calls: bool,
    text_delta_sink: AgentTextDeltaSink,
    mut answer: String,
    buffered_deltas: Vec<String>,
    completed_response: Option<Value>,
    saw_completed: bool,
) -> Result<AgentStep, LlmError> {
    if !saw_completed {
        return Err(incomplete_stream_eof_error(
            "OpenAI Responses tool loop stream ended before response.completed",
            &answer,
        ));
    }
    let body = completed_response.ok_or_else(|| {
        LlmError::provider(
            "OpenAI Responses tool loop stream completed without response body",
            "sse",
        )
    })?;
    let step_usage = extract_response_usage(&body);
    let calls = extract_function_calls(&body)?;
    if !calls.is_empty() {
        if !allow_tool_calls {
            return Err(LlmError::new(
                "tool_loop_limit",
                "tool loop returned tool calls when tool calls are disabled",
                "tool_loop",
            ));
        }
        append_response_output_items(input, &body)?;
        return Ok(AgentStep::ToolCalls {
            calls: calls
                .into_iter()
                .map(|call| AgentToolCall {
                    name: call.name,
                    call_id: call.call_id,
                    arguments: call.arguments,
                })
                .collect(),
            usage: step_usage,
        });
    }

    if answer.trim().is_empty()
        && let Some(completed_answer) = extract_response_output_text(&body)
        && !completed_answer.trim().is_empty()
    {
        answer = completed_answer;
    }
    if answer.trim().is_empty() {
        return Err(LlmError::provider(
            "OpenAI tool loop returned empty final text output",
            "provider",
        ));
    }
    if allow_tool_calls {
        if buffered_deltas.is_empty() {
            text_delta_sink(answer.clone()).await?;
        } else {
            for delta in buffered_deltas {
                text_delta_sink(delta).await?;
            }
        }
    }
    Ok(AgentStep::FinalAnswer {
        reply: answer,
        usage: step_usage,
    })
}

fn enforce_tool_loop_budget(
    context_budget: Option<ContextBudgetConfig>,
    payload: &Value,
) -> Result<(), LlmError> {
    let Some(config) = context_budget else {
        return Ok(());
    };
    // Responses Tool Loop 首期不拆分、不淘汰已进入循环的结构化轮次；
    // 工具结果增长依靠单项结果上限和 max_rounds 控制，超预算时显式失败。
    let report = ensure_required_budget(
        config,
        BudgetItemKind::ToolLoopAtomicTurn,
        estimated_json_chars(payload, "tool_loop")?,
        "tool_loop",
    )?;
    log_budget_report("responses_tool_loop", &report);
    Ok(())
}

fn openai_tool_loop_input(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::{AgentTextDeltaFuture, run_agent_loop};
    use crate::tool::{Tool, ToolCallDependency, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use axum::{
        Json, Router,
        body::{Body, Bytes},
        extract::State,
        http::{Response, header},
        routing::post,
    };
    use futures::{StreamExt, stream};
    use serde_json::json;
    use std::{
        convert::Infallible,
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
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
        let mut input = Vec::new();
        let deltas = Arc::new(StdMutex::new(Vec::new()));
        let step = finalize_responses_tool_loop_stream(
            &mut input,
            true,
            recording_delta_sink(deltas.clone()),
            "草稿".to_owned(),
            vec!["草稿".to_owned()],
            Some(json!({
                "output": [{
                    "type": "function_call",
                    "name": "get_weather",
                    "call_id": "call_weather_1",
                    "arguments": "{\"city\":\"杭州\"}"
                }]
            })),
            true,
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
            group_member_role: None,
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

    struct PrepareFailToolStub;

    #[async_trait]
    impl Tool for PrepareFailToolStub {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "prepare_fail_tool".to_owned(),
                description: "prepare failure test".to_owned(),
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
            _arguments: Value,
        ) -> Result<crate::tool::ToolPreparation, LlmError> {
            Err(LlmError::new(
                "bad_tool_arguments",
                "prepare failed",
                "tool",
            ))
        }

        async fn execute(
            &self,
            _context: ToolContext,
            _arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            panic!("prepare failure tool should never execute");
        }
    }

    struct SoftFailToolStub {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SoftFailToolStub {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "soft_fail_tool".to_owned(),
                description: "returns structured failure".to_owned(),
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

        async fn execute(
            &self,
            _context: ToolContext,
            arguments: Value,
        ) -> Result<ToolOutput, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::json(json!({
                "ok": false,
                "error_code": "soft_failure",
                "value": arguments["value"],
            })))
        }
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

    async fn mock_prepare_failure_handler(
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
                        "name": "prepare_fail_tool",
                        "call_id": "call_prepare_fail_1",
                        "arguments": "{\"value\":\"bad\"}"
                    },
                    {
                        "type": "function_call",
                        "name": "get_weather",
                        "call_id": "call_weather_2",
                        "arguments": "{\"city\":\"杭州\"}"
                    }
                ]
            }));
        }
        Json(json!({
            "output_text": "准备失败已汇总。",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "准备失败已汇总。"}]
            }]
        }))
    }

    async fn mock_soft_failure_handler(
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
                        "name": "soft_fail_tool",
                        "call_id": "call_soft_fail_1",
                        "arguments": "{\"value\":\"first\"}"
                    },
                    {
                        "type": "function_call",
                        "name": "ok_tool",
                        "call_id": "call_ok_2",
                        "arguments": "{\"value\":\"second\"}"
                    }
                ]
            }));
        }
        Json(json!({
            "output_text": "业务失败已汇总。",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "业务失败已汇总。"}]
            }]
        }))
    }

    async fn mock_prepare_order_handler(
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
                        "name": "first_order_tool",
                        "call_id": "call_first_order",
                        "arguments": "{\"value\":\"first\"}"
                    },
                    {
                        "type": "function_call",
                        "name": "second_order_tool",
                        "call_id": "call_second_order",
                        "arguments": "{\"value\":\"second\"}"
                    }
                ]
            }));
        }
        Json(json!({
            "output_text": "顺序已记录。",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "顺序已记录。"}]
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

    async fn completed_stream_that_never_closes() -> Response<Body> {
        let completed = Bytes::from_static(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"direct answer\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"direct answer\"}]}]}}\n\n",
        );
        let body = Body::from_stream(
            stream::once(async move { Ok::<Bytes, Infallible>(completed) })
                .chain(stream::pending()),
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap()
    }

    async fn done_stream_that_never_closes() -> Response<Body> {
        let frames = Bytes::from_static(
            b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"done answer\"}\n\ndata: [DONE]\n\n",
        );
        let body = Body::from_stream(
            stream::once(async move { Ok::<Bytes, Infallible>(frames) }).chain(stream::pending()),
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap()
    }

    async fn spawn_never_closing_completed_stream() -> String {
        let app = Router::new().route("/v1/responses", post(completed_stream_that_never_closes));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    async fn spawn_never_closing_done_stream() -> String {
        let app = Router::new().route("/v1/responses", post(done_stream_that_never_closes));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
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

    async fn spawn_prepare_failure_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
        let state = Arc::new(Mutex::new(ToolLoopMockState {
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_prepare_failure_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    async fn spawn_soft_failure_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
        let state = Arc::new(Mutex::new(ToolLoopMockState {
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_soft_failure_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    async fn spawn_prepare_order_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
        let state = Arc::new(Mutex::new(ToolLoopMockState {
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_prepare_order_handler))
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
            None,
            true,
            false,
        );

        assert_eq!(payload["parallel_tool_calls"], false);
        assert!(payload.get("tool_choice").is_none());
        assert!(payload.get("stream").is_none());
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

    #[tokio::test]
    async fn tool_loop_executes_function_call_and_returns_output_to_model() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let outcome = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("杭州今天需要带伞吗？")],
                    &registry,
                    None,
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
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
    async fn agent_stream_finishes_on_completed_without_waiting_for_http_eof() {
        let base_url = spawn_never_closing_completed_stream().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let mut session = ResponsesAgentSession::new(
            reqwest::Client::new(),
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
        assert!(!diagnostics.saw_done);
    }

    #[tokio::test]
    async fn agent_stream_finishes_on_done_without_waiting_for_http_eof() {
        let base_url = spawn_never_closing_done_stream().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let mut session = ResponsesAgentSession::new(
            reqwest::Client::new(),
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
        assert!(diagnostics.saw_completed);
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

        assert_eq!(active, HashSet::from([0]));
        assert!(completed.is_empty());
    }

    #[tokio::test]
    async fn tool_loop_budget_exceeded_before_first_provider_request() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let err = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("杭州今天需要带伞吗？")],
                    &registry,
                    Some(crate::context_budget::ContextBudgetConfig {
                        context_window_chars: 120,
                        output_reserve_chars: 20,
                        protected_recent_turns: 0,
                    }),
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "tool_loop");
        assert!(state.lock().await.requests.is_empty());
    }

    #[tokio::test]
    async fn tool_loop_budget_exceeded_after_tool_result_skips_next_provider_request() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let err = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("杭州今天需要带伞吗？")],
                    &registry,
                    Some(crate::context_budget::ContextBudgetConfig {
                        context_window_chars: 420,
                        output_reserve_chars: 20,
                        protected_recent_turns: 0,
                    }),
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_exceeded");
        assert_eq!(err.stage, "tool_loop");
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["tools"][0]["name"], "get_weather");
    }

    #[tokio::test]
    async fn tool_loop_budget_estimate_error_skips_provider_request() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let err = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("__force_json_estimate_error__")],
                    &registry,
                    Some(crate::context_budget::ContextBudgetConfig {
                        context_window_chars: 10_000,
                        output_reserve_chars: 20,
                        protected_recent_turns: 0,
                    }),
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "context_budget_estimate_error");
        assert_eq!(err.stage, "tool_loop");
        assert!(state.lock().await.requests.is_empty());
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

        let outcome = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("连续执行两个工具")],
                    &registry,
                    None,
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
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

    #[tokio::test]
    async fn tool_loop_prepares_same_round_calls_before_executing_any_tool() {
        let (base_url, _state) = spawn_prepare_order_mock().await;
        let sequence = Arc::new(StdMutex::new(Vec::new()));
        let mut registry = ToolRegistry::new();
        registry
            .insert(Arc::new(PrepareOrderToolStub {
                name: "first_order_tool",
                sequence: sequence.clone(),
            }))
            .unwrap();
        registry
            .insert(Arc::new(PrepareOrderToolStub {
                name: "second_order_tool",
                sequence: sequence.clone(),
            }))
            .unwrap();
        let client = reqwest::Client::new();

        let outcome = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("同轮调用两个工具")],
                    &registry,
                    None,
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
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
    async fn tool_loop_keeps_independent_calls_after_prepare_failure() {
        let (base_url, state) = spawn_prepare_failure_mock().await;
        let registry = ToolRegistry::new()
            .register(PrepareFailToolStub)
            .unwrap()
            .register(WeatherToolStub)
            .unwrap();
        let client = reqwest::Client::new();

        let outcome = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("先失败再查天气")],
                    &registry,
                    None,
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "准备失败已汇总。");
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        let second_input = state.requests[1]["input"].as_array().unwrap();
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_prepare_fail_1"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"prepare failed\""))
        }));
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_weather_2"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"weather\":\"小雨\""))
        }));
    }

    #[tokio::test]
    async fn tool_loop_skips_dependent_call_after_structured_tool_failure() {
        let (base_url, state) = spawn_soft_failure_mock().await;
        let soft_fail_calls = Arc::new(AtomicUsize::new(0));
        let ok_calls = Arc::new(AtomicUsize::new(0));
        let registry = ToolRegistry::new()
            .register(SoftFailToolStub {
                calls: soft_fail_calls.clone(),
            })
            .unwrap()
            .register(SequenceToolStub {
                fail: false,
                calls: ok_calls.clone(),
            })
            .unwrap();
        let client = reqwest::Client::new();

        let outcome = run_agent_loop(
            Box::new(
                ResponsesAgentSession::new(
                    client,
                    "test-key".to_owned(),
                    Some(base_url),
                    "openai",
                    "gpt-test".to_owned(),
                    10 * 1024 * 1024,
                    1200,
                    None,
                    &[ChatMessage::user("先返回业务失败，再尝试依赖调用")],
                    &registry,
                    None,
                )
                .unwrap(),
            ),
            registry,
            test_context(),
            3,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "业务失败已汇总。");
        assert_eq!(soft_fail_calls.load(Ordering::SeqCst), 1);
        assert_eq!(ok_calls.load(Ordering::SeqCst), 0);
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        let second_input = state.requests[1]["input"].as_array().unwrap();
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_soft_fail_1"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"soft_failure\""))
        }));
        assert!(second_input.iter().any(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call_ok_2"
                && item["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("\"skipped\":true"))
        }));
    }
}
