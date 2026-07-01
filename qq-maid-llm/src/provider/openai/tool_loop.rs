//! OpenAI Responses 原生 Function Tool Loop。
//!
//! 本模块只处理 Responses 协议层的 function call / function_call_output 往返。
//! 具体业务能力由上层 crate 通过 `ToolRegistry` 注册，避免 LLM crate 反向依赖 Core。

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
        ChatOutcome,
        tool_loop::{ToolLoopCall, ToolLoopExecutor},
        types::{ChatMessage, TokenUsage},
    },
    tool::{ToolContext, ToolMetadata, ToolRegistry},
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
    let mut tool_executor = ToolLoopExecutor::new(&req.tools, &req.tool_context);

    for round in 0..=req.max_rounds {
        let payload = openai_tool_loop_payload(
            &input,
            &tools,
            req.model,
            req.max_output_tokens,
            round < req.max_rounds,
        );
        enforce_tool_loop_budget(req.context_budget, &payload)?;
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
                executed_tools: tool_executor.executed_tools(),
                tool_results: tool_executor.tool_results(),
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
        tool_executor.reset_dependency_chain();
        // 同轮工具调用必须先完成全部参数预绑定，再允许任何工具修改状态；
        // Todo 的可见编号选择依赖这个边界，不能边 prepare 边执行。
        let prepared_calls = calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                tool_executor.prepare_call(
                    ToolLoopCall {
                        name: &call.name,
                        call_id: &call.call_id,
                        arguments: &call.arguments,
                    },
                    round,
                    index,
                )
            })
            .collect::<Vec<_>>();
        for (call, prepared) in calls.iter().zip(prepared_calls) {
            let result = tool_executor.execute_prepared_call(prepared).await;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": result.output,
            }));
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
            context_budget: None,
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
    async fn tool_loop_budget_exceeded_before_first_provider_request() {
        let (base_url, state) = spawn_tool_loop_mock().await;
        let registry = ToolRegistry::new().register(WeatherToolStub).unwrap();
        let client = reqwest::Client::new();

        let err = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州今天需要带伞吗？")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 120,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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

        let err = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("杭州今天需要带伞吗？")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 420,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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

        let err = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("__force_json_estimate_error__")],
            context_budget: Some(crate::context_budget::ContextBudgetConfig {
                context_window_chars: 10_000,
                output_reserve_chars: 20,
                protected_recent_turns: 0,
            }),
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("连续执行两个工具")],
            context_budget: None,
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

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("同轮调用两个工具")],
            context_budget: None,
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("先失败再查天气")],
            context_budget: None,
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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

        let outcome = openai_responses_tool_loop(OpenAiToolLoopRequest {
            client: &client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-test",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("先返回业务失败，再尝试依赖调用")],
            context_budget: None,
            tools: registry,
            tool_context: test_context(),
            max_rounds: 3,
        })
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
