//! OpenAI Responses Agent 单步会话实现。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::Value;

use crate::{
    agent_loop::{
        AgentStep, AgentStepSession, AgentStreamingDiagnostics, AgentTextDeltaSink, AgentToolCall,
        AgentToolResult,
    },
    context_budget::ContextBudgetConfig,
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    tool::ToolRegistry,
};

use super::{
    diagnostics::{classify_responses_stream_failure, replace_streaming_diagnostics},
    payload::{
        enforce_tool_loop_budget, openai_tool_defs, openai_tool_loop_input,
        openai_tool_loop_payload,
    },
    response::{append_response_output_items, append_tool_results, extract_function_calls},
    streaming::collect_responses_tool_loop_stream,
};
use crate::provider::openai::{
    extract::{
        extract_response_output_parts, extract_response_output_text, extract_response_usage,
    },
    tool_calls_disabled_error,
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
    #[cfg(test)]
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
        Self::new_with_image_generation(
            client,
            api_key,
            base_url,
            provider,
            model,
            media_max_bytes,
            max_output_tokens,
            reasoning_effort,
            messages,
            tools,
            context_budget,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_image_generation(
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
        image_generation_enabled: bool,
    ) -> Result<Self, LlmError> {
        let input = openai_tool_loop_input(messages, media_max_bytes)?;
        let mut tool_defs = openai_tool_defs(tools.metadata());
        if image_generation_enabled {
            tool_defs.push(serde_json::json!({"type": "image_generation"}));
        }
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
        let (payload, tools_disabled) = enforce_tool_loop_budget(self.context_budget, payload)?;
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
        if !calls.is_empty() && (!allow_tool_calls || tools_disabled) {
            return Err(tool_calls_disabled_error());
        }
        if calls.is_empty() {
            let output_parts = extract_response_output_parts(&body);
            let reply = extract_response_output_text(&body).unwrap_or_default();
            if reply.trim().is_empty() && output_parts.is_empty() {
                return Err(LlmError::provider(
                    "OpenAI tool loop returned empty final output",
                    "provider",
                ));
            }
            Ok(AgentStep::FinalAnswer {
                reply,
                output_parts,
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
        let (payload, tools_disabled) = enforce_tool_loop_budget(self.context_budget, payload)?;
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
            allow_tool_calls && !tools_disabled,
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
