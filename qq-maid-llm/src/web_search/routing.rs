use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{error::LlmError, provider::types::ModelId};

use super::{DynWebSearchExecutor, WebSearchExecutor, WebSearchOutcome, WebSearchRequest};

/// 按搜索模型前缀路由 `/查`：无前缀或 `openai:` 仍走 OpenAI，`gemini:` 走 Gemini 官方 Google Search。
pub(super) struct RoutedWebSearchExecutor {
    default_model: String,
    openai: DynWebSearchExecutor,
    gemini: DynWebSearchExecutor,
}

impl RoutedWebSearchExecutor {
    pub(super) fn new(
        default_model: String,
        openai: DynWebSearchExecutor,
        gemini: DynWebSearchExecutor,
    ) -> Self {
        Self {
            default_model,
            openai,
            gemini,
        }
    }

    fn route_request(
        &self,
        mut req: WebSearchRequest,
    ) -> Result<(DynWebSearchExecutor, WebSearchRequest), LlmError> {
        let configured_model = req
            .model_override
            .as_deref()
            .unwrap_or(self.default_model.as_str());
        let model = ModelId::parse(configured_model, "request")?;
        req.model_override = Some(model.name);
        match model.provider {
            Some(crate::provider::types::ModelProvider::Gemini) => Ok((self.gemini.clone(), req)),
            Some(crate::provider::types::ModelProvider::OpenAi) | None => {
                Ok((self.openai.clone(), req))
            }
            Some(provider) => Err(LlmError::new(
                "bad_request",
                format!(
                    "search model provider `{}` is not supported by /查; supported: openai, gemini",
                    provider.as_str()
                ),
                "request",
            )),
        }
    }
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
            "openai:gpt-search".to_owned(),
            Arc::new(MissingWebSearchExecutor),
            Arc::new(MissingGeminiWebSearchExecutor),
        );
        let base_req = WebSearchRequest {
            query: "测试".to_owned(),
            raw_question: None,
            max_results: None,
            context_size: None,
            model_override: None,
        };

        let (provider, routed_req) = executor.route_request(base_req.clone()).unwrap();
        assert_eq!(provider.provider_name(), "openai");
        assert_eq!(routed_req.model_override.as_deref(), Some("gpt-search"));

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
