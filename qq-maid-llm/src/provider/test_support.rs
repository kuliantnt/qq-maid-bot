//! provider 单测共享辅助。
//!
//! 这里只放多个 provider 测试都会复用的轻量 stub，避免把同一组测试工具在
//! DeepSeek / BigModel / OpenAI 之间各复制一份。

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    error::LlmError,
    tool::{Tool, ToolContext, ToolMetadata, ToolOutput},
};

pub(crate) struct WeatherToolStub {
    weather: &'static str,
}

impl WeatherToolStub {
    pub(crate) fn new(weather: &'static str) -> Self {
        Self { weather }
    }
}

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
            "ok": true,
            "city": arguments["city"],
            "weather": self.weather
        })))
    }
}

pub(crate) fn test_tool_context() -> ToolContext {
    ToolContext {
        task_id: "task-1".to_owned(),
        actor: ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "test".to_owned(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: None,
        execution_deadline: None,
    }
}

#[derive(Debug)]
pub(crate) struct MockChatCompletionsState {
    bodies: VecDeque<String>,
    pub(crate) requests: Vec<Value>,
}

async fn mock_chat_completions_handler(
    State(state): State<Arc<Mutex<MockChatCompletionsState>>>,
    body: Body,
) -> impl IntoResponse {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    let mut state = state.lock().await;
    state.requests.push(request);
    let body = state
        .bodies
        .pop_front()
        .expect("mock chat response queue should not be empty");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

/// 启动 Chat Completions provider 共用的顺序响应 fake，并保留完整请求供各协议测试断言。
pub(crate) async fn spawn_chat_completions_mock(
    bodies: Vec<String>,
) -> (String, Arc<Mutex<MockChatCompletionsState>>) {
    let state = Arc::new(Mutex::new(MockChatCompletionsState {
        bodies: bodies.into(),
        requests: Vec::new(),
    }));
    let app = Router::new()
        .route("/chat/completions", post(mock_chat_completions_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}
