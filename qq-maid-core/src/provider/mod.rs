//! LLM 提供商抽象层。
//!
//! 定义了统一的 [`LlmProvider`] trait，屏蔽不同 LLM API（OpenAI、DeepSeek）的差异。
//! 同时提供通用模型候选链路由逻辑，以及 [`ChatOutcome`] 等通用类型。

pub mod deepseek;
pub mod openai;
pub mod status;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    config::{AppConfig, ProviderMode},
    error::LlmError,
    provider::types::{ChatRequest, ModelId, ModelProvider, ModelRoute, TokenUsage},
    util::metrics::LlmMetrics,
};

/// LLM 调用的最终输出结果。
#[derive(Debug, Clone)]
pub struct ChatOutcome {
    /// 模型返回的文本回复。
    pub reply: String,
    /// 本次请求的指标记录（延迟、首 token 时间等）。
    pub metrics: LlmMetrics,
    /// 令牌用量统计（输入/输出/总计），部分提供商可能不返回。
    pub usage: Option<TokenUsage>,
    /// 是否因前序模型候选失败而使用了后续候选。
    pub fallback_used: bool,
}

/// LLM 提供商统一接口。
///
/// 所有后端（OpenAI、DeepSeek 等）必须实现此 trait。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 发送聊天请求并返回结果。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError>;
    /// 提供商名称，例如 "openai"、"deepseek"。
    fn name(&self) -> &'static str;
    /// 当前使用的模型名称。
    fn model(&self) -> &str;
    /// 是否启用了流式传输。
    fn stream_enabled(&self) -> bool;
}

/// 线程安全的 LLM 提供商智能指针别名。
pub type DynLlmProvider = Arc<dyn LlmProvider>;

/// 根据配置构建 LLM 提供商实例。
///
/// - `OpenAi`：仅使用 OpenAI 提供商。
/// - `DeepSeek`：仅使用 DeepSeek 提供商。
/// - `Auto`：根据模型候选链路由；单 OpenAI 主模型仍兼容原 OpenAI -> DeepSeek fallback。
pub fn build_provider(config: &AppConfig) -> Result<DynLlmProvider, LlmError> {
    match config.provider {
        ProviderMode::OpenAi => {
            for (name, route) in config.configured_model_routes()? {
                ensure_route_supported(&route, ModelProvider::OpenAi, ModelProvider::OpenAi, name)?;
            }
            let provider: DynLlmProvider = Arc::new(openai::OpenAiRigProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "openai",
                ModelProvider::OpenAi,
                config.model_route.clone(),
                vec![(ModelProvider::OpenAi, provider)],
            )?))
        }
        ProviderMode::DeepSeek => {
            for (name, route) in config.configured_model_routes()? {
                ensure_route_supported(
                    &route,
                    ModelProvider::DeepSeek,
                    ModelProvider::DeepSeek,
                    name,
                )?;
            }
            let provider: DynLlmProvider = Arc::new(deepseek::DeepSeekRigProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "deepseek",
                ModelProvider::DeepSeek,
                config.model_route.clone(),
                vec![(ModelProvider::DeepSeek, provider)],
            )?))
        }
        ProviderMode::Auto => {
            let route = auto_default_route(config)?;
            let provider_routes = auto_provider_routes(config, &route)?;
            let required_providers =
                provider_kinds_for_routes(&provider_routes, ModelProvider::OpenAi);
            let mut providers: Vec<(ModelProvider, DynLlmProvider)> = Vec::new();

            if required_providers.contains(&ModelProvider::DeepSeek) {
                ensure_deepseek_api_key_for_routes(config, &provider_routes)?;
            }

            for provider_kind in required_providers {
                match provider_kind {
                    ModelProvider::OpenAi => providers.push((
                        ModelProvider::OpenAi,
                        Arc::new(openai::OpenAiRigProvider::new(config)?),
                    )),
                    ModelProvider::DeepSeek => providers.push((
                        ModelProvider::DeepSeek,
                        Arc::new(deepseek::DeepSeekRigProvider::new(config)?),
                    )),
                }
            }

            Ok(Arc::new(ModelRouteProvider::new(
                "auto",
                ModelProvider::OpenAi,
                route,
                providers,
            )?))
        }
    }
}

/// 通用模型候选链提供商。
///
/// 先执行 OpenAI/DeepSeek 各自内部的 Responses、Chat Completions、空流补非流等
/// 兼容策略；只有某个候选整体失败且错误允许跨模型降级时，才尝试下一个候选。
struct ModelRouteProvider {
    name: &'static str,
    default_provider: ModelProvider,
    default_route: ModelRoute,
    providers: Vec<(ModelProvider, DynLlmProvider)>,
    model_display: String,
}

impl ModelRouteProvider {
    fn new(
        name: &'static str,
        default_provider: ModelProvider,
        default_route: ModelRoute,
        providers: Vec<(ModelProvider, DynLlmProvider)>,
    ) -> Result<Self, LlmError> {
        if providers.is_empty() {
            return Err(LlmError::config(
                "no LLM provider is available for model route",
            ));
        }
        let model_display = default_route.display();
        Ok(Self {
            name,
            default_provider,
            default_route,
            providers,
            model_display,
        })
    }

    fn provider_for(&self, provider: ModelProvider) -> Option<&DynLlmProvider> {
        self.providers
            .iter()
            .find(|(candidate, _)| *candidate == provider)
            .map(|(_, provider)| provider)
    }
}

#[async_trait]
impl LlmProvider for ModelRouteProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let route = match req.model.as_deref() {
            Some(value) => ModelRoute::parse(value, "request")?,
            None => self.default_route.clone(),
        };
        let task = model_task_name(&req);
        let mut failures = Vec::new();

        for (index, candidate) in route.candidates().iter().enumerate() {
            let provider_kind = candidate.provider.unwrap_or(self.default_provider);
            let provider = self.provider_for(provider_kind).ok_or_else(|| {
                LlmError::config(format!(
                    "provider `{}` is not available for model candidate `{}`",
                    provider_kind.as_str(),
                    candidate.to_request_model()
                ))
            })?;
            let mut candidate_req = req.clone();
            candidate_req.model = Some(candidate.to_request_model());

            match provider.chat(candidate_req).await {
                Ok(mut outcome) => {
                    tracing::info!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "success",
                        "model candidate succeeded"
                    );
                    // provider 内部兼容 fallback 与跨模型候选降级语义不同；这里只在
                    // 真正使用后续模型候选时标记，保持原有候选链行为不变。
                    outcome.fallback_used |= index > 0;
                    return Ok(outcome);
                }
                Err(err) => {
                    let fallback = index + 1 < route.len() && should_try_next_model(&err);
                    tracing::warn!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "failed",
                        error_code = err.code.as_str(),
                        error_stage = err.stage.as_str(),
                        error_kind = model_error_kind(&err),
                        fallback,
                        "model candidate failed"
                    );
                    if !fallback {
                        if route.len() == 1 || !should_try_next_model(&err) {
                            return Err(err);
                        }
                        failures.push(ModelAttemptFailure::new(
                            index,
                            provider_kind,
                            candidate,
                            err,
                        ));
                        return Err(aggregate_route_error(task, failures));
                    }
                    failures.push(ModelAttemptFailure::new(
                        index,
                        provider_kind,
                        candidate,
                        err,
                    ));
                }
            }
        }

        Err(aggregate_route_error(task, failures))
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn model(&self) -> &str {
        &self.model_display
    }

    fn stream_enabled(&self) -> bool {
        self.providers
            .first()
            .map(|(_, provider)| provider.stream_enabled())
            .unwrap_or(false)
    }
}

#[derive(Debug)]
struct ModelAttemptFailure {
    index: usize,
    provider: ModelProvider,
    model: String,
    error: LlmError,
}

impl ModelAttemptFailure {
    fn new(index: usize, provider: ModelProvider, candidate: &ModelId, error: LlmError) -> Self {
        Self {
            index,
            provider,
            model: candidate.name.clone(),
            error,
        }
    }
}

fn auto_default_route(config: &AppConfig) -> Result<ModelRoute, LlmError> {
    let mut candidates = config.model_route.candidates().to_vec();
    // 兼容旧的 `LLM_PROVIDER=auto` 行为：单个 OpenAI/裸主模型在可恢复失败时，
    // 仍可降级到 `DEEPSEEK_MODEL`。用户显式写多个候选时则严格按配置顺序执行。
    if candidates.len() == 1
        && config.deepseek_api_key.is_some()
        && candidates[0].provider != Some(ModelProvider::DeepSeek)
    {
        let deepseek_model = deepseek::deepseek_config_model(&config.deepseek_model)?;
        candidates.push(ModelId {
            provider: Some(ModelProvider::DeepSeek),
            name: deepseek_model,
        });
    }
    ModelRoute::from_candidates(candidates)
}

fn auto_provider_routes(
    config: &AppConfig,
    default_route: &ModelRoute,
) -> Result<Vec<(&'static str, ModelRoute)>, LlmError> {
    let mut routes = config.configured_model_routes()?;
    if let Some((_, route)) = routes.iter_mut().find(|(name, _)| *name == "LLM_MODEL") {
        // provider 初始化必须使用 auto 模式的实际默认链，才能保留单 OpenAI
        // 主模型自动追加 DeepSeek fallback 的兼容行为。
        *route = default_route.clone();
    }
    Ok(routes)
}

fn provider_kinds_for_routes(
    routes: &[(&'static str, ModelRoute)],
    default_provider: ModelProvider,
) -> Vec<ModelProvider> {
    [ModelProvider::OpenAi, ModelProvider::DeepSeek]
        .into_iter()
        .filter(|provider| {
            routes
                .iter()
                .any(|(_, route)| route_uses_provider(route, *provider, default_provider))
        })
        .collect()
}

fn ensure_deepseek_api_key_for_routes(
    config: &AppConfig,
    routes: &[(&'static str, ModelRoute)],
) -> Result<(), LlmError> {
    let uses_deepseek = routes
        .iter()
        .filter_map(|(name, route)| {
            route_uses_provider(route, ModelProvider::DeepSeek, ModelProvider::OpenAi)
                .then_some(*name)
        })
        .collect::<Vec<_>>()
        .join(", ");
    if uses_deepseek.is_empty() {
        return Ok(());
    }

    let api_key = config.deepseek_api_key.as_ref().ok_or_else(|| {
        LlmError::config(format!(
            "DEEPSEEK_API_KEY is required because configured model routes include DeepSeek: {uses_deepseek}"
        ))
    })?;
    if api_key.trim().is_empty() {
        return Err(LlmError::config(format!(
            "DEEPSEEK_API_KEY is required because configured model routes include DeepSeek: {uses_deepseek}"
        )));
    }
    Ok(())
}

fn ensure_route_supported(
    route: &ModelRoute,
    supported: ModelProvider,
    default_provider: ModelProvider,
    name: &str,
) -> Result<(), LlmError> {
    for candidate in route.candidates() {
        let provider = candidate.provider.unwrap_or(default_provider);
        if provider != supported {
            return Err(LlmError::config(format!(
                "{name} candidate `{}` requires provider `{}`, but LLM_PROVIDER is `{}`",
                candidate.to_request_model(),
                provider.as_str(),
                supported.as_str()
            )));
        }
    }
    Ok(())
}

fn route_uses_provider(
    route: &ModelRoute,
    provider: ModelProvider,
    default_provider: ModelProvider,
) -> bool {
    route
        .candidates()
        .iter()
        .any(|candidate| candidate.provider.unwrap_or(default_provider) == provider)
}

/// 判断当前错误是否允许跨候选模型降级。
///
/// 这里只接收上游传输、限流、超时、空响应和 provider 协议类失败；配置错误、
/// 本地请求构造错误和业务参数错误会直接返回，避免把本地问题放大成多次计费请求。
fn should_try_next_model(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "timeout" | "provider_error" | "http_error" | "rate_limited" | "upstream_unavailable"
    )
}

fn model_error_kind(err: &LlmError) -> &'static str {
    match err.code.as_str() {
        "timeout" => "timeout",
        "http_error" => "http_error",
        "provider_error" if matches!(err.stage.as_str(), "stream" | "sse") => "stream_error",
        "provider_error" if err.stage == "json" => "invalid_response",
        "provider_error" => "provider_error",
        "rate_limited" => "rate_limited",
        "upstream_unavailable" => "upstream_unavailable",
        "bad_request" => "permanent",
        "config" => "config",
        _ => "permanent",
    }
}

fn model_task_name(req: &ChatRequest) -> &str {
    req.metadata
        .get("purpose")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("chat")
}

fn aggregate_route_error(task: &str, failures: Vec<ModelAttemptFailure>) -> LlmError {
    let details = failures
        .into_iter()
        .map(|failure| {
            format!(
                "#{} {}:{} -> {}@{}",
                failure.index,
                failure.provider.as_str(),
                failure.model,
                failure.error.code,
                failure.error.stage
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    LlmError::provider(
        format!("all model candidates failed for task `{task}`: {details}"),
        "provider_route",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OpenAiApiMode;
    use crate::{
        config::{
            DEFAULT_APP_DB_FILE, DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_MAX_OUTPUT_TOKENS,
            DEFAULT_MEMBER_ID_MAPPING_FILE, DEFAULT_PROMPT_DIR, DEFAULT_REQUEST_TIMEOUT_SECONDS,
            DEFAULT_RSS_HTTP_TIMEOUT_SECONDS, DEFAULT_RSS_MAX_BODY_BYTES,
            DEFAULT_RSS_MAX_PUSH_PER_FEED, DEFAULT_RSS_POLL_INTERVAL_SECONDS,
            DEFAULT_RSS_PUSH_MAX_FAILURES, DEFAULT_RSS_PUSH_MESSAGE_TYPE, DEFAULT_RSS_PUSH_URL,
            DEFAULT_RSS_SEEN_RETENTION, DEFAULT_RSS_SUMMARY_MAX_CHARS, DEFAULT_SERVER_HOST,
            DEFAULT_SERVER_PORT, DEFAULT_TTFT_WARN_SECONDS, ProviderMode,
        },
        provider::types::{ChatMessage, ChatRequest},
        util::metrics::LlmMetrics,
    };
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct MockProvider {
        name: &'static str,
        model: &'static str,
        stream: bool,
        results: Arc<Mutex<Vec<Result<ChatOutcome, LlmError>>>>,
        calls: Arc<Mutex<usize>>,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    impl MockProvider {
        fn new(name: &'static str, results: Vec<Result<ChatOutcome, LlmError>>) -> Self {
            Self {
                name,
                model: "mock-model",
                stream: false,
                results: Arc::new(Mutex::new(results)),
                calls: Arc::new(Mutex::new(0)),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            *self.calls.lock().unwrap() += 1;
            self.requests.lock().unwrap().push(req);
            self.results.lock().unwrap().remove(0)
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn model(&self) -> &str {
            self.model
        }

        fn stream_enabled(&self) -> bool {
            self.stream
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            session_id: "group:g1".to_owned(),
            model: None,
            messages: vec![ChatMessage::user("hi")],
            metadata: HashMap::new(),
        }
    }

    fn outcome(reply: &str) -> ChatOutcome {
        ChatOutcome {
            reply: reply.to_owned(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: "mock-model".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            fallback_used: false,
        }
    }

    fn app_config(provider: ProviderMode, model: &str) -> AppConfig {
        AppConfig {
            provider,
            model: model.to_owned(),
            model_route: ModelRoute::parse_config(model, "LLM_MODEL").unwrap(),
            title_model: None,
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
            openai_search_model: "gpt-5.5".to_owned(),
            openai_api_key: Some("test-openai-key".to_owned()),
            openai_base_url: None,
            openai_api_mode: OpenAiApiMode::Auto,
            deepseek_api_key: None,
            deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
            deepseek_model: "deepseek:deepseek-chat".to_owned(),
            stream: true,
            send_mode: "final".to_owned(),
            request_timeout_seconds: DEFAULT_REQUEST_TIMEOUT_SECONDS,
            ttft_warn_seconds: DEFAULT_TTFT_WARN_SECONDS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            server_host: DEFAULT_SERVER_HOST.to_owned(),
            server_port: DEFAULT_SERVER_PORT,
            app_db_file: DEFAULT_APP_DB_FILE.to_owned(),
            rss_enabled: true,
            rss_poll_interval_seconds: DEFAULT_RSS_POLL_INTERVAL_SECONDS,
            rss_http_timeout_seconds: DEFAULT_RSS_HTTP_TIMEOUT_SECONDS,
            rss_max_body_bytes: DEFAULT_RSS_MAX_BODY_BYTES,
            rss_max_push_per_feed: DEFAULT_RSS_MAX_PUSH_PER_FEED,
            rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS,
            rss_seen_retention: DEFAULT_RSS_SEEN_RETENTION,
            rss_push_max_failures: DEFAULT_RSS_PUSH_MAX_FAILURES,
            rss_push_url: DEFAULT_RSS_PUSH_URL.to_owned(),
            rss_push_token: None,
            rss_push_message_type: DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
            todo_daily_reminder_enabled: false,
            todo_daily_reminder_time: crate::config::DailyReminderTime { hour: 9, minute: 0 },
            rss_allow_private_urls: false,
            prompt_dir: DEFAULT_PROMPT_DIR.to_owned(),
            prompt_dir_uses_builtin_defaults: true,
            world_file: None,
            context_modules_file: None,
            member_id_mapping_file: DEFAULT_MEMBER_ID_MAPPING_FILE.to_owned(),
            qweather_api_key: "test-qweather-key".to_owned(),
            qweather_api_host: "https://api.qweather.com".to_owned(),
            qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
            web_console_enabled: false,
            web_console_allowed_origins: Vec::new(),
        }
    }

    fn auto_required_provider_kinds(config: &AppConfig) -> Result<Vec<ModelProvider>, LlmError> {
        let route = auto_default_route(config)?;
        let provider_routes = auto_provider_routes(config, &route)?;
        if provider_kinds_for_routes(&provider_routes, ModelProvider::OpenAi)
            .contains(&ModelProvider::DeepSeek)
        {
            ensure_deepseek_api_key_for_routes(config, &provider_routes)?;
        }
        Ok(provider_kinds_for_routes(
            &provider_routes,
            ModelProvider::OpenAi,
        ))
    }

    fn route_provider(
        route: &str,
        openai_results: Vec<Result<ChatOutcome, LlmError>>,
        deepseek_results: Vec<Result<ChatOutcome, LlmError>>,
    ) -> (ModelRouteProvider, Arc<MockProvider>, Arc<MockProvider>) {
        let openai = Arc::new(MockProvider::new("openai", openai_results));
        let deepseek = Arc::new(MockProvider::new("deepseek", deepseek_results));
        let provider = ModelRouteProvider::new(
            "auto",
            ModelProvider::OpenAi,
            ModelRoute::parse_config(route, "LLM_MODEL").unwrap(),
            vec![
                (ModelProvider::OpenAi, openai.clone()),
                (ModelProvider::DeepSeek, deepseek.clone()),
            ],
        )
        .unwrap();
        (provider, openai, deepseek)
    }

    #[test]
    fn auto_default_route_appends_deepseek_fallback_for_single_openai_model() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());

        let route = auto_default_route(&config).unwrap();
        let provider = build_provider(&config).unwrap();

        assert_eq!(
            route.display(),
            "openai:gpt-5.4-mini,deepseek:deepseek-chat"
        );
        assert_eq!(
            provider.model(),
            "openai:gpt-5.4-mini,deepseek:deepseek-chat"
        );
    }

    #[test]
    fn auto_default_route_keeps_explicit_candidate_order() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());

        let route = auto_default_route(&config).unwrap();

        assert_eq!(route.display(), "openai:gpt-5.4-mini,openai:gpt-5.4");
    }

    #[test]
    fn auto_provider_set_includes_deepseek_from_translation_model() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());
        config.translation_model = Some("deepseek:deepseek-chat".to_owned());

        let providers = auto_required_provider_kinds(&config).unwrap();
        let provider = build_provider(&config).unwrap();

        assert_eq!(
            providers,
            vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
        );
        assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
    }

    #[test]
    fn auto_provider_set_includes_specialty_deepseek_with_explicit_openai_main_chain() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());
        config.translation_model = Some("deepseek:deepseek-chat,openai:gpt-5.4-mini".to_owned());

        let default_route = auto_default_route(&config).unwrap();
        let providers = auto_required_provider_kinds(&config).unwrap();
        let provider = build_provider(&config).unwrap();

        assert_eq!(
            default_route.display(),
            "openai:gpt-5.4-mini,openai:gpt-5.4"
        );
        assert_eq!(
            providers,
            vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
        );
        assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
    }

    #[test]
    fn auto_provider_set_rejects_specialty_deepseek_without_api_key() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
        config.translation_model = Some("deepseek:deepseek-chat".to_owned());

        let err = match build_provider(&config) {
            Ok(_) => panic!("build_provider should reject missing DeepSeek API key"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
        assert!(err.message.contains("DEEPSEEK_API_KEY"));
        assert!(err.message.contains("TRANSLATION_MODEL"));
    }

    #[test]
    fn auto_provider_set_keeps_openai_only_without_deepseek_key() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
        config.title_model = Some("openai:gpt-5.4-mini".to_owned());
        config.translation_model = Some("openai:gpt-5.4-mini".to_owned());

        let providers = auto_required_provider_kinds(&config).unwrap();
        let provider = build_provider(&config).unwrap();

        assert_eq!(providers, vec![ModelProvider::OpenAi]);
        assert_eq!(provider.name(), "auto");
        assert_eq!(provider.model(), "openai:gpt-5.4-mini");
    }

    #[test]
    fn auto_provider_set_deduplicates_repeated_specialty_providers() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());
        config.title_model = Some("deepseek:deepseek-chat".to_owned());
        config.todo_model = Some("deepseek:deepseek-chat,openai:gpt-5.4-mini".to_owned());
        config.memory_model = Some("deepseek:deepseek-chat".to_owned());
        config.translation_model = Some("deepseek:deepseek-chat".to_owned());

        let providers = auto_required_provider_kinds(&config).unwrap();

        assert_eq!(
            providers,
            vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
        );
    }

    #[test]
    fn auto_deepseek_only_does_not_require_openai_provider() {
        let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
        config.openai_api_key = None;
        config.deepseek_api_key = Some("test-deepseek-key".to_owned());

        let providers = auto_required_provider_kinds(&config).unwrap();
        let provider = build_provider(&config).unwrap();

        assert_eq!(providers, vec![ModelProvider::DeepSeek]);
        assert_eq!(provider.name(), "auto");
        assert_eq!(provider.model(), "deepseek:deepseek-chat");
    }

    #[test]
    fn fixed_provider_modes_validate_specialty_routes_at_startup() {
        let mut config = app_config(ProviderMode::OpenAi, "openai:gpt-5.4-mini");
        config.translation_model = Some("deepseek:deepseek-chat".to_owned());

        let err = match build_provider(&config) {
            Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
        assert!(err.message.contains("TRANSLATION_MODEL"));
        assert!(err.message.contains("requires provider `deepseek`"));
    }

    #[test]
    fn configured_specialty_route_rejects_unsupported_provider_at_startup() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
        config.translation_model = Some("anthropic:claude".to_owned());

        let err = match build_provider(&config) {
            Ok(_) => panic!("build_provider should reject unsupported provider prefix"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
        assert!(err.message.contains("TRANSLATION_MODEL"));
        assert!(err.message.contains("unsupported model provider prefix"));
    }

    #[test]
    fn provider_errors_are_fallback_eligible() {
        assert!(should_try_next_model(&LlmError::provider(
            "upstream failed",
            "provider"
        )));
        assert!(should_try_next_model(&LlmError::timeout("request")));
        assert!(!should_try_next_model(&LlmError::config("missing key")));
        assert!(!should_try_next_model(&LlmError::new(
            "bad_request",
            "bad local request",
            "request"
        )));
    }

    #[tokio::test]
    async fn model_route_provider_uses_first_successful_candidate() {
        let (provider, openai, deepseek) = route_provider(
            "openai:gpt-a,deepseek:deepseek-chat",
            vec![Ok(outcome("primary"))],
            vec![Ok(outcome("fallback"))],
        );

        let result = provider.chat(request()).await.unwrap();

        assert_eq!(result.reply, "primary");
        assert!(!result.fallback_used);
        assert_eq!(openai.calls(), 1);
        assert_eq!(deepseek.calls(), 0);
        assert_eq!(openai.requests()[0].model.as_deref(), Some("openai:gpt-a"));
    }

    #[tokio::test]
    async fn model_route_provider_falls_back_on_eligible_error() {
        let (provider, openai, deepseek) = route_provider(
            "openai:gpt-a,deepseek:deepseek-chat",
            vec![Err(LlmError::timeout("provider"))],
            vec![Ok(outcome("fallback"))],
        );

        let result = provider.chat(request()).await.unwrap();

        assert_eq!(result.reply, "fallback");
        assert!(result.fallback_used);
        assert_eq!(openai.calls(), 1);
        assert_eq!(deepseek.calls(), 1);
        assert_eq!(
            deepseek.requests()[0].model.as_deref(),
            Some("deepseek:deepseek-chat")
        );
    }

    #[tokio::test]
    async fn model_route_provider_keeps_permanent_error() {
        let (provider, openai, deepseek) = route_provider(
            "openai:gpt-a,deepseek:deepseek-chat",
            vec![Err(LlmError::config("missing key"))],
            vec![Ok(outcome("fallback"))],
        );

        let err = provider.chat(request()).await.unwrap_err();

        assert_eq!(err.code, "config");
        assert_eq!(openai.calls(), 1);
        assert_eq!(deepseek.calls(), 0);
    }

    #[tokio::test]
    async fn model_route_provider_aggregates_all_candidate_failures() {
        let (provider, openai, deepseek) = route_provider(
            "openai:gpt-a,deepseek:deepseek-chat",
            vec![Err(LlmError::timeout("provider"))],
            vec![Err(LlmError::provider("empty response", "provider"))],
        );

        let err = provider.chat(request()).await.unwrap_err();

        assert_eq!(err.code, "provider_error");
        assert_eq!(err.stage, "provider_route");
        assert!(err.message.contains("#0 openai:gpt-a -> timeout@provider"));
        assert!(
            err.message
                .contains("#1 deepseek:deepseek-chat -> provider_error@provider")
        );
        assert_eq!(openai.calls(), 1);
        assert_eq!(deepseek.calls(), 1);
    }

    #[tokio::test]
    async fn model_route_provider_uses_request_route_override() {
        let (provider, openai, deepseek) = route_provider(
            "openai:gpt-a",
            vec![Ok(outcome("primary"))],
            vec![Ok(outcome("deepseek"))],
        );
        let mut req = request();
        req.model = Some("deepseek:deepseek-chat".to_owned());

        let result = provider.chat(req).await.unwrap();

        assert_eq!(result.reply, "deepseek");
        assert_eq!(openai.calls(), 0);
        assert_eq!(deepseek.calls(), 1);
    }
}
