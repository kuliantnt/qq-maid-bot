//! OpenAI 提供商实现。
//!
//! 主链路直接调用 Responses API；在 `OPENAI_API_MODE=auto` 且 Responses
//! 出现可恢复上游错误时，降级到同 endpoint 的 Chat Completions。

mod chat;
mod chat_tool_loop;
mod extract;
mod fallback;
mod payload;
mod responses;
mod stream;
mod tool_loop;
mod transport;

use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream as futures_stream};

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{LlmConfig, OpenAiApiMode},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol,
        outcome_to_stream,
        types::{ChatMessage, ChatRequest, ModelProvider, ModelRoute},
    },
};

pub(crate) use chat::{
    ChatCompletionsClient, chat_completions_stream, chat_completions_with_stream_fallback,
};
pub(crate) use chat_tool_loop::{
    begin_chat_completions_session, provider_chat_completions_tool_calling_protocol,
};

struct OpenAiChatFallbackRequest<'a> {
    api_mode: OpenAiApiMode,
    stream: bool,
    responses_client: &'a reqwest::Client,
    chat_client: &'a ChatCompletionsClient,
    api_key: &'a str,
    base_url: Option<&'a str>,
    provider: &'a str,
    model: &'a str,
    max_output_tokens: u64,
    reasoning_effort: Option<crate::provider::types::ReasoningEffort>,
    messages: &'a [ChatMessage],
}

/// OpenAI 提供商实现。
pub struct OpenAiProvider {
    /// 直连 Responses API 的 HTTP 客户端。
    responses_client: reqwest::Client,
    /// OpenAI 兼容 Chat Completions fallback 客户端。
    chat_client: ChatCompletionsClient,
    /// OpenAI API 密钥。
    api_key: String,
    /// 自定义 API 基础地址。
    base_url: Option<String>,
    /// 默认模型名称。
    model: String,
    api_mode: OpenAiApiMode,
    /// 是否启用流式传输。
    stream: bool,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl OpenAiProvider {
    /// 从 LLM 配置创建 OpenAI 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
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
        let chat_client = ChatCompletionsClient::new(
            api_key.clone(),
            config.openai_base_url.as_deref(),
            http_client.clone(),
        );

        Ok(Self {
            responses_client: http_client,
            chat_client,
            api_key,
            base_url: config.openai_base_url.clone(),
            model: openai_config_model(&config.model_route)?,
            api_mode: config.openai_api_mode,
            stream: config.stream,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    /// 执行聊天补全，根据配置选择 Responses 或 Chat Completions。`model` 支持 `"openai:"` 前缀。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_openai_model(req.model.as_deref(), &self.model)?;
        openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: self.api_mode,
            stream: self.stream,
            responses_client: &self.responses_client,
            chat_client: &self.chat_client,
            api_key: &self.api_key,
            base_url: self.base_url.as_deref(),
            provider: self.name(),
            model: &effective_model,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
        })
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let effective_model = effective_openai_model(req.model.as_deref(), &self.model)?;
        if !self.stream {
            let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
                api_mode: self.api_mode,
                stream: false,
                responses_client: &self.responses_client,
                chat_client: &self.chat_client,
                api_key: &self.api_key,
                base_url: self.base_url.as_deref(),
                provider: self.name(),
                model: &effective_model,
                max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
                reasoning_effort: req.reasoning_effort,
                messages: &req.messages,
            })
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        openai_stream_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: self.api_mode,
            stream: self.stream,
            responses_client: &self.responses_client,
            chat_client: &self.chat_client,
            api_key: &self.api_key,
            base_url: self.base_url.as_deref(),
            provider: self.name(),
            model: &effective_model,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
        })
        .await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        // 仅 Responses auto 模式适配 Tool Calling；ChatOnly 等返回 None，
        // 由 LlmProvider::chat_with_tools 默认实现安全回退到普通 chat。
        if self.tool_calling_protocol(req.chat.model.as_deref())
            != Some(ToolCallingProtocol::OpenAiResponses)
        {
            return Ok(None);
        }
        let effective_model = effective_openai_model(req.chat.model.as_deref(), &self.model)?;
        Ok(Some(Box::new(tool_loop::ResponsesAgentSession::new(
            self.responses_client.clone(),
            self.api_key.clone(),
            self.base_url.clone(),
            self.name(),
            effective_model,
            req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
            req.chat.reasoning_effort,
            &req.chat.messages,
            req.tools,
            req.chat.context_budget,
        )?)))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        if self.api_mode == OpenAiApiMode::Auto
            && effective_openai_model(model, &self.model).is_ok()
        {
            Some(ToolCallingProtocol::OpenAiResponses)
        } else {
            None
        }
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

async fn openai_stream_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<LlmStream, LlmError> {
    if !req.stream {
        let outcome = openai_chat_with_chat_fallback(req).await?;
        return Ok(outcome_to_stream(outcome));
    }
    match req.api_mode {
        OpenAiApiMode::Auto => openai_auto_stream_with_chat_fallback(req).await,
        OpenAiApiMode::ChatOnly => {
            chat::chat_completions_stream(
                req.chat_client,
                req.provider,
                req.model,
                req.max_output_tokens,
                req.messages,
                true,
            )
            .await
        }
    }
}

async fn openai_chat_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    match req.api_mode {
        OpenAiApiMode::Auto => openai_auto_chat_with_chat_fallback(req).await,
        OpenAiApiMode::ChatOnly => {
            chat_completions_with_stream_fallback(
                req.stream,
                req.chat_client,
                req.provider,
                req.model,
                req.max_output_tokens,
                req.messages,
            )
            .await
        }
    }
}

async fn openai_auto_stream_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<LlmStream, LlmError> {
    let responses_req = responses::OpenAiResponsesChatRequest {
        stream: true,
        client: req.responses_client,
        api_key: req.api_key,
        base_url: req.base_url,
        provider: req.provider,
        model: req.model,
        max_output_tokens: req.max_output_tokens,
        reasoning_effort: req.reasoning_effort,
        messages: req.messages,
        allow_completed_response_fallback: true,
    };
    match responses::openai_responses_chat_stream(&responses_req).await {
        Ok(stream) => Ok(openai_responses_runtime_fallback_stream(stream, req)),
        Err(err) if fallback::should_fallback_to_chat_after_responses_error(&err) => {
            tracing::warn!(
                provider = req.provider,
                model = %req.model,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "OpenAI Responses stream init failed; falling back to Chat Completions stream"
            );
            chat::chat_completions_stream(
                req.chat_client,
                req.provider,
                req.model,
                req.max_output_tokens,
                req.messages,
                true,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

fn openai_responses_runtime_fallback_stream(
    responses_stream: LlmStream,
    req: OpenAiChatFallbackRequest<'_>,
) -> LlmStream {
    Box::pin(futures_stream::unfold(
        OpenAiRuntimeFallbackStreamState {
            responses_stream: Some(responses_stream),
            chat_stream: None,
            chat_client: req.chat_client.clone(),
            provider: req.provider.to_owned(),
            model: req.model.to_owned(),
            max_output_tokens: req.max_output_tokens,
            messages: req.messages.to_vec(),
            emitted_non_empty_delta: false,
            fallback_used: false,
            done: false,
        },
        |mut state| async move {
            let event = next_openai_runtime_fallback_event(&mut state).await;
            event.map(|event| (event, state))
        },
    ))
}

struct OpenAiRuntimeFallbackStreamState {
    responses_stream: Option<LlmStream>,
    chat_stream: Option<LlmStream>,
    chat_client: ChatCompletionsClient,
    provider: String,
    model: String,
    max_output_tokens: u64,
    messages: Vec<ChatMessage>,
    emitted_non_empty_delta: bool,
    fallback_used: bool,
    done: bool,
}

async fn next_openai_runtime_fallback_event(
    state: &mut OpenAiRuntimeFallbackStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if state.done {
            return None;
        }
        if let Some(stream) = state.responses_stream.as_mut() {
            match stream.next().await {
                Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                    if !delta.is_empty() {
                        state.emitted_non_empty_delta = true;
                    }
                    return Some(Ok(LlmStreamEvent::TextDelta(delta)));
                }
                Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used,
                })) => {
                    state.done = true;
                    return Some(Ok(LlmStreamEvent::Completed {
                        usage,
                        finish_reason,
                        fallback_used: fallback_used || state.fallback_used,
                    }));
                }
                Some(Err(err)) => {
                    if state.emitted_non_empty_delta
                        || !fallback::should_fallback_to_chat_after_responses_error(&err)
                    {
                        state.done = true;
                        return Some(Err(err));
                    }
                    tracing::warn!(
                        provider = state.provider.as_str(),
                        model = %state.model,
                        error_code = err.code.as_str(),
                        error_stage = err.stage.as_str(),
                        "OpenAI Responses stream failed before first delta; falling back to Chat Completions stream"
                    );
                    state.responses_stream = None;
                    match chat::chat_completions_stream(
                        &state.chat_client,
                        &state.provider,
                        &state.model,
                        state.max_output_tokens,
                        &state.messages,
                        true,
                    )
                    .await
                    {
                        Ok(stream) => {
                            state.chat_stream = Some(stream);
                            state.fallback_used = true;
                        }
                        Err(err) => {
                            state.done = true;
                            return Some(Err(err));
                        }
                    }
                    continue;
                }
                None => {
                    state.done = true;
                    return Some(Err(LlmError::provider(
                        "OpenAI Responses stream ended without completion event",
                        "stream",
                    )));
                }
            }
        }

        let Some(stream) = state.chat_stream.as_mut() else {
            state.done = true;
            return None;
        };
        match stream.next().await {
            Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                if !delta.is_empty() {
                    state.emitted_non_empty_delta = true;
                }
                return Some(Ok(LlmStreamEvent::TextDelta(delta)));
            }
            Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason,
                fallback_used,
            })) => {
                state.done = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used: fallback_used || state.fallback_used,
                }));
            }
            Some(Err(err)) => {
                state.done = true;
                return Some(Err(err));
            }
            None => {
                state.done = true;
                return Some(Err(LlmError::provider(
                    "Chat Completions fallback stream ended without completion event",
                    "stream",
                )));
            }
        }
    }
}

async fn openai_auto_chat_with_chat_fallback(
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
            reasoning_effort: req.reasoning_effort,
            messages: req.messages,
            allow_completed_response_fallback: true,
        },
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
        Err(err) if fallback::should_fallback_to_chat_after_responses_error(&err) => {
            tracing::warn!(
                provider = req.provider,
                model = %req.model,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "OpenAI Responses chat failed; falling back to Chat Completions"
            );
            chat_completions_with_stream_fallback(
                req.stream,
                req.chat_client,
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
pub(crate) fn openai_config_model(route: &ModelRoute) -> Result<String, LlmError> {
    route
        .candidates()
        .iter()
        .find_map(|model| match model.provider {
            Some(ModelProvider::OpenAi) | None => Some(model.name.clone()),
            Some(ModelProvider::DeepSeek) | Some(ModelProvider::BigModel) => None,
        })
        .ok_or_else(|| {
            LlmError::config(
                "LLM_MODEL for OpenAI provider must include openai: prefix or no prefix",
            )
        })
}

/// 决定本次请求实际使用的模型名称。
fn effective_openai_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = crate::provider::types::ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::OpenAi) | None => Ok(model.name),
        Some(ModelProvider::DeepSeek) | Some(ModelProvider::BigModel) => Err(LlmError::new(
            "bad_request",
            "non-openai-prefixed model cannot be used by OpenAI provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{StatusCode as AxumStatusCode, header},
        response::IntoResponse,
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

    #[derive(Debug)]
    struct MockOpenAiStreamState {
        responses_body: String,
        chat_body: String,
        responses_calls: usize,
        chat_calls: usize,
    }

    async fn mock_stream_responses_handler(
        State(state): State<Arc<Mutex<MockOpenAiStreamState>>>,
        _body: Body,
    ) -> impl IntoResponse {
        let mut state = state.lock().await;
        state.responses_calls += 1;
        (
            AxumStatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            state.responses_body.clone(),
        )
    }

    async fn mock_stream_chat_handler(
        State(state): State<Arc<Mutex<MockOpenAiStreamState>>>,
        _body: Body,
    ) -> impl IntoResponse {
        let mut state = state.lock().await;
        state.chat_calls += 1;
        (
            AxumStatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            state.chat_body.clone(),
        )
    }

    async fn spawn_mock_openai_stream(
        responses_body: String,
        chat_body: String,
    ) -> (String, Arc<Mutex<MockOpenAiStreamState>>) {
        let state = Arc::new(Mutex::new(MockOpenAiStreamState {
            responses_body,
            chat_body,
            responses_calls: 0,
            chat_calls: 0,
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_stream_responses_handler))
            .route("/v1/chat/completions", post(mock_stream_chat_handler))
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
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": text
                }
            }],
            "usage": {
                "prompt_tokens": 2,
                "completion_tokens": 3,
                "total_tokens": 5
            }
        })
    }

    fn provider_with_api_mode(api_mode: OpenAiApiMode) -> OpenAiProvider {
        let http_client = reqwest::Client::new();
        OpenAiProvider {
            responses_client: http_client.clone(),
            chat_client: ChatCompletionsClient::new("test-key".to_owned(), None, http_client),
            api_key: "test-key".to_owned(),
            base_url: None,
            model: "gpt-5.5".to_owned(),
            api_mode,
            stream: false,
            max_output_tokens: 1200,
        }
    }

    #[test]
    fn openai_tool_calling_protocol_requires_responses_auto_mode() {
        assert_eq!(
            provider_with_api_mode(OpenAiApiMode::Auto).tool_calling_protocol(None),
            Some(ToolCallingProtocol::OpenAiResponses)
        );
        assert_eq!(
            provider_with_api_mode(OpenAiApiMode::ChatOnly).tool_calling_protocol(None),
            None
        );
    }

    #[tokio::test]
    async fn openai_chat_uses_responses_without_chat_fallback_when_responses_succeeds() {
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
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: false,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            reasoning_effort: None,
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
    async fn openai_chat_falls_back_to_chat_completions_after_responses_http_error() {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
            responses_status: AxumStatusCode::BAD_REQUEST,
            responses_body: serde_json::json!({"error": {"message": "invalid responses schema"}}),
            chat_body: mock_chat_response("chat fallback ok"),
            responses_calls: 0,
            chat_calls: 0,
            chat_requests: Vec::new(),
        })
        .await;
        let http_client = reqwest::Client::new();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());
        let messages = [
            ChatMessage::system("system"),
            ChatMessage {
                role: crate::provider::types::ChatRole::Assistant,
                content: "old reply".to_owned(),
            },
            ChatMessage::user("again"),
        ];

        let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: false,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            reasoning_effort: None,
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

    #[tokio::test]
    async fn openai_chat_only_uses_chat_completions_without_responses() {
        let (base_url, state) = spawn_mock_openai(MockOpenAiState {
            responses_status: AxumStatusCode::INTERNAL_SERVER_ERROR,
            responses_body: serde_json::json!({"error": {"message": "responses should not be called"}}),
            chat_body: mock_chat_response("chat only ok"),
            responses_calls: 0,
            chat_calls: 0,
            chat_requests: Vec::new(),
        })
        .await;
        let http_client = reqwest::Client::new();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::ChatOnly,
            stream: false,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages: &[ChatMessage::user("hi")],
        })
        .await
        .unwrap();

        assert_eq!(outcome.reply, "chat only ok");
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 0);
        assert_eq!(state.chat_calls, 1);
    }

    #[tokio::test]
    async fn openai_responses_stream_falls_back_before_first_delta() {
        let (base_url, state) = spawn_mock_openai_stream(
            "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"responses unavailable\"}}}\n\n"
                .to_owned(),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"chat fallback\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        )
        .await;
        let http_client = reqwest::Client::new();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let stream = openai_auto_stream_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: true,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages: &[ChatMessage::user("hi")],
        })
        .await
        .unwrap();
        let outcome = crate::provider::collect_llm_stream(stream, "openai", "gpt-5.5")
            .await
            .unwrap();

        assert_eq!(outcome.reply, "chat fallback");
        assert!(outcome.fallback_used);
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1);
        assert_eq!(state.chat_calls, 1);
    }

    #[tokio::test]
    async fn openai_responses_stream_does_not_fallback_after_delta() {
        let (base_url, state) = spawn_mock_openai_stream(
            concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
                "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"broken\"}}}\n\n",
            )
            .to_owned(),
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"must not append\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        )
        .await;
        let http_client = reqwest::Client::new();
        let chat_client =
            ChatCompletionsClient::new("test-key", Some(&base_url), http_client.clone());

        let stream = openai_auto_stream_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: true,
            responses_client: &http_client,
            chat_client: &chat_client,
            api_key: "test-key",
            base_url: Some(&base_url),
            provider: "openai",
            model: "gpt-5.5",
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages: &[ChatMessage::user("hi")],
        })
        .await
        .unwrap();
        let err = crate::provider::collect_llm_stream(stream, "openai", "gpt-5.5")
            .await
            .unwrap_err();

        assert_eq!(err.stage, "sse");
        let state = state.lock().await;
        assert_eq!(state.responses_calls, 1);
        assert_eq!(state.chat_calls, 0);
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
