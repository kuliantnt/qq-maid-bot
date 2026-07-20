use super::{
    payload::{enforce_tool_loop_budget, openai_tool_loop_payload},
    response::{FunctionCall, extract_function_calls},
    session::ResponsesAgentSession,
    streaming::{finalize_responses_tool_loop_stream, observe_responses_function_call_event},
};
use crate::{
    agent_loop::{
        AgentStep, AgentStepSession, AgentTextDeltaFuture, AgentTextDeltaSink, run_agent_loop,
    },
    context_budget::{ContextBudgetConfig, estimated_json_chars},
    error::LlmError,
    provider::types::{ChatMessage, ReasoningEffort},
    sse::SseFrame,
    tool::{Tool, ToolCallDependency, ToolContext, ToolMetadata, ToolOutput, ToolRegistry},
};
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{Response, header},
    routing::post,
};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
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
        actor: qq_maid_common::identity_context::ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: qq_maid_common::identity_context::ExecutionConversationContext {
            platform: "test".to_owned(),
            account_id: None,
            kind: qq_maid_common::identity_context::ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: None,
        execution_deadline: None,
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
    let latest = state.requests.last().expect("request recorded");
    if (latest.get("tools").is_none() && latest.get("tool_choice").is_none())
        || latest.get("tool_choice") == Some(&json!("none"))
        || latest
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return Json(json!({
            "output_text": "杭州今天有小雨，建议带伞。",
            "output": [{"type":"message","content":[{"type":"output_text","text":"杭州今天有小雨，建议带伞。"}]}]
        }));
    }
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

async fn mock_disabled_tools_function_call_handler(
    State(state): State<Arc<Mutex<ToolLoopMockState>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.lock().await.requests.push(body);
    Json(json!({
        "output": [{
            "type": "function_call",
            "name": "ok_tool",
            "call_id": "call_forbidden",
            "arguments": "{\"value\":\"must-not-run\"}"
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

async fn spawn_disabled_tools_function_call_mock() -> (String, Arc<Mutex<ToolLoopMockState>>) {
    let state = Arc::new(Mutex::new(ToolLoopMockState {
        requests: Vec::new(),
    }));
    let app = Router::new()
        .route(
            "/v1/responses",
            post(mock_disabled_tools_function_call_handler),
        )
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
        stream::once(async move { Ok::<Bytes, Infallible>(completed) }).chain(stream::pending()),
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

mod integration;
mod payload;
mod response;
mod streaming;
