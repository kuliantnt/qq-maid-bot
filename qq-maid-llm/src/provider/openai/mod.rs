//! OpenAI 提供商实现。
//!
//! OpenAI 主链路直接调用 Responses API，失败时再降级到 rig-core Chat
//! Completions。此文件只保留 provider glue logic；HTTP、SSE、payload 细节
//! 分别下沉到子模块中，避免入口继续承担多种职责。

mod chat;
mod extract;
mod fallback;
mod payload;
mod responses;
mod stream;
mod transport;

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    config::AppConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider,
        types::{ChatMessage, ChatRequest, ModelId, ModelProvider},
    },
};

#[allow(unused_imports)]
pub(crate) use chat::{completion_with_stream_fallback, to_rig_messages};

struct OpenAiChatFallbackRequest<'a> {
    stream: bool,
    responses_client: &'a reqwest::Client,
    rig_client: &'a chat::RigChatFallbackClient,
    api_key: &'a str,
    base_url: Option<&'a str>,
    provider: &'a str,
    model: &'a str,
    max_output_tokens: u64,
    messages: &'a [ChatMessage],
}

/// OpenAI 提供商实现。
pub struct OpenAiRigProvider {
    /// 直连 Responses API 的 HTTP 客户端。
    ///
    /// 历史名称保留为 `OpenAiRigProvider` 以减少调用方改动；主链路直接拼
    /// Responses 请求体，避免无 provider message id 的 assistant 历史被兼容层
    /// 序列化成 `input_text`。
    responses_client: reqwest::Client,
    /// rig-core Chat Completions fallback 客户端。
    ///
    /// 某些 OpenAI 兼容层在 `/v1/responses` 或 SSE 链路上并不稳定，保留这条保守
    /// 路径可以尽量保证 QQ 侧仍能拿到回复。
    rig_client: chat::RigChatFallbackClient,
    /// OpenAI API 密钥。
    api_key: String,
    /// 自定义 API 基础地址。
    base_url: Option<String>,
    /// 默认模型名称。
    model: String,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl OpenAiRigProvider {
    /// 从应用配置创建 OpenAI 提供商实例。
    ///
    /// 需要配置 `openai_api_key`，可选自定义 `openai_base_url`。
    /// HTTP 客户端超时时间由 `request_timeout_seconds` 控制。
    pub fn new(config: &AppConfig) -> Result<Self, LlmError> {
        let api_key = config
            .openai_api_key
            .clone()
            .ok_or_else(|| LlmError::config("OPENAI_API_KEY is required"))?;
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build OpenAI HTTP client: {err}"))
            })?;
        let rig_client = chat::RigChatFallbackClient::new(
            &api_key,
            config.openai_base_url.as_deref(),
            http_client.clone(),
        )?;

        Ok(Self {
            responses_client: http_client,
            rig_client,
            api_key,
            base_url: config.openai_base_url.clone(),
            model: openai_config_model(&config.model)?,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiRigProvider {
    /// 执行聊天补全，根据配置选择流式或非流式调用。`model` 支持 `"openai:"` 前缀。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_openai_model(req.model.as_deref(), &self.model)?;
        openai_chat_with_rig_fallback(OpenAiChatFallbackRequest {
            stream: self.stream,
            responses_client: &self.responses_client,
            rig_client: &self.rig_client,
            api_key: &self.api_key,
            base_url: self.base_url.as_deref(),
            provider: self.name(),
            model: &effective_model,
            max_output_tokens: self.max_output_tokens,
            messages: &req.messages,
        })
        .await
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

/// OpenAI 普通聊天：优先走直接 Responses 请求，失败时降级到 rig-core Chat Completions。
///
/// 这里保留两条链路是为了兼容不同上游网关：官方 Responses schema 需要 assistant
/// 历史使用 `output_text`，但部分 OpenAI 兼容层会在 `/v1/responses`、SSE 或 HTML
/// 错误页上表现不稳定；降级到 rig-core 的 Chat Completions 可尽量保证 QQ 端有回复。
async fn openai_chat_with_rig_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    match responses::openai_responses_chat_with_stream_fallback(
        responses::OpenAiResponsesChatRequest {
            stream: req.stream,
            client: req.responses_client,
            api_key: req.api_key,
            base_url: req.base_url,
            provider: req.provider,
            model: req.model,
            max_output_tokens: req.max_output_tokens,
            messages: req.messages,
        },
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
        Err(err) if fallback::should_fallback_to_rig_after_responses_error(&err) => {
            tracing::warn!(
                provider = req.provider,
                model = %req.model,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "OpenAI Responses chat failed; falling back to rig-core Chat Completions"
            );
            chat::openai_rig_chat_with_stream_fallback(
                req.stream,
                req.rig_client,
                req.provider,
                req.model,
                req.max_output_tokens,
                req.messages,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

/// 验证并解析 OpenAI 的配置模型名。
///
/// 只允许 `openai:` 前缀或无前缀；若为 `deepseek:` 前缀则返回配置错误。
pub(crate) fn openai_config_model(value: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, "LLM_MODEL")?;
    match model.provider {
        Some(ModelProvider::OpenAi) | None => Ok(model.name),
        Some(ModelProvider::DeepSeek) => Err(LlmError::config(
            "LLM_MODEL for OpenAI provider must use openai: prefix or no prefix",
        )),
    }
}

/// 决定本次请求实际使用的模型名称。
///
/// 如果请求中指定了模型，则去掉 `openai:` 前缀后返回；
/// 若指定了 `deepseek:` 前缀则拒绝；无指定时返回默认模型。
fn effective_openai_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::OpenAi) | None => Ok(model.name),
        Some(ModelProvider::DeepSeek) => Err(LlmError::new(
            "bad_request",
            "deepseek-prefixed model cannot be used by OpenAI provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router, extract::State, http::StatusCode as AxumStatusCode, response::IntoResponse,
        routing::post,
    };
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockOpenAiState {
        responses_status: AxumStatusCode,
        responses_body: Value,
        chat_body: Value,
        responses_calls: usize,
        chat_calls: usize,
        chat_requests: Vec<Value>,
    }

    async fn mock_responses_handler(
        State(state): State<Arc<Mutex<MockOpenAiState>>>,
        Json(_body): Json<Value>,
    ) -> impl IntoResponse {
        let mut state = state.lock().await;
        state.responses_calls += 1;
        (state.responses_status, Json(state.responses_body.clone()))
    }

    async fn mock_chat_completions_handler(
        State(state): State<Arc<Mutex<MockOpenAiState>>>,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let mut state = state.lock().await;
        state.chat_calls += 1;
        state.chat_requests.push(body);
        Json(state.chat_body.clone())
    }

    async fn spawn_mock_openai(state: MockOpenAiState) -> (String, Arc<Mutex<MockOpenAiState>>) {
        let state = Arc::new(Mutex::new(state));
        let app = Router::new()
            .route("/v1/responses", post(mock_responses_handler))
            .route("/v1/chat/completions", post(mock_chat_completions_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    fn mock_chat_response(text: &str) -> Value {
        serde_json::json!({
            "id": "chatcmpl_test",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-5.5",
            "system_fingerprint": null,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": text
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 2,
                "total_tokens": 5
            }
        })
    }

    #[tokio::test]
    async fn openai_chat_uses_responses_without_rig_fallback_when_responses_succeeds() {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
            responses_status: AxumStatusCode::OK,
            responses_body: serde_json::json!({"output_text": "responses ok"}),
            chat_body: mock_chat_response("chat fallback"),
            responses_calls: 0,
            chat_calls: 0,
            chat_requests: Vec::new(),
        })
        .await;
        let http_client = reqwest::Client::new();
        let rig_client =
            chat::RigChatFallbackClient::new("test-key", Some(&base_url), http_client.clone())
                .unwrap();

        let outcome = openai_chat_with_rig_fallback(OpenAiChatFallbackRequest {
            stream: false,
            responses_client: &http_client,
            rig_client: &rig_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            messages: &[ChatMessage::user("hi")],
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "responses ok");
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1);
        assert_eq!(state.chat_calls, 0);
    }

    #[tokio::test]
    async fn openai_chat_falls_back_to_rig_chat_completions_after_responses_http_error() {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
            responses_status: AxumStatusCode::BAD_REQUEST,
            responses_body: serde_json::json!({
                "error": {
                    "message": "Invalid value: 'input_text'. Supported values are: 'output_text' and 'refusal'."
                }
            }),
            chat_body: mock_chat_response("chat fallback ok"),
            responses_calls: 0,
            chat_calls: 0,
            chat_requests: Vec::new(),
        })
        .await;
        let http_client = reqwest::Client::new();
        let rig_client =
            chat::RigChatFallbackClient::new("test-key", Some(&base_url), http_client.clone())
                .unwrap();

        let messages = [
            ChatMessage::system("system"),
            ChatMessage {
                role: crate::provider::types::ChatRole::Assistant,
                content: "old reply".to_owned(),
            },
            ChatMessage::user("again"),
        ];
        let outcome = openai_chat_with_rig_fallback(OpenAiChatFallbackRequest {
            stream: false,
            responses_client: &http_client,
            rig_client: &rig_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            messages: &messages,
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "chat fallback ok");
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1);
        assert_eq!(state.chat_calls, 1);
        let request = state.chat_requests.first().unwrap();
        assert_eq!(request["model"], "gpt-5.5");
        assert_eq!(request["messages"][1]["role"], "assistant");
        assert_eq!(request["messages"][1]["content"][0]["type"], "text");
        assert_eq!(request["messages"][1]["content"][0]["text"], "old reply");
        assert!(request.get("input").is_none());
    }

    #[test]
    fn effective_openai_model_strips_openai_prefix() {
        assert_eq!(
            effective_openai_model(Some("openai:gpt-5-mini"), "default").unwrap(),
            "gpt-5-mini"
        );
        assert_eq!(
            effective_openai_model(Some("gpt-5-mini"), "default").unwrap(),
            "gpt-5-mini"
        );
        assert_eq!(effective_openai_model(None, "default").unwrap(), "default");
    }

    #[test]
    fn effective_openai_model_rejects_deepseek_prefix() {
        let err = effective_openai_model(Some("deepseek:deepseek-chat"), "default").unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
}
