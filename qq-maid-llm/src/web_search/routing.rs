use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    error::LlmError,
    provider::types::{ModelId, ModelProvider},
};

use super::{
    DynWebSearchExecutor, WebSearchBackend, WebSearchExecutor, WebSearchOutcome, WebSearchRequest,
};

/// 先按统一后端配置分流；provider_native 再按完整 `provider:model` 身份选择执行器。
pub(super) struct RoutedWebSearchExecutor {
    default_backend: WebSearchBackend,
    default_model: String,
    default_max_results: u8,
    native_providers: HashMap<ModelProvider, DynWebSearchExecutor>,
    tavily: DynWebSearchExecutor,
    disabled: DynWebSearchExecutor,
}

impl RoutedWebSearchExecutor {
    pub(super) fn new(
        default_backend: WebSearchBackend,
        default_model: String,
        default_max_results: u8,
        native_providers: HashMap<ModelProvider, DynWebSearchExecutor>,
        tavily: DynWebSearchExecutor,
        disabled: DynWebSearchExecutor,
    ) -> Self {
        Self {
            default_backend,
            default_model,
            default_max_results,
            native_providers,
            tavily,
            disabled,
        }
    }

    fn route_request(
        &self,
        mut req: WebSearchRequest,
    ) -> Result<(DynWebSearchExecutor, WebSearchRequest), LlmError> {
        let backend = req.backend_override.unwrap_or(self.default_backend);
        if req.max_results.is_none() {
            req.max_results = Some(self.default_max_results);
        }
        let configured_model = req
            .model_override
            .as_deref()
            .unwrap_or(self.default_model.as_str());
        match backend {
            WebSearchBackend::Tavily => Ok((self.tavily.clone(), req)),
            WebSearchBackend::Disabled => Ok((self.disabled.clone(), req)),
            WebSearchBackend::ProviderNative => {
                let model = ModelId::parse(configured_model, "request")?;
                // 裸模型是历史公开配置格式，只在这一处明确兼容为内置 OpenAI；
                // 任何显式前缀都按完整身份查表，绝不能回落到同名 OpenAI 模型。
                let provider = model.provider.unwrap_or(ModelProvider::OpenAi);
                let executor = self
                    .native_providers
                    .get(&provider)
                    .ok_or_else(|| unsupported_provider_error(provider.as_str()))?;
                req.model_override = Some(model.name);
                Ok((executor.clone(), req))
            }
        }
    }
}

fn unsupported_provider_error(provider: &str) -> LlmError {
    LlmError::new(
        "bad_request",
        format!(
            "search provider `{provider}` is not configured for provider_native search; use built-in OpenAI/Gemini, declare an openai_responses provider, or configure Tavily"
        ),
        "request",
    )
}

#[async_trait]
impl WebSearchExecutor for RoutedWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let (executor, routed_req) = self.route_request(req)?;
        let provider = executor.provider_name();
        let model = routed_req.model_override.clone().unwrap_or_default();
        executor
            .query(routed_req)
            .await
            .map_err(|err| err.with_upstream_context(provider, model))
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (executor, routed_req) = self.route_request(req)?;
        let provider = executor.provider_name();
        let model = routed_req.model_override.clone().unwrap_or_default();
        executor
            .query_stream(routed_req, delta_tx)
            .await
            .map_err(|err| err.with_upstream_context(provider, model))
    }

    fn provider_name(&self) -> &'static str {
        "auto"
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use super::*;
    use crate::web_search::WebSearchOutcome;

    struct MarkerExecutor(&'static str);

    #[async_trait]
    impl WebSearchExecutor for MarkerExecutor {
        async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
            unreachable!("route selection test does not execute provider requests")
        }

        fn provider_name(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn routed_web_search_executor_selects_provider_by_model_prefix() {
        let native_providers: HashMap<ModelProvider, DynWebSearchExecutor> = HashMap::from([
            (
                ModelProvider::OpenAi,
                Arc::new(MarkerExecutor("openai")) as DynWebSearchExecutor,
            ),
            (
                ModelProvider::Gemini,
                Arc::new(MarkerExecutor("gemini")) as DynWebSearchExecutor,
            ),
            (
                ModelProvider::Custom("xai".to_owned()),
                Arc::new(MarkerExecutor("xai")) as DynWebSearchExecutor,
            ),
            (
                ModelProvider::Custom("routerb".to_owned()),
                Arc::new(MarkerExecutor("routerb")) as DynWebSearchExecutor,
            ),
        ]);
        let executor = RoutedWebSearchExecutor::new(
            WebSearchBackend::ProviderNative,
            "openai:gpt-search".to_owned(),
            8,
            native_providers,
            Arc::new(MarkerExecutor("tavily")),
            Arc::new(MarkerExecutor("disabled")),
        );
        let base_req = WebSearchRequest {
            query: "测试".to_owned(),
            raw_question: None,
            max_results: None,
            context_size: None,
            topic: None,
            time_range: None,
            backend_override: None,
            model_override: None,
        };

        let (provider, routed_req) = executor.route_request(base_req.clone()).unwrap();
        assert_eq!(provider.provider_name(), "openai");
        assert_eq!(routed_req.model_override.as_deref(), Some("gpt-search"));
        assert_eq!(routed_req.max_results, Some(8));

        let (provider, routed_req) = executor
            .route_request(WebSearchRequest {
                model_override: Some("gemini:gemini-2.5-flash".to_owned()),
                ..base_req.clone()
            })
            .unwrap();
        assert_eq!(provider.provider_name(), "gemini");
        assert_eq!(
            routed_req.model_override.as_deref(),
            Some("gemini-2.5-flash")
        );

        let (provider, routed_req) = executor
            .route_request(WebSearchRequest {
                model_override: Some("xai:grok-4".to_owned()),
                ..base_req.clone()
            })
            .unwrap();
        assert_eq!(provider.provider_name(), "xai");
        assert_eq!(routed_req.model_override.as_deref(), Some("grok-4"));

        let (provider, routed_req) = executor
            .route_request(WebSearchRequest {
                model_override: Some("routerb:grok-4".to_owned()),
                ..base_req.clone()
            })
            .unwrap();
        assert_eq!(provider.provider_name(), "routerb");
        assert_eq!(routed_req.model_override.as_deref(), Some("grok-4"));

        let err = match executor.route_request(WebSearchRequest {
            model_override: Some("deepseek:deepseek-chat".to_owned()),
            ..base_req.clone()
        }) {
            Ok(_) => panic!("deepseek search route should be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.code, "bad_request");
        assert!(err.message.contains("not configured for provider_native"));

        let (provider, routed_req) = executor
            .route_request(WebSearchRequest {
                model_override: Some("gpt-search-legacy".to_owned()),
                ..base_req
            })
            .unwrap();
        assert_eq!(provider.provider_name(), "openai");
        assert_eq!(
            routed_req.model_override.as_deref(),
            Some("gpt-search-legacy")
        );
    }
}
