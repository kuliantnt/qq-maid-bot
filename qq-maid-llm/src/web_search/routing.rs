use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{error::LlmError, provider::types::ModelId};

use super::{
    DynWebSearchExecutor, WebSearchBackend, WebSearchExecutor, WebSearchOutcome, WebSearchRequest,
};

/// 先按统一后端配置分流；provider_native 再按模型前缀选择 OpenAI 或 Gemini。
pub(super) struct RoutedWebSearchExecutor {
    default_backend: WebSearchBackend,
    default_model: String,
    default_max_results: u8,
    openai: DynWebSearchExecutor,
    gemini: DynWebSearchExecutor,
    tavily: DynWebSearchExecutor,
    disabled: DynWebSearchExecutor,
}

impl RoutedWebSearchExecutor {
    pub(super) fn new(
        default_backend: WebSearchBackend,
        default_model: String,
        default_max_results: u8,
        openai: DynWebSearchExecutor,
        gemini: DynWebSearchExecutor,
        tavily: DynWebSearchExecutor,
        disabled: DynWebSearchExecutor,
    ) -> Self {
        Self {
            default_backend,
            default_model,
            default_max_results,
            openai,
            gemini,
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
                req.model_override = Some(model.name);
                match model.provider {
                    Some(crate::provider::types::ModelProvider::Gemini) => {
                        Ok((self.gemini.clone(), req))
                    }
                    Some(crate::provider::types::ModelProvider::OpenAi) | None => {
                        Ok((self.openai.clone(), req))
                    }
                    Some(provider) => Err(unsupported_provider_error(provider.as_str())),
                }
            }
        }
    }
}

fn unsupported_provider_error(provider: &str) -> LlmError {
    LlmError::new(
        "bad_request",
        format!(
            "search model provider `{provider}` is not supported by /查; supported: openai, gemini"
        ),
        "request",
    )
}

#[async_trait]
impl WebSearchExecutor for RoutedWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let (executor, routed_req) = self.route_request(req)?;
        executor.query(routed_req).await
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (executor, routed_req) = self.route_request(req)?;
        executor.query_stream(routed_req, delta_tx).await
    }

    fn provider_name(&self) -> &'static str {
        "auto"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::web_search::{
        gemini::MissingGeminiWebSearchExecutor, openai::MissingWebSearchExecutor,
    };

    #[test]
    fn routed_web_search_executor_selects_provider_by_model_prefix() {
        let executor = RoutedWebSearchExecutor::new(
            WebSearchBackend::ProviderNative,
            "openai:gpt-search".to_owned(),
            8,
            Arc::new(MissingWebSearchExecutor),
            Arc::new(MissingGeminiWebSearchExecutor),
            Arc::new(MissingWebSearchExecutor),
            Arc::new(MissingWebSearchExecutor),
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

        let err = match executor.route_request(WebSearchRequest {
            model_override: Some("deepseek:deepseek-chat".to_owned()),
            ..base_req
        }) {
            Ok(_) => panic!("deepseek search route should be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.code, "bad_request");
        assert!(err.message.contains("supported: openai, gemini"));
    }
}
