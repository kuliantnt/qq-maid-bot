//! DeepSeek 提供商实现。
//!
//! DeepSeek 使用 OpenAI 兼容 Chat Completions 协议；本模块只维护
//! DeepSeek 的 base URL、认证和模型前缀规则差异。

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    config::LlmConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol, ToolChatRequest,
        openai::{
            ChatCompletionsClient, chat_completions_stream, chat_completions_with_stream_fallback,
            provider_chat_completions_tool_calling_protocol,
            provider_chat_with_chat_completions_tools,
        },
        outcome_to_stream,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// DeepSeek 提供商实现。
pub struct DeepSeekProvider {
    /// OpenAI 兼容 Chat Completions 客户端。
    client: ChatCompletionsClient,
    /// 默认模型名称（如 `"deepseek-chat"`）。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl DeepSeekProvider {
    /// 从 LLM 配置创建 DeepSeek 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .deepseek_api_key
            .clone()
            .ok_or_else(|| LlmError::config("DEEPSEEK_API_KEY is required"))?;
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build DeepSeek HTTP client: {err}"))
            })?;
        let client = ChatCompletionsClient::new(
            api_key,
            Some(config.deepseek_base_url.as_str()),
            http_client,
        );

        Ok(Self {
            client,
            model: deepseek_config_model(&config.deepseek_model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for DeepSeekProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_deepseek_model(req.model.as_deref(), &self.model)?;
        chat_completions_with_stream_fallback(
            self.stream,
            &self.client,
            self.name(),
            &effective_model,
            self.max_output_tokens,
            &req.messages,
        )
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let effective_model = effective_deepseek_model(req.model.as_deref(), &self.model)?;
        if !self.stream {
            let outcome = chat_completions_with_stream_fallback(
                false,
                &self.client,
                self.name(),
                &effective_model,
                self.max_output_tokens,
                &req.messages,
            )
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        chat_completions_stream(
            &self.client,
            self.name(),
            &effective_model,
            self.max_output_tokens,
            &req.messages,
            true,
        )
        .await
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        if self.tool_calling_protocol(req.chat.model.as_deref())
            != Some(ToolCallingProtocol::ChatCompletionsToolCalls)
        {
            return self.chat(req.chat).await;
        }
        provider_chat_with_chat_completions_tools(
            &self.client,
            self.name(),
            &self.model,
            self.max_output_tokens,
            req,
            effective_deepseek_model,
        )
        .await
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        provider_chat_completions_tool_calling_protocol(
            model,
            &self.model,
            effective_deepseek_model,
        )
    }

    fn name(&self) -> &'static str {
        "deepseek"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

/// 验证并解析 DeepSeek 的配置模型名。
pub(crate) fn deepseek_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "DEEPSEEK_MODEL")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::BigModel) => Err(LlmError::config(
            "DEEPSEEK_MODEL must use deepseek: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的 DeepSeek 模型名称。
fn effective_deepseek_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::DeepSeek) | None => Ok(model.name),
        Some(ModelProvider::OpenAi) | Some(ModelProvider::BigModel) => Err(LlmError::new(
            "bad_request",
            "non-deepseek-prefixed model cannot be used by DeepSeek provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::test_support::{WeatherToolStub, test_tool_context},
        tool::ToolRegistry,
    };
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockChatState {
        bodies: Vec<String>,
        requests: Vec<Value>,
    }

    async fn mock_chat_handler(
        State(state): State<Arc<Mutex<MockChatState>>>,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let mut state = state.lock().await;
        state.requests.push(request);
        let body = state.bodies.remove(0);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
    }

    async fn spawn_mock_chat(bodies: Vec<String>) -> (String, Arc<Mutex<MockChatState>>) {
        let state = Arc::new(Mutex::new(MockChatState {
            bodies,
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/chat/completions", post(mock_chat_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), state)
    }

    #[test]
    fn effective_deepseek_model_strips_deepseek_prefix() {
        assert_eq!(
            effective_deepseek_model(Some("deepseek:deepseek-chat"), "default").unwrap(),
            "deepseek-chat"
        );
        assert_eq!(
            effective_deepseek_model(Some("deepseek-chat"), "default").unwrap(),
            "deepseek-chat"
        );
        assert_eq!(
            effective_deepseek_model(None, "default").unwrap(),
            "default"
        );
    }

    #[test]
    fn effective_deepseek_model_rejects_openai_prefix() {
        let err = effective_deepseek_model(Some("openai:gpt-5-mini"), "default").unwrap_err();
        assert_eq!(err.code, "bad_request");
    }

    #[test]
    fn deepseek_tool_calling_protocol_uses_chat_completions() {
        let provider = DeepSeekProvider {
            client: ChatCompletionsClient::new("test-key", None, reqwest::Client::new()),
            model: "deepseek-chat".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };

        assert_eq!(
            provider.tool_calling_protocol(Some("deepseek:deepseek-chat")),
            Some(ToolCallingProtocol::ChatCompletionsToolCalls)
        );
        assert_eq!(provider.tool_calling_protocol(Some("openai:gpt-5")), None);
    }

    #[tokio::test]
    async fn chat_retries_non_stream_after_empty_sse_when_stream_enabled() {
        let (base_url, state) = spawn_mock_chat(vec![
            "data: [DONE]\n\n".to_owned(),
            json!({"choices": [{"message": {"content": "deepseek non-stream"}}]}).to_string(),
        ])
        .await;
        let provider = DeepSeekProvider {
            client: ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new()),
            model: "deepseek-chat".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };

        let outcome = provider
            .chat(ChatRequest {
                session_id: "s".to_owned(),
                model: None,
                messages: vec![crate::provider::types::ChatMessage::user("hi")],
                context_budget: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "deepseek non-stream");
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["stream"], true);
        assert!(requests[1].get("stream").is_none());
    }

    #[tokio::test]
    async fn stream_chat_after_delta_error_does_not_retry_non_stream() {
        let (base_url, state) = spawn_mock_chat(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\ndata: {not-json}\n\n"
                .to_owned(),
        ])
        .await;
        let provider = DeepSeekProvider {
            client: ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new()),
            model: "deepseek-chat".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };

        let mut stream = provider
            .stream_chat(ChatRequest {
                session_id: "s".to_owned(),
                model: None,
                messages: vec![crate::provider::types::ChatMessage::user("hi")],
                context_budget: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();
        assert!(matches!(
            stream.next().await,
            Some(Ok(crate::provider::LlmStreamEvent::TextDelta(_)))
        ));
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(state.lock().await.requests.len(), 1);
    }

    #[tokio::test]
    async fn chat_with_tools_runs_chat_completions_tool_loop() {
        let (base_url, state) = spawn_mock_chat(vec![
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
            })
            .to_string(),
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "杭州有阵雨，记得带伞。"
                    }
                }]
            })
            .to_string(),
        ])
        .await;
        let provider = DeepSeekProvider {
            client: ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new()),
            model: "deepseek-chat".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("阵雨"))
            .unwrap();

        let outcome = provider
            .chat_with_tools(ToolChatRequest {
                chat: ChatRequest {
                    session_id: "s".to_owned(),
                    model: None,
                    messages: vec![crate::provider::types::ChatMessage::user("杭州天气怎么样")],
                    context_budget: None,
                    metadata: Default::default(),
                },
                tools,
                tool_context: test_tool_context(),
                max_rounds: 2,
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "杭州有阵雨，记得带伞。");
        assert_eq!(outcome.executed_tools, vec!["get_weather"]);
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], "auto");
        assert_eq!(requests[1]["messages"][2]["role"], "tool");
    }
}
