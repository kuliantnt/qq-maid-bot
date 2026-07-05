//! 智谱 BigModel 提供商实现。
//!
//! BigModel 官方通用端点使用 HTTP Bearer 鉴权，并提供 OpenAI 兼容的
//! `/chat/completions` 接口；这里复用已有 Chat Completions adapter，
//! 只维护 BigModel 自身的配置项和模型前缀规则。

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::LlmConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol,
        openai::{
            ChatCompletionsClient, begin_chat_completions_session, chat_completions_stream,
            chat_completions_with_stream_fallback, provider_chat_completions_tool_calling_protocol,
        },
        outcome_to_stream,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

const MULTIMODAL_UNSUPPORTED_MESSAGE: &str =
    "我收到图片或文件了，但当前模型暂时不支持图片/文件理解。你可以补充文字说明，我先帮你记录。";

/// 智谱 BigModel 提供商实现。
pub struct BigModelProvider {
    /// OpenAI 兼容 Chat Completions 客户端。
    client: ChatCompletionsClient,
    /// 默认模型名称（如 `"glm-5.2"`）。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl BigModelProvider {
    /// 从 LLM 配置创建 BigModel 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .bigmodel_api_key
            .clone()
            .ok_or_else(|| LlmError::config("BIGMODEL_API_KEY is required"))?;
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build BigModel HTTP client: {err}"))
            })?;
        let client = ChatCompletionsClient::new(
            api_key,
            Some(config.bigmodel_base_url.as_str()),
            http_client,
        );

        Ok(Self {
            client,
            model: bigmodel_config_model(&config.bigmodel_model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for BigModelProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        reject_multimodal_if_needed(&req)?;
        let effective_model = effective_bigmodel_model(req.model.as_deref(), &self.model)?;
        chat_completions_with_stream_fallback(
            self.stream,
            &self.client,
            self.name(),
            &effective_model,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
        )
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        reject_multimodal_if_needed(&req)?;
        let effective_model = effective_bigmodel_model(req.model.as_deref(), &self.model)?;
        if !self.stream {
            let outcome = chat_completions_with_stream_fallback(
                false,
                &self.client,
                self.name(),
                &effective_model,
                req.max_output_tokens.unwrap_or(self.max_output_tokens),
                &req.messages,
            )
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        chat_completions_stream(
            &self.client,
            self.name(),
            &effective_model,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
            true,
        )
        .await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        if self.tool_calling_protocol(req.chat.model.as_deref())
            != Some(ToolCallingProtocol::ChatCompletionsToolCalls)
        {
            return Ok(None);
        }
        begin_chat_completions_session(
            req,
            self.client.clone(),
            self.name(),
            &self.model,
            req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
            effective_bigmodel_model,
        )
        .await
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        provider_chat_completions_tool_calling_protocol(
            model,
            &self.model,
            effective_bigmodel_model,
        )
    }

    fn name(&self) -> &str {
        "bigmodel"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

fn reject_multimodal_if_needed(req: &ChatRequest) -> Result<(), LlmError> {
    if req.has_non_text_parts() {
        return Err(LlmError::new(
            "unsupported_input_part",
            MULTIMODAL_UNSUPPORTED_MESSAGE,
            "request",
        ));
    }
    Ok(())
}

/// 验证并解析 BigModel 的配置模型名。
pub(crate) fn bigmodel_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "BIGMODEL_MODEL")?;
    match model.provider {
        Some(ModelProvider::BigModel) | None => Ok(model.name),
        Some(ModelProvider::OpenAi)
        | Some(ModelProvider::DeepSeek)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::config(
            "BIGMODEL_MODEL must use bigmodel: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的 BigModel 模型名称。
fn effective_bigmodel_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::BigModel) | None => Ok(model.name),
        Some(ModelProvider::OpenAi)
        | Some(ModelProvider::DeepSeek)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::new(
            "bad_request",
            "non-bigmodel-prefixed model cannot be used by BigModel provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolChatRequest;
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
    fn effective_bigmodel_model_strips_bigmodel_prefix() {
        assert_eq!(
            effective_bigmodel_model(Some("bigmodel:glm-5.2"), "default").unwrap(),
            "glm-5.2"
        );
        assert_eq!(
            effective_bigmodel_model(Some("zhipu:glm-4-flash"), "default").unwrap(),
            "glm-4-flash"
        );
        assert_eq!(
            effective_bigmodel_model(Some("glm-5.2"), "default").unwrap(),
            "glm-5.2"
        );
        assert_eq!(
            effective_bigmodel_model(None, "default").unwrap(),
            "default"
        );
    }

    #[test]
    fn effective_bigmodel_model_rejects_other_provider_prefix() {
        let err = effective_bigmodel_model(Some("openai:gpt-5-mini"), "default").unwrap_err();
        assert_eq!(err.code, "bad_request");

        let err = bigmodel_config_model("deepseek:deepseek-chat").unwrap_err();
        assert_eq!(err.code, "config");
    }

    #[test]
    fn bigmodel_tool_calling_protocol_uses_chat_completions() {
        let provider = BigModelProvider {
            client: ChatCompletionsClient::new("test-key", None, reqwest::Client::new()),
            model: "glm-5.2".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };

        assert_eq!(
            provider.tool_calling_protocol(Some("bigmodel:glm-5.2")),
            Some(ToolCallingProtocol::ChatCompletionsToolCalls)
        );
        assert_eq!(provider.tool_calling_protocol(Some("openai:gpt-5")), None);
    }

    #[tokio::test]
    async fn chat_retries_non_stream_after_empty_sse_when_stream_enabled() {
        let (base_url, state) = spawn_mock_chat(vec![
            "data: [DONE]\n\n".to_owned(),
            json!({"choices": [{"message": {"content": "bigmodel non-stream"}}]}).to_string(),
        ])
        .await;
        let provider = BigModelProvider {
            client: ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new()),
            model: "glm-5.2".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };

        let outcome = provider
            .chat(ChatRequest {
                session_id: "s".to_owned(),
                model: None,
                messages: vec![crate::provider::types::ChatMessage::user("hi")],
                context_budget: None,
                max_output_tokens: None,
                reasoning_effort: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "bigmodel non-stream");
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["stream"], true);
        assert!(requests[1].get("stream").is_none());
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
                                "arguments": r#"{"city":"北京"}"#
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
                        "content": "北京今天晴。"
                    }
                }]
            })
            .to_string(),
        ])
        .await;
        let provider = BigModelProvider {
            client: ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new()),
            model: "glm-5.2".to_owned(),
            stream: true,
            max_output_tokens: 1200,
        };
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("晴"))
            .unwrap();

        let outcome = provider
            .chat_with_tools(ToolChatRequest {
                chat: ChatRequest {
                    session_id: "s".to_owned(),
                    model: None,
                    messages: vec![crate::provider::types::ChatMessage::user("北京天气怎么样")],
                    context_budget: None,
                    max_output_tokens: None,
                    reasoning_effort: None,
                    metadata: Default::default(),
                },
                tools,
                tool_context: test_tool_context(),
                max_rounds: 2,
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "北京今天晴。");
        assert_eq!(outcome.executed_tools, vec!["get_weather"]);
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], "auto");
        assert_eq!(requests[1]["messages"][2]["role"], "tool");
    }
}
