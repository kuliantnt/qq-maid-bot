//! 配置驱动的 OpenAI-compatible Chat Completions provider。
//!
//! MiMo、OpenRouter、火山方舟等只要暴露 `/chat/completions` 兼容端点，就可以通过
//! provider registry 复用这一层；本模块不包含任何具体模型名或供应商专用分支。

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStep, AgentStepSession, AgentToolResult},
    config::OpenAiCompatibleProviderConfig,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol,
        openai::{
            ChatCompletionsClient, begin_chat_completions_session, chat_completions_stream,
            chat_completions_with_stream_fallback, provider_chat_completions_tool_calling_protocol,
        },
        outcome_to_stream,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

/// 配置驱动的 OpenAI-compatible provider。
pub struct OpenAiCompatibleProvider {
    id: ModelProvider,
    name: String,
    client: ChatCompletionsClient,
    /// 默认模型仅用于无请求级模型覆盖的固定 provider 场景。
    model: String,
    stream: bool,
    max_output_tokens: u64,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        config: &OpenAiCompatibleProviderConfig,
        default_model: String,
        stream: bool,
        request_timeout_seconds: u64,
        max_output_tokens: u64,
    ) -> Result<Self, LlmError> {
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| LlmError::config(format!("{} is required", config.api_key_env)))?;
        let timeout = config
            .request_timeout_seconds
            .unwrap_or(request_timeout_seconds);
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .build()
            .map_err(|err| {
                LlmError::config(format!(
                    "failed to build {} HTTP client: {err}",
                    config.id.as_str()
                ))
            })?;
        let client =
            ChatCompletionsClient::new(api_key, Some(config.base_url.as_str()), http_client)
                .with_auth(config.auth.clone());

        Ok(Self {
            id: config.id.clone(),
            name: config.id.as_str().to_owned(),
            client,
            model: default_model,
            stream,
            max_output_tokens,
        })
    }

    fn effective_model(&self, override_model: Option<&str>) -> Result<String, LlmError> {
        let Some(value) = override_model else {
            return Ok(self.model.clone());
        };
        let model = ModelId::parse(value, "request")?;
        match model.provider {
            Some(provider) if provider == self.id => Ok(model.name),
            None => Ok(model.name),
            Some(provider) => Err(LlmError::new(
                "bad_request",
                format!(
                    "model prefix `{}` cannot be used by `{}` provider",
                    provider.as_str(),
                    self.id.as_str()
                ),
                "request",
            )),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = self.effective_model(req.model.as_deref())?;
        chat_completions_with_stream_fallback(
            self.stream,
            &self.client,
            self.name(),
            &effective_model,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
        )
        .await
        .map_err(map_auth_error_to_unavailable)
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let effective_model = self.effective_model(req.model.as_deref())?;
        if !self.stream {
            let outcome = chat_completions_with_stream_fallback(
                false,
                &self.client,
                self.name(),
                &effective_model,
                req.max_output_tokens.unwrap_or(self.max_output_tokens),
                &req.messages,
            )
            .await
            .map_err(map_auth_error_to_unavailable)?;
            return Ok(outcome_to_stream(outcome));
        }
        let stream = chat_completions_stream(
            &self.client,
            self.name(),
            &effective_model,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
            true,
        )
        .await
        .map_err(map_auth_error_to_unavailable)?;
        Ok(map_stream_errors(stream))
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
        let Some(session) = begin_chat_completions_session(
            req,
            self.client.clone(),
            self.name(),
            &self.model,
            req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
            |value, _| self.effective_model(value),
        )
        .await
        .map_err(map_auth_error_to_unavailable)?
        else {
            return Ok(None);
        };
        Ok(Some(Box::new(AuthMappingAgentSession { inner: session })))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        provider_chat_completions_tool_calling_protocol(model, &self.model, |value, _| {
            self.effective_model(value)
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

struct AuthMappingAgentSession {
    inner: Box<dyn AgentStepSession + Send>,
}

#[async_trait]
impl AgentStepSession for AuthMappingAgentSession {
    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        self.inner
            .advance(results, allow_tool_calls)
            .await
            .map_err(map_auth_error_to_unavailable)
    }
}

fn map_stream_errors(stream: LlmStream) -> LlmStream {
    Box::pin(stream.map(|event: Result<LlmStreamEvent, LlmError>| {
        event.map_err(map_auth_error_to_unavailable)
    }))
}

fn map_auth_error_to_unavailable(err: LlmError) -> LlmError {
    if err.code == "config"
        && (err.message.contains("HTTP 401") || err.message.contains("HTTP 403"))
    {
        return LlmError::provider(err.message, "provider_unavailable");
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::HttpAuthConfig, provider::types::ChatMessage};
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, StatusCode, Uri, header},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockState {
        status: StatusCode,
        paths: Vec<String>,
        auth_headers: Vec<Option<String>>,
        api_key_headers: Vec<Option<String>>,
        requests: Vec<Value>,
    }

    impl Default for MockState {
        fn default() -> Self {
            Self {
                status: StatusCode::OK,
                paths: Vec::new(),
                auth_headers: Vec::new(),
                api_key_headers: Vec::new(),
                requests: Vec::new(),
            }
        }
    }

    async fn mock_chat_handler(
        State(state): State<Arc<Mutex<MockState>>>,
        uri: Uri,
        headers: HeaderMap,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let api_key = headers
            .get("api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut state = state.lock().await;
        state.paths.push(uri.path().to_owned());
        state.auth_headers.push(auth);
        state.api_key_headers.push(api_key);
        state.requests.push(request);
        let response_body = if state.status.is_success() {
            json!({"choices": [{"message": {"content": "mimo reply"}}]}).to_string()
        } else {
            json!({"error": {"message": "unauthorized"}}).to_string()
        };
        (
            state.status,
            [(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            )],
            response_body,
        )
    }

    async fn spawn_mock_chat(status: StatusCode) -> (String, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState {
            status,
            ..MockState::default()
        }));
        let app = Router::new()
            .route("/v1/chat/completions", post(mock_chat_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/"), state)
    }

    fn mimo_config(base_url: String) -> OpenAiCompatibleProviderConfig {
        OpenAiCompatibleProviderConfig {
            id: ModelProvider::Custom("mimo".to_owned()),
            base_url,
            api_key_env: "MIMO_API_KEY".to_owned(),
            api_key: Some("test-mimo-key".to_owned()),
            auth: HttpAuthConfig::default(),
            request_timeout_seconds: None,
        }
    }

    #[tokio::test]
    async fn custom_provider_uses_chat_completions_endpoint_header_and_model() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            1200,
        )
        .unwrap();

        let outcome = provider
            .chat(ChatRequest {
                session_id: "s".to_owned(),
                model: Some("mimo:mimo-v2.5-pro".to_owned()),
                messages: vec![ChatMessage::user("hi")],
                context_budget: None,
                max_output_tokens: None,
                reasoning_effort: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "mimo reply");
        let state = state.lock().await;
        assert_eq!(state.paths, vec!["/v1/chat/completions"]);
        assert_eq!(
            state.auth_headers,
            vec![Some("Bearer test-mimo-key".to_owned())]
        );
        assert_eq!(state.requests[0]["model"], "mimo-v2.5-pro");
        assert!(state.requests[0].get("stream").is_none());
    }

    #[tokio::test]
    async fn custom_provider_supports_non_authorization_api_key_header() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &OpenAiCompatibleProviderConfig {
                id: ModelProvider::Custom("mimo".to_owned()),
                base_url,
                api_key_env: "MIMO_API_KEY".to_owned(),
                api_key: Some("test-key".to_owned()),
                auth: HttpAuthConfig {
                    header: "api-key".to_owned(),
                    scheme: None,
                },
                request_timeout_seconds: None,
            },
            "mimo-v2.5".to_owned(),
            false,
            90,
            1200,
        )
        .unwrap();

        provider
            .chat(ChatRequest {
                session_id: "s".to_owned(),
                model: Some("mimo:mimo-v2.5-pro".to_owned()),
                messages: vec![ChatMessage::user("hi")],
                context_budget: None,
                max_output_tokens: None,
                reasoning_effort: None,
                metadata: Default::default(),
            })
            .await
            .unwrap();

        let state = state.lock().await;
        assert_eq!(state.auth_headers, vec![None]);
        assert_eq!(state.api_key_headers, vec![Some("test-key".to_owned())]);
    }

    #[tokio::test]
    async fn custom_provider_maps_auth_failure_to_candidate_unavailable() {
        let (base_url, _state) = spawn_mock_chat(StatusCode::UNAUTHORIZED).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            1200,
        )
        .unwrap();

        let err = provider
            .chat(ChatRequest {
                session_id: "s".to_owned(),
                model: Some("mimo:mimo-v2.5-pro".to_owned()),
                messages: vec![ChatMessage::user("hi")],
                context_budget: None,
                max_output_tokens: None,
                reasoning_effort: None,
                metadata: Default::default(),
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "provider_error");
        assert_eq!(err.stage, "provider_unavailable");
        assert!(err.message.contains("HTTP 401"));
    }
}
