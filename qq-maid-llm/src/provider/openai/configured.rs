//! 配置驱动的 OpenAI Responses provider。
//!
//! 这里只负责把自定义 Provider ID、连接和认证元数据装配到既有 Responses
//! 请求、SSE 与 Function Tool Calling 实现，不维护供应商或模型名单。

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{OpenAiApiMode, OpenAiResponsesProviderConfig},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol, outcome_to_stream,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

use super::{
    ChatCompletionsClient, OpenAiChatFallbackRequest, openai_chat_with_chat_fallback,
    openai_stream_with_chat_fallback, responses, tool_loop,
};

/// 使用配置文件连接元数据的 Responses provider。
pub(crate) struct ConfiguredResponsesProvider {
    id: ModelProvider,
    name: String,
    responses_client: reqwest::Client,
    chat_client: ChatCompletionsClient,
    api_key: String,
    base_url: String,
    auth: crate::config::HttpAuthConfig,
    model: String,
    stream: bool,
    chat_fallback: bool,
    media_max_bytes: u64,
    max_output_tokens: u64,
}

impl ConfiguredResponsesProvider {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: &OpenAiResponsesProviderConfig,
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
        let responses_client = qq_maid_common::http_client::try_builder()
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
        let chat_client = ChatCompletionsClient::new(
            api_key.clone(),
            Some(config.base_url.as_str()),
            responses_client.clone(),
        )
        .with_auth(config.auth.clone());

        Ok(Self {
            id: config.id.clone(),
            name: config.id.as_str().to_owned(),
            responses_client,
            chat_client,
            api_key,
            base_url: config.base_url.clone(),
            auth: config.auth.clone(),
            model: default_model,
            stream,
            chat_fallback: config.chat_fallback,
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

    fn responses_request<'a>(
        &'a self,
        req: &'a ChatRequest,
        model: &'a str,
        stream: bool,
    ) -> responses::OpenAiResponsesChatRequest<'a> {
        responses::OpenAiResponsesChatRequest {
            stream,
            client: &self.responses_client,
            api_key: &self.api_key,
            base_url: Some(&self.base_url),
            auth: Some(&self.auth),
            provider: &self.name,
            model,
            media_max_bytes: self.media_max_bytes,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
            allow_completed_response_fallback: true,
            // 配置驱动 Provider 首版只开放客户端 Function Tool Calling。
            image_generation_enabled: false,
        }
    }

    fn fallback_request<'a>(
        &'a self,
        req: &'a ChatRequest,
        model: &'a str,
    ) -> OpenAiChatFallbackRequest<'a> {
        OpenAiChatFallbackRequest {
            api_mode: OpenAiApiMode::Auto,
            stream: self.stream,
            responses_client: &self.responses_client,
            chat_client: &self.chat_client,
            api_key: &self.api_key,
            base_url: Some(&self.base_url),
            responses_auth: Some(&self.auth),
            provider: &self.name,
            model,
            media_max_bytes: self.media_max_bytes,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
            image_generation_enabled: false,
        }
    }
}

#[async_trait]
impl LlmProvider for ConfiguredResponsesProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let model = self.effective_model(req.model.as_deref())?;
        if self.chat_fallback {
            return openai_chat_with_chat_fallback(self.fallback_request(&req, &model)).await;
        }
        responses::openai_responses_chat_with_stream_fallback(self.responses_request(
            &req,
            &model,
            self.stream,
        ))
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let model = self.effective_model(req.model.as_deref())?;
        if self.chat_fallback {
            return openai_stream_with_chat_fallback(self.fallback_request(&req, &model)).await;
        }
        if !self.stream {
            let outcome = responses::openai_responses_chat_with_stream_fallback(
                self.responses_request(&req, &model, false),
            )
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        responses::openai_responses_chat_stream(&self.responses_request(&req, &model, true)).await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        let model = self.effective_model(req.chat.model.as_deref())?;
        Ok(Some(Box::new(
            tool_loop::ResponsesAgentSession::new_configured(
                self.responses_client.clone(),
                self.api_key.clone(),
                Some(self.base_url.clone()),
                Some(self.auth.clone()),
                &self.name,
                model,
                self.media_max_bytes,
                req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
                req.chat.reasoning_effort,
                &req.chat.messages,
                req.tools,
                req.chat.context_budget,
                false,
            )?,
        )))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.effective_model(model)
            .ok()
            .map(|_| ToolCallingProtocol::OpenAiResponses)
    }

    fn supports_vision(&self, model: Option<&str>) -> bool {
        // Responses 的 input_image 编码由公共 payload 负责；不在本地猜测具体模型能力。
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
    use std::{collections::VecDeque, sync::Arc};

    use axum::{
        Router,
        body::Body,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use qq_maid_common::input_part::{MessageInputPart, MessageMedia};
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::Mutex};

    use crate::{
        config::{HttpAuthConfig, OpenAiCompatibleProviderConfig, OpenAiResponsesProviderConfig},
        provider::{
            LlmProvider, ToolChatRequest, collect_llm_stream,
            openai_compatible::OpenAiCompatibleProvider,
            routing::ModelRouteProvider,
            test_support::{WeatherToolStub, test_tool_context},
            types::{ChatMessage, ChatRequest, ModelProvider, ModelRoute},
        },
        tool::ToolRegistry,
    };

    use super::ConfiguredResponsesProvider;

    #[derive(Clone)]
    struct MockReply {
        status: StatusCode,
        content_type: &'static str,
        body: String,
    }

    #[derive(Default)]
    struct MockState {
        replies: VecDeque<MockReply>,
        requests: Vec<CapturedRequest>,
    }

    struct CapturedRequest {
        path: String,
        authorization: Option<String>,
        accept: Option<String>,
        body: Value,
    }

    async fn handler(
        State(state): State<Arc<Mutex<MockState>>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let mut state = state.lock().await;
        state.requests.push(CapturedRequest {
            path: uri.path().to_owned(),
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            accept: headers
                .get(header::ACCEPT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body: serde_json::from_slice(&bytes).unwrap(),
        });
        let reply = state.replies.pop_front().unwrap();
        (
            reply.status,
            [(header::CONTENT_TYPE, reply.content_type)],
            reply.body,
        )
    }

    async fn spawn_mock(replies: Vec<MockReply>) -> (String, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState {
            replies: replies.into(),
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/responses", post(handler))
            .route("/v1/chat/completions", post(handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1"), state)
    }

    fn provider_config_for(
        id: &str,
        base_url: String,
        chat_fallback: bool,
    ) -> OpenAiResponsesProviderConfig {
        OpenAiResponsesProviderConfig {
            id: ModelProvider::Custom(id.to_owned()),
            base_url,
            api_key_env: "OPENCODE_API_KEY".to_owned(),
            api_key: Some("shared-key".to_owned()),
            auth: HttpAuthConfig::default(),
            request_timeout_seconds: Some(5),
            chat_fallback,
        }
    }

    fn provider_config(base_url: String, chat_fallback: bool) -> OpenAiResponsesProviderConfig {
        provider_config_for("opencode_zen", base_url, chat_fallback)
    }

    fn request(model: &str) -> ChatRequest {
        ChatRequest {
            session_id: "test-session".to_owned(),
            model: Some(model.to_owned()),
            messages: vec![ChatMessage::user("你好")],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: Default::default(),
        }
    }

    fn image_request(model: &str) -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage::user_with_parts(
                "看图",
                vec![
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/png".to_owned()),
                        url: Some("https://example.test/image.png".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            ..request(model)
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
            ..request(model)
        }
    }

    #[tokio::test]
    async fn configured_responses_uses_provider_url_bearer_auth_and_raw_model() {
        let (base_url, state) = spawn_mock(vec![MockReply {
            status: StatusCode::OK,
            content_type: "application/json",
            body: json!({"output_text": "Zen ok"}).to_string(),
        }])
        .await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "default-model".to_owned(),
            false,
            90,
            1024,
            1200,
        )
        .unwrap();

        let outcome = provider
            .chat(image_request("opencode_zen:gpt-test"))
            .await
            .unwrap();

        assert_eq!(outcome.reply, "Zen ok");
        assert_eq!(outcome.metrics.provider, "opencode_zen");
        assert!(provider.supports_vision(Some("opencode_zen:gpt-test")));
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests[0].path, "/v1/responses");
        assert_eq!(
            state.requests[0].authorization.as_deref(),
            Some("Bearer shared-key")
        );
        assert_eq!(state.requests[0].body["model"], "gpt-test");
        assert_eq!(
            state.requests[0].body["input"][0]["content"],
            json!([
                {"type": "input_text", "text": "看图"},
                {"type": "input_image", "image_url": "https://example.test/image.png"}
            ])
        );
    }

    #[tokio::test]
    async fn configured_responses_stream_reuses_responses_sse_parser() {
        let (base_url, state) = spawn_mock(vec![MockReply {
            status: StatusCode::OK,
            content_type: "text/event-stream",
            body: concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你\"}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"好\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"你好\"}}\n\n",
            )
            .to_owned(),
        }])
        .await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "gpt-test".to_owned(),
            true,
            90,
            1024,
            1200,
        )
        .unwrap();

        let stream = provider
            .stream_chat(image_request("opencode_zen:gpt-test"))
            .await
            .unwrap();
        let outcome = collect_llm_stream(stream, "opencode_zen", "gpt-test")
            .await
            .unwrap();

        assert_eq!(outcome.reply, "你好");
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests[0].path, "/v1/responses");
        assert_eq!(
            state.requests[0].accept.as_deref(),
            Some("text/event-stream")
        );
        assert_eq!(
            state.requests[0].body["input"][0]["content"][1]["type"],
            "input_image"
        );
    }

    #[tokio::test]
    async fn disabled_chat_fallback_never_requests_chat_completions() {
        let (base_url, state) = spawn_mock(vec![MockReply {
            status: StatusCode::BAD_REQUEST,
            content_type: "application/json",
            body: json!({"error": {"message": "model requires /messages"}}).to_string(),
        }])
        .await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "gpt-test".to_owned(),
            false,
            90,
            1024,
            1200,
        )
        .unwrap();

        let error = provider
            .chat(request("opencode_zen:messages-only-model"))
            .await
            .unwrap_err();

        assert!(error.message.contains("opencode_zen"));
        assert!(error.message.contains("model requires /messages"));
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 1);
        assert_eq!(state.requests[0].path, "/v1/responses");
    }

    #[tokio::test]
    async fn responses_failure_uses_next_route_candidate_without_same_provider_chat_request() {
        let (base_url, state) = spawn_mock(vec![
            MockReply {
                status: StatusCode::BAD_GATEWAY,
                content_type: "application/json",
                body: json!({"error": {"message": "Zen unavailable"}}).to_string(),
            },
            MockReply {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({"choices": [{"message": {"content": "Go fallback"}}]}).to_string(),
            },
        ])
        .await;
        let zen_id = ModelProvider::Custom("opencode_zen".to_owned());
        let go_id = ModelProvider::Custom("opencode_go".to_owned());
        let zen = Arc::new(
            ConfiguredResponsesProvider::new(
                &provider_config(base_url.clone(), false),
                "gpt-test".to_owned(),
                false,
                90,
                1024,
                1200,
            )
            .unwrap(),
        );
        let go = Arc::new(
            OpenAiCompatibleProvider::new(
                &OpenAiCompatibleProviderConfig {
                    id: go_id.clone(),
                    base_url,
                    api_key_env: "OPENCODE_API_KEY".to_owned(),
                    api_key: Some("shared-key".to_owned()),
                    auth: HttpAuthConfig::default(),
                    request_timeout_seconds: None,
                },
                "kimi-test".to_owned(),
                false,
                90,
                1024,
                1200,
            )
            .unwrap(),
        );
        let route = ModelRoute::parse_config("opencode_zen:gpt-test,opencode_go:kimi-test", "test")
            .unwrap();
        let provider = ModelRouteProvider::new(
            "auto",
            ModelProvider::OpenAi,
            route,
            vec![(zen_id, zen), (go_id, go)],
        )
        .unwrap();

        let outcome = provider
            .chat(request("opencode_zen:gpt-test,opencode_go:kimi-test"))
            .await
            .unwrap();

        assert_eq!(outcome.reply, "Go fallback");
        assert!(outcome.fallback_used);
        let state = state.lock().await;
        assert_eq!(
            state
                .requests
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            ["/v1/responses", "/v1/chat/completions"]
        );
    }

    #[tokio::test]
    async fn same_model_name_falls_back_between_distinct_provider_base_urls() {
        let (routera_url, routera_state) = spawn_mock(vec![MockReply {
            status: StatusCode::BAD_GATEWAY,
            content_type: "application/json",
            body: json!({"error": {"message": "router A unavailable"}}).to_string(),
        }])
        .await;
        let (routerb_url, routerb_state) = spawn_mock(vec![MockReply {
            status: StatusCode::OK,
            content_type: "application/json",
            body: json!({"output_text": "router B reply"}).to_string(),
        }])
        .await;
        let routera_id = ModelProvider::Custom("routera".to_owned());
        let routerb_id = ModelProvider::Custom("routerb".to_owned());
        let routera = Arc::new(
            ConfiguredResponsesProvider::new(
                &provider_config_for("routera", routera_url, false),
                "gpt-5.6-luna".to_owned(),
                false,
                90,
                1024,
                1200,
            )
            .unwrap(),
        );
        let routerb = Arc::new(
            ConfiguredResponsesProvider::new(
                &provider_config_for("routerb", routerb_url, false),
                "gpt-5.6-luna".to_owned(),
                false,
                90,
                1024,
                1200,
            )
            .unwrap(),
        );
        let route =
            ModelRoute::parse_config("routera:gpt-5.6-luna,routerb:gpt-5.6-luna", "test").unwrap();
        let provider = ModelRouteProvider::new(
            "auto",
            ModelProvider::OpenAi,
            route,
            vec![(routera_id, routera), (routerb_id, routerb)],
        )
        .unwrap();

        let outcome = provider
            .chat(request("routera:gpt-5.6-luna,routerb:gpt-5.6-luna"))
            .await
            .unwrap();

        assert_eq!(outcome.reply, "router B reply");
        assert!(outcome.fallback_used);
        for state in [routera_state, routerb_state] {
            let state = state.lock().await;
            assert_eq!(state.requests.len(), 1);
            assert_eq!(state.requests[0].path, "/v1/responses");
            assert_eq!(state.requests[0].body["model"], "gpt-5.6-luna");
        }
    }

    #[tokio::test]
    async fn configured_responses_rejects_wrong_provider_prefix_before_request() {
        let (base_url, state) = spawn_mock(Vec::new()).await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "gpt-test".to_owned(),
            false,
            90,
            1024,
            1200,
        )
        .unwrap();

        let error = provider
            .chat(request("opencode_go:gpt-test"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "bad_request");
        assert!(state.lock().await.requests.is_empty());
    }

    #[tokio::test]
    async fn configured_responses_executes_function_tool_calling() {
        let (base_url, state) = spawn_mock(vec![
            MockReply {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({
                    "output": [{
                        "type": "function_call",
                        "call_id": "call-weather",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"杭州\"}"
                    }]
                })
                .to_string(),
            },
            MockReply {
                status: StatusCode::OK,
                content_type: "application/json",
                body: json!({"output_text": "建议带伞"}).to_string(),
            },
        ])
        .await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "gpt-test".to_owned(),
            false,
            90,
            1024,
            1200,
        )
        .unwrap();
        let tools = ToolRegistry::new()
            .register(WeatherToolStub::new("小雨"))
            .unwrap();

        let outcome = provider
            .chat_with_tools(ToolChatRequest {
                chat: image_request("opencode_zen:gpt-test"),
                tools,
                tool_context: test_tool_context(),
                max_rounds: 3,
                progress_sink: None,
                final_delta_sink: None,
                run_handle: None,
            })
            .await
            .unwrap();

        assert_eq!(outcome.reply, "建议带伞");
        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        assert!(state.requests[0].body.get("tools").is_some());
        assert_eq!(
            state.requests[0].body["input"][0]["content"][1]["type"],
            "input_image"
        );
        assert!(
            state.requests[1].body["input"]
                .as_array()
                .is_some_and(|input| input.iter().any(|item| {
                    item["type"] == "function_call_output" && item["call_id"] == "call-weather"
                }))
        );
    }

    #[tokio::test]
    async fn configured_responses_still_rejects_file_input_before_request() {
        let (base_url, state) = spawn_mock(Vec::new()).await;
        let provider = ConfiguredResponsesProvider::new(
            &provider_config(base_url, false),
            "gpt-test".to_owned(),
            false,
            90,
            1024,
            1200,
        )
        .unwrap();

        let error = provider
            .chat(file_request("opencode_zen:gpt-test"))
            .await
            .unwrap_err();

        assert_eq!(error.code, "unsupported_input_part");
        assert!(error.message.contains("文件"));
        assert!(state.lock().await.requests.is_empty());
    }
}
