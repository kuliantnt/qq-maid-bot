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
    Box<dyn std::future::Future<Output = Result<crate::provider::ChatOutcome, LlmError>> + Send>,
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
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
    let first_messages = requests[0]["messages"].as_array().unwrap();
    let second_messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(
        first_messages.as_slice(),
        &second_messages[..first_messages.len()]
    );
    assert_eq!(requests[0]["tools"], requests[1]["tools"]);
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
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
async fn tool_loop_budget_before_first_request_disables_tools_for_answer() {
    let (base_url, state) = spawn_mock_tool_loop(vec![json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "should not be requested"
            }
        }]
    })])
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
        Some(crate::context_budget::ContextBudgetConfig {
            context_window_chars: 120,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        }),
        2,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "should not be requested");
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].get("tools").is_none());
    assert!(requests[0].get("tool_choice").is_none());
}

#[tokio::test]
async fn non_stream_budget_finalization_rejects_provider_tool_calls_without_execution() {
    let (base_url, state) = spawn_mock_tool_loop(vec![json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_forbidden",
                    "type": "function",
                    "function": {
                        "name": "first_order_tool",
                        "arguments": r#"{"value":"must-not-run"}"#
                    }
                }]
            }
        }]
    })])
    .await;
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
    let sequence = Arc::new(StdMutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools
        .insert(Arc::new(PrepareOrderToolStub {
            name: "first_order_tool",
            sequence: sequence.clone(),
        }))
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

    assert_eq!(err.code, "tool_loop_limit");
    assert_eq!(err.stage, "tool_loop");
    assert!(sequence.lock().unwrap().is_empty());
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].get("tools").is_none());
    assert!(requests[0].get("tool_choice").is_none());
}

#[test]
fn tool_loop_budget_ignores_transport_only_payload_fields() {
    let messages = vec![json!({"role": "user", "content": "完成待办"})];
    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "list_todos",
            "description": "列出待办",
            "parameters": {"type": "object", "properties": {}},
        },
    })];
    let payload = chat_completions_tool_loop_payload(
        &messages,
        &tools,
        &"model-name-that-must-not-count".repeat(20),
        1200,
        true,
        true,
    );
    let model_context = json!({"messages": messages, "tools": tools});
    let model_context_chars = estimated_json_chars(&model_context, "tool_loop").unwrap();
    assert!(estimated_json_chars(&payload, "tool_loop").unwrap() > model_context_chars);

    enforce_tool_loop_budget(
        Some(ContextBudgetConfig {
            context_window_chars: model_context_chars + 20,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        }),
        &payload,
    )
    .unwrap();
}

#[test]
fn payload_disables_tool_calls_explicitly() {
    let payload = chat_completions_tool_loop_payload(
        &[json!({"role": "user", "content": "总结已有结果"})],
        &[json!({"type": "function", "function": {"name": "search"}})],
        "test-model",
        1200,
        false,
        false,
    );

    assert!(payload.get("tools").is_none());
    assert!(payload.get("tool_choice").is_none());

    let streaming_payload = chat_completions_tool_loop_payload(
        &[json!({"role": "user", "content": "总结已有结果"})],
        &[json!({"type": "function", "function": {"name": "search"}})],
        "test-model",
        1200,
        false,
        true,
    );
    assert!(streaming_payload.get("tools").is_none());
    assert!(streaming_payload.get("tool_choice").is_none());
    assert_eq!(streaming_payload["stream"], true);
}

#[tokio::test]
async fn tool_loop_budget_after_tool_result_disables_tools_for_final_answer() {
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
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
        Some(crate::context_budget::ContextBudgetConfig {
            context_window_chars: 500,
            output_reserve_chars: 20,
            protected_recent_turns: 0,
        }),
        2,
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "should not be requested");
    let requests = &state.lock().await.requests;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["tools"][0]["function"]["name"], "get_weather");
    assert!(requests[1].get("tools").is_none());
    assert!(requests[1].get("tool_choice").is_none());
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
    let client = ChatCompletionsClient::new(
        "test-key",
        Some(&base_url),
        qq_maid_common::http_client::client(),
    );
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
