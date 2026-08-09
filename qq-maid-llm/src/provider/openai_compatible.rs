//! 配置驱动的 OpenAI-compatible Chat Completions provider。
//!
//! MiMo、OpenRouter、火山方舟等只要暴露 `/chat/completions` 兼容端点，就可以通过
//! provider registry 复用这一层；本模块不包含任何具体模型名或供应商专用分支。

use std::time::Duration;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::OpenAiCompatibleProviderConfig,
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
use async_trait::async_trait;

/// 配置驱动的 OpenAI-compatible provider。
pub struct OpenAiCompatibleProvider {
    id: ModelProvider,
    name: String,
    client: ChatCompletionsClient,
    /// 默认模型仅用于无请求级模型覆盖的固定 provider 场景。
    model: String,
    stream: bool,
    media_max_bytes: u64,
    max_output_tokens: u64,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        config: &OpenAiCompatibleProviderConfig,
        default_model: String,
        stream: bool,
        request_timeout_seconds: u64,
        media_max_bytes: u64,
        max_output_tokens: u64,
    ) -> Result<Self, LlmError> {
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| LlmError::config(format!("{} is required", config.api_key_env)))?;
        let timeout = config
            .request_timeout_seconds
            .unwrap_or(request_timeout_seconds);
        let http_client = qq_maid_common::http_client::try_builder()
            .map_err(|err| {
                LlmError::config(format!(
                    "failed to configure {} TLS: {err}",
                    config.id.as_str()
                ))
            })?
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
            media_max_bytes,
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
            self.media_max_bytes,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
        )
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let effective_model = self.effective_model(req.model.as_deref())?;
        if !self.stream {
            let outcome = chat_completions_with_stream_fallback(
                false,
                &self.client,
                self.name(),
                &effective_model,
                self.media_max_bytes,
                req.max_output_tokens.unwrap_or(self.max_output_tokens),
                &req.messages,
            )
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        let stream = chat_completions_stream(
            &self.client,
            self.name(),
            &effective_model,
            self.media_max_bytes,
            req.max_output_tokens.unwrap_or(self.max_output_tokens),
            &req.messages,
            true,
        )
        .await?;
        Ok(stream)
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
            self.media_max_bytes,
            req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
            |value, _| self.effective_model(value),
        )
        .await?
        else {
            return Ok(None);
        };
        Ok(Some(session))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        provider_chat_completions_tool_calling_protocol(model, &self.model, |value, _| {
            self.effective_model(value)
        })
    }

    fn supports_vision(&self, model: Option<&str>) -> bool {
        // 是否真正支持图片由上游模型决定；这里只确认该请求属于当前 Provider，
        // 图片编码继续复用公共 Chat Completions payload，不维护模型白名单。
        self.effective_model(model).is_ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::HttpAuthConfig,
        provider::{
            ToolChatRequest, collect_llm_stream,
            test_support::{WeatherToolStub, test_tool_context},
            types::ChatMessage,
        },
        tool::ToolRegistry,
    };
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, StatusCode, Uri, header},
        response::IntoResponse,
        routing::post,
    };
    use qq_maid_common::input_part::{MessageInputPart, MessageMedia};
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
        let streaming = request["stream"].as_bool() == Some(true);
        state.paths.push(uri.path().to_owned());
        state.auth_headers.push(auth);
        state.api_key_headers.push(api_key);
        state.requests.push(request);
        let (content_type, response_body) = if state.status.is_success() && streaming {
            (
                "text/event-stream",
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"mimo reply\"}}]}\n\n",
                    "data: [DONE]\n\n"
                )
                .to_owned(),
            )
        } else if state.status.is_success() {
            (
                "application/json",
                json!({"choices": [{"message": {"content": "mimo reply"}}]}).to_string(),
            )
        } else {
            (
                "application/json",
                json!({"error": {"message": "unauthorized"}}).to_string(),
            )
        };
        (
            state.status,
            [(header::CONTENT_TYPE, content_type)],
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

    async fn spawn_opencode_chat_mock() -> (String, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState {
            status: StatusCode::OK,
            ..MockState::default()
        }));
        let app = Router::new()
            .route("/zen/v1/chat/completions", post(mock_chat_handler))
            .route("/zen/go/v1/chat/completions", post(mock_chat_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), state)
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

    fn image_request(model: &str) -> ChatRequest {
        ChatRequest {
            session_id: "s".to_owned(),
            model: Some(model.to_owned()),
            messages: vec![ChatMessage::user_with_parts(
                "看图",
                vec![
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        url: Some("https://example.test/image.jpg".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: Default::default(),
        }
    }

    fn file_request(model: &str) -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage::user_with_parts(
                "读文件",
                vec![MessageInputPart::file(MessageMedia {
                    mime_type: Some("application/pdf".to_owned()),
                    url: Some("https://example.test/document.pdf".to_owned()),
                    ..Default::default()
                })],
            )],
            ..image_request(model)
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
            10 * 1024 * 1024,
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
    async fn custom_provider_chat_sends_text_and_image_with_common_payload() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            10 * 1024 * 1024,
            1200,
        )
        .unwrap();

        let outcome = provider
            .chat(image_request("mimo:mimo-v2.5-pro"))
            .await
            .unwrap();

        assert_eq!(outcome.reply, "mimo reply");
        assert!(provider.supports_vision(Some("mimo:mimo-v2.5-pro")));
        let state = state.lock().await;
        let content = state.requests[0]["messages"][0]["content"]
            .as_array()
            .unwrap();
        assert_eq!(content[0], json!({"type": "text", "text": "看图"}));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "https://example.test/image.jpg"
        );
    }

    #[tokio::test]
    async fn custom_provider_stream_sends_text_and_image_with_common_payload() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            true,
            90,
            10 * 1024 * 1024,
            1200,
        )
        .unwrap();

        let stream = provider
            .stream_chat(image_request("mimo:mimo-v2.5-pro"))
            .await
            .unwrap();
        let outcome = collect_llm_stream(stream, "mimo", "mimo-v2.5-pro")
            .await
            .unwrap();

        assert_eq!(outcome.reply, "mimo reply");
        let state = state.lock().await;
        assert_eq!(state.requests[0]["stream"], true);
        assert_eq!(
            state.requests[0]["messages"][0]["content"][1]["type"],
            "image_url"
        );
    }

    #[tokio::test]
    async fn custom_provider_agent_loop_sends_text_and_image_with_common_payload() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            10 * 1024 * 1024,
            1200,
        )
        .unwrap();
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("晴"))
            .unwrap();

        let outcome = provider
            .chat_with_tools(ToolChatRequest {
                chat: image_request("mimo:mimo-v2.5-pro"),
                tools,
                tool_context: test_tool_context(),
                max_rounds: 2,
                progress_sink: None,
                final_delta_sink: None,
                run_handle: None,
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "mimo reply");
        let state = state.lock().await;
        assert!(state.requests[0].get("tools").is_some());
        assert_eq!(
            state.requests[0]["messages"][0]["content"][1]["type"],
            "image_url"
        );
    }

    #[tokio::test]
    async fn custom_provider_still_rejects_file_input_before_request() {
        let (base_url, state) = spawn_mock_chat(StatusCode::OK).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            10 * 1024 * 1024,
            1200,
        )
        .unwrap();

        let error = provider
            .chat(file_request("mimo:mimo-v2.5-pro"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "unsupported_input_part");
        assert!(error.message.contains("文件"));
        assert!(state.lock().await.requests.is_empty());
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
            10 * 1024 * 1024,
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
    async fn custom_provider_keeps_auth_failure_classification() {
        let (base_url, _state) = spawn_mock_chat(StatusCode::UNAUTHORIZED).await;
        let provider = OpenAiCompatibleProvider::new(
            &mimo_config(base_url),
            "mimo-v2.5".to_owned(),
            false,
            90,
            10 * 1024 * 1024,
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

        assert_eq!(err.code, "authentication_failed");
        assert_eq!(err.kind(), crate::error::LlmErrorKind::Authentication);
        assert_eq!(err.upstream_status, Some(401));
        assert!(err.message.contains("HTTP 401"));
    }

    #[tokio::test]
    async fn opencode_zen_and_go_chat_use_distinct_urls_raw_models_and_shared_key() {
        let (root, state) = spawn_opencode_chat_mock().await;
        for (id, base_path, model) in [
            ("opencode_zen_chat", "/zen/v1", "deepseek-test"),
            ("opencode_go", "/zen/go/v1", "kimi-test"),
        ] {
            let provider = OpenAiCompatibleProvider::new(
                &OpenAiCompatibleProviderConfig {
                    id: ModelProvider::Custom(id.to_owned()),
                    base_url: format!("{root}{base_path}"),
                    api_key_env: "OPENCODE_API_KEY".to_owned(),
                    api_key: Some("shared-opencode-key".to_owned()),
                    auth: HttpAuthConfig::default(),
                    request_timeout_seconds: None,
                },
                model.to_owned(),
                false,
                90,
                10 * 1024 * 1024,
                1200,
            )
            .unwrap();
            provider
                .chat(ChatRequest {
                    session_id: "s".to_owned(),
                    model: Some(format!("{id}:{model}")),
                    messages: vec![ChatMessage::user("hi")],
                    context_budget: None,
                    max_output_tokens: None,
                    reasoning_effort: None,
                    metadata: Default::default(),
                })
                .await
                .unwrap();
        }

        let state = state.lock().await;
        assert_eq!(
            state.paths,
            vec!["/zen/v1/chat/completions", "/zen/go/v1/chat/completions"]
        );
        assert_eq!(state.requests[0]["model"], "deepseek-test");
        assert_eq!(state.requests[1]["model"], "kimi-test");
        assert_eq!(
            state.auth_headers,
            vec![
                Some("Bearer shared-opencode-key".to_owned()),
                Some("Bearer shared-opencode-key".to_owned())
            ]
        );
    }
}
