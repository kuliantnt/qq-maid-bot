//! OpenAI Responses 原生 Function Tool Loop 的协议适配层。
//!
//! 本模块只处理 Responses 协议层的 function call / function_call_output 往返，
//! 把一次模型请求转换为统一 [`AgentStep`]。轮次推进、最大轮数、工具执行和
//! 退出条件由 `qq_maid_llm::agent_loop::run_agent_loop` 统一控制；本模块不再
//! 维护自己的循环。具体业务能力由上层 crate 通过 `ToolRegistry` 注册，
//! 避免 LLM crate 反向依赖 Core。

use serde_json::{Value, json};

use crate::{
    agent_loop::{AgentStep, AgentStepSession, AgentToolCall, AgentToolResult},
    context_budget::{
        BudgetItemKind, ContextBudgetConfig, ensure_required_budget, estimated_json_chars,
        log_budget_report,
    },
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    tool::{ToolMetadata, ToolRegistry},
};

use super::{
    extract::{extract_response_output_text, extract_response_usage},
    payload::{openai_model_supports_reasoning, openai_responses_message},
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
    provider: &'static str,
    model: String,
    max_output_tokens: u64,
    reasoning_effort: Option<ReasoningEffort>,
    input: Vec<Value>,
    tool_defs: Vec<Value>,
    context_budget: Option<ContextBudgetConfig>,
}

impl ResponsesAgentSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        client: reqwest::Client,
        api_key: String,
        base_url: Option<String>,
        provider: &'static str,
        model: String,
        max_output_tokens: u64,
        reasoning_effort: Option<ReasoningEffort>,
        messages: &[ChatMessage],
        tools: &ToolRegistry,
        context_budget: Option<ContextBudgetConfig>,
    ) -> Result<Self, LlmError> {
        let input = openai_tool_loop_input(messages)?;
        let tool_defs = openai_tool_defs(tools.metadata());
        Ok(Self {
            client,
            api_key,
            base_url,
            provider,
            model,
            max_output_tokens,
            reasoning_effort,
            input,
            tool_defs,
            context_budget,
        })
    }
}

#[async_trait::async_trait]
impl AgentStepSession for ResponsesAgentSession {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        // 回填上一轮工具执行结果（首轮 results 为空，跳过）。
        for result in results {
            self.input.push(json!({
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": result.output,
            }));
        }

        let payload = openai_tool_loop_payload(
            &self.input,
            &self.tool_defs,
            &self.model,
            self.max_output_tokens,
            self.reasoning_effort,
            allow_tool_calls,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionCall {
    name: String,
    call_id: String,
    arguments: String,
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
    reasoning_effort: Option<ReasoningEffort>,
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
    if let Some(effort) = reasoning_effort.filter(|_| openai_model_supports_reasoning(model)) {
        payload["reasoning"] = json!({ "effort": effort.as_str() });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::run_agent_loop;
    use crate::tool::{Tool, ToolCallDependency, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use axum::{Json, Router, extract::State, routing::post};
    use serde_json::json;
    use std::sync::{
        Arc, Mutex as StdMutex,
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
        );

        assert_eq!(payload["parallel_tool_calls"], false);
        assert!(payload.get("tool_choice").is_none());
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
