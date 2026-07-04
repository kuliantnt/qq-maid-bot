//! LLM / Web Search 并发限制包装器。
//!
//! 该模块位于 `qq-maid-llm`，统一约束真正会触发上游模型或 OpenAI Responses 请求的入口。

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::debug;

use crate::{
    error::LlmError,
    provider::{
        ChatOutcome, DynLlmProvider, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol,
        ToolChatRequest,
    },
    web_search::{DynWebSearchExecutor, WebSearchExecutor, WebSearchOutcome, WebSearchRequest},
};

#[derive(Clone)]
pub struct LimitingLlmProvider {
    inner: DynLlmProvider,
    semaphore: Option<Arc<Semaphore>>,
}

impl LimitingLlmProvider {
    pub fn new(inner: DynLlmProvider, semaphore: Option<Arc<Semaphore>>) -> Self {
        Self { inner, semaphore }
    }
}

#[async_trait]
impl LlmProvider for LimitingLlmProvider {
    async fn chat(
        &self,
        req: crate::provider::types::ChatRequest,
    ) -> Result<ChatOutcome, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.chat(req).await;
        };
        log_waiting_permits("chat", semaphore);
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let result = self.inner.chat(req).await;
        drop(permit);
        result
    }

    async fn stream_chat(
        &self,
        req: crate::provider::types::ChatRequest,
    ) -> Result<LlmStream, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.stream_chat(req).await;
        };
        log_waiting_permits("stream_chat", semaphore);
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let inner = self.inner.stream_chat(req).await?;
        Ok(Box::pin(PermitHoldingStream {
            inner,
            _permit: permit,
        }))
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.chat_with_tools(req).await;
        };
        log_waiting_permits("chat_with_tools", semaphore);
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let result = self.inner.chat_with_tools(req).await;
        drop(permit);
        result
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.inner.tool_calling_protocol(model)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn stream_enabled(&self) -> bool {
        self.inner.stream_enabled()
    }
}

pub struct PermitHoldingStream {
    inner: LlmStream,
    _permit: OwnedSemaphorePermit,
}

impl Stream for PermitHoldingStream {
    type Item = Result<LlmStreamEvent, LlmError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

#[derive(Clone)]
pub struct LimitingWebSearchExecutor {
    inner: DynWebSearchExecutor,
    semaphore: Option<Arc<Semaphore>>,
}

impl LimitingWebSearchExecutor {
    pub fn new(inner: DynWebSearchExecutor, semaphore: Option<Arc<Semaphore>>) -> Self {
        Self { inner, semaphore }
    }
}

#[async_trait]
impl WebSearchExecutor for LimitingWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.query(req).await;
        };
        log_waiting_permits("web_search.query", semaphore);
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let result = self.inner.query(req).await;
        drop(permit);
        result
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let Some(semaphore) = &self.semaphore else {
            return self.inner.query_stream(req, delta_tx).await;
        };
        log_waiting_permits("web_search.query_stream", semaphore);
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LlmError::provider("LLM semaphore closed", "limiter"))?;
        let result = self.inner.query_stream(req, delta_tx).await;
        drop(permit);
        result
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }
}

fn log_waiting_permits(kind: &str, semaphore: &Arc<Semaphore>) {
    debug!(
        kind,
        available_permits = semaphore.available_permits(),
        "waiting for shared LLM concurrency permit"
    );
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use futures::{StreamExt, stream};
    use tokio::sync::{Barrier, Notify};

    use super::*;
    use crate::{
        metrics::LlmMetrics,
        provider::{LlmStreamEvent, types::ChatMessage},
        web_search::{WebSearchRequest, WebSearchSource},
    };

    #[derive(Clone)]
    struct BlockingProvider {
        entered: Arc<AtomicUsize>,
        release_chat: Arc<Notify>,
    }

    impl BlockingProvider {
        fn new() -> Self {
            Self {
                entered: Arc::new(AtomicUsize::new(0)),
                release_chat: Arc::new(Notify::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for BlockingProvider {
        async fn chat(
            &self,
            _req: crate::provider::types::ChatRequest,
        ) -> Result<ChatOutcome, LlmError> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.release_chat.notified().await;
            Ok(ChatOutcome {
                reply: "chat".to_owned(),
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
                executed_tools: Vec::new(),
                tool_results: Vec::new(),
            })
        }

        async fn stream_chat(
            &self,
            _req: crate::provider::types::ChatRequest,
        ) -> Result<LlmStream, LlmError> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(stream::iter(vec![
                Ok(LlmStreamEvent::TextDelta("a".to_owned())),
                Ok(LlmStreamEvent::TextDelta("b".to_owned())),
                Ok(LlmStreamEvent::Completed {
                    usage: None,
                    finish_reason: None,
                    fallback_used: false,
                }),
            ])))
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-model"
        }

        fn stream_enabled(&self) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct BlockingSearchExecutor {
        entered: Arc<AtomicUsize>,
        barrier: Arc<Barrier>,
        stream_started: Arc<AtomicUsize>,
    }

    impl BlockingSearchExecutor {
        fn new() -> Self {
            Self {
                entered: Arc::new(AtomicUsize::new(0)),
                barrier: Arc::new(Barrier::new(2)),
                stream_started: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl WebSearchExecutor for BlockingSearchExecutor {
        async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait().await;
            Ok(WebSearchOutcome {
                answer: "query".to_owned(),
                sources: vec![WebSearchSource {
                    title: "A".to_owned(),
                    url: "https://a.test".to_owned(),
                    snippet: String::new(),
                }],
                provider: "openai".to_owned(),
                elapsed_ms: 1,
            })
        }

        async fn query_stream(
            &self,
            _req: WebSearchRequest,
            delta_tx: tokio::sync::mpsc::Sender<String>,
        ) -> Result<WebSearchOutcome, LlmError> {
            self.stream_started.fetch_add(1, Ordering::SeqCst);
            let _ = delta_tx.send("delta".to_owned()).await;
            self.barrier.wait().await;
            Ok(WebSearchOutcome {
                answer: "delta".to_owned(),
                sources: Vec::new(),
                provider: "openai".to_owned(),
                elapsed_ms: 1,
            })
        }

        fn provider_name(&self) -> &'static str {
            "openai"
        }
    }

    fn request() -> crate::provider::types::ChatRequest {
        crate::provider::types::ChatRequest {
            session_id: "private:u1".to_owned(),
            model: None,
            messages: vec![ChatMessage::user("hi")],
            context_budget: None,
            max_output_tokens: None,
            reasoning_effort: None,
            metadata: HashMap::new(),
        }
    }

    fn search_request() -> WebSearchRequest {
        WebSearchRequest {
            query: "rust".to_owned(),
            raw_question: None,
            max_results: None,
            context_size: None,
            model_override: None,
        }
    }

    #[tokio::test]
    async fn stream_chat_holds_permit_until_stream_dropped() {
        let provider = Arc::new(BlockingProvider::new());
        let semaphore = Arc::new(Semaphore::new(1));
        let limiter = LimitingLlmProvider::new(provider.clone(), Some(semaphore.clone()));

        let mut stream = limiter.stream_chat(request()).await.unwrap();
        assert_eq!(provider.entered.load(Ordering::SeqCst), 1);
        assert_eq!(semaphore.available_permits(), 0);

        let next = limiter.stream_chat(request());
        tokio::pin!(next);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = &mut next => panic!("second stream should wait for permit"),
        }

        let mut deltas = Vec::new();
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                LlmStreamEvent::TextDelta(delta) => deltas.push(delta),
                LlmStreamEvent::Completed { .. } => break,
            }
        }
        assert_eq!(deltas, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(semaphore.available_permits(), 0);
        drop(stream);
        let second = next.await.unwrap();
        drop(second);
    }

    #[tokio::test]
    async fn chat_and_query_share_same_semaphore() {
        let provider = Arc::new(BlockingProvider::new());
        let search = Arc::new(BlockingSearchExecutor::new());
        let semaphore = Arc::new(Semaphore::new(1));
        let limiter = LimitingLlmProvider::new(provider.clone(), Some(semaphore.clone()));
        let search_limiter = LimitingWebSearchExecutor::new(search.clone(), Some(semaphore));
        let task = tokio::spawn(async move { limiter.chat(request()).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let query = search_limiter.query(search_request());
        tokio::pin!(query);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = &mut query => panic!("query should wait behind chat permit"),
        }
        provider.release_chat.notify_waiters();
        let _ = task.await.unwrap();
    }

    #[tokio::test]
    async fn zero_limit_passthrough_keeps_calls_direct() {
        let provider = Arc::new(BlockingProvider::new());
        let limiter = LimitingLlmProvider::new(provider.clone(), None);
        let search = Arc::new(BlockingSearchExecutor::new());
        let search_limiter = LimitingWebSearchExecutor::new(search.clone(), None);

        let _ = limiter.stream_chat(request()).await.unwrap();
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(4);
        let search_for_task = search_limiter.clone();
        let barrier = search.barrier.clone();
        let task = tokio::spawn(async move {
            search_for_task
                .query_stream(search_request(), delta_tx)
                .await
                .unwrap()
        });
        barrier.wait().await;
        let outcome = task.await.unwrap();
        assert_eq!(outcome.answer, "delta");
        assert_eq!(delta_rx.recv().await.as_deref(), Some("delta"));
    }

    #[tokio::test]
    async fn dropping_stream_releases_permit() {
        let provider = Arc::new(BlockingProvider::new());
        let semaphore = Arc::new(Semaphore::new(1));
        let limiter = LimitingLlmProvider::new(provider.clone(), Some(semaphore.clone()));

        let stream = limiter.stream_chat(request()).await.unwrap();
        assert_eq!(semaphore.available_permits(), 0);
        drop(stream);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn search_stream_uses_shared_limit() {
        let search = Arc::new(BlockingSearchExecutor::new());
        let semaphore = Arc::new(Semaphore::new(1));
        let limiter = LimitingWebSearchExecutor::new(search.clone(), Some(semaphore.clone()));
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel(4);

        let task = tokio::spawn({
            let limiter = limiter.clone();
            async move {
                limiter
                    .query_stream(search_request(), delta_tx)
                    .await
                    .unwrap()
            }
        });

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                assert_eq!(semaphore.available_permits(), 0);
            }
            value = delta_rx.recv() => {
                assert_eq!(value.as_deref(), Some("delta"));
            }
        }

        let second = limiter.query(search_request());
        tokio::pin!(second);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = &mut second => panic!("second search should wait for stream permit"),
        }
        search.barrier.wait().await;
        let _ = task.await.unwrap();
    }
}
