//! LLM 统一服务入口。
//!
//! 该入口只组装 HTTP client、Provider、模型候选链、Web Search 和健康状态，
//! 不包含标题、Todo、记忆、翻译等 core 业务方法。

use tokio::sync::Semaphore;
use tokio::sync::mpsc;

use crate::{
    config::LlmConfig,
    error::LlmError,
    provider::{
        ChatOutcome, DynLlmProvider, LlmStream, build_provider,
        limiter::{LimitingLlmProvider, LimitingWebSearchExecutor},
        status::{UpstreamStatus, observe_provider},
        types::ChatRequest,
    },
    web_search::{
        DynWebSearchExecutor, WebSearchOutcome, WebSearchRequest, build_web_search_executor,
    },
};

#[derive(Clone)]
pub struct LlmService {
    provider: DynLlmProvider,
    web_search_executor: DynWebSearchExecutor,
    upstream_status: UpstreamStatus,
}

impl LlmService {
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let upstream_status = UpstreamStatus::default();
        let gate = None::<std::sync::Arc<Semaphore>>;
        let provider = observe_provider(
            std::sync::Arc::new(LimitingLlmProvider::new(
                build_provider(config)?,
                gate.clone(),
            )),
            upstream_status.clone(),
        );
        let web_search_executor = std::sync::Arc::new(LimitingWebSearchExecutor::new(
            build_web_search_executor(config)?,
            gate,
        ));
        Ok(Self {
            provider,
            web_search_executor,
            upstream_status,
        })
    }

    pub async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.provider.chat(req).await
    }

    pub async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.provider.stream_chat(req).await
    }

    pub async fn web_search(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.web_search_executor.query(req).await
    }

    pub async fn web_search_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.web_search_executor.query_stream(req, delta_tx).await
    }

    pub fn upstream_status(&self) -> UpstreamStatus {
        self.upstream_status.clone()
    }

    pub fn provider(&self) -> DynLlmProvider {
        self.provider.clone()
    }

    pub fn web_search_executor(&self) -> DynWebSearchExecutor {
        self.web_search_executor.clone()
    }
}
