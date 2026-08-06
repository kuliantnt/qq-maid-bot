use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use qq_maid_llm::{
    tool::ToolContext,
    web_search::{
        DEFAULT_MAX_RESULTS, WebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource,
    },
};
use tokio::sync::mpsc;

use crate::error::LlmError;

#[derive(Clone, Default)]
pub(super) struct MockWebSearchExecutor {
    pub(super) requests: Arc<Mutex<Vec<WebSearchRequest>>>,
    pub(super) stream_calls: Arc<AtomicUsize>,
    pub(super) max_results_limit: Option<u8>,
}

#[async_trait]
impl WebSearchExecutor for MockWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(WebSearchOutcome {
            answer: format!("answer: {}", req.query),
            sources: vec![WebSearchSource {
                title: "source title".to_owned(),
                url: "https://example.com".to_owned(),
                snippet: "source snippet".to_owned(),
            }],
            provider: "mock-query".to_owned(),
            elapsed_ms: 12,
        })
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self.query(req).await?;
        let _ = delta_tx.send(outcome.answer.clone()).await;
        Ok(outcome)
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
    }

    fn max_results_limit(&self) -> u8 {
        self.max_results_limit.unwrap_or(DEFAULT_MAX_RESULTS)
    }
}

pub(super) struct EmptyWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for EmptyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Ok(WebSearchOutcome {
            answer: String::new(),
            sources: Vec::new(),
            provider: "tavily".to_owned(),
            elapsed_ms: 12,
        })
    }

    fn provider_name(&self) -> &'static str {
        "tavily"
    }
}

pub(super) struct FailingWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for FailingWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::new(
            "tavily_auth_error",
            "Tavily rejected the configured API key",
            "tavily_http",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "tavily"
    }
}

pub(super) struct LargeWebSearchExecutor;

#[derive(Clone)]
pub(super) struct ScriptedWebSearchExecutor {
    pub(super) outcomes: Arc<Mutex<VecDeque<Result<WebSearchOutcome, LlmError>>>>,
    pub(super) calls: Arc<AtomicUsize>,
}

impl ScriptedWebSearchExecutor {
    pub(super) fn failures(errors: Vec<LlmError>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(errors.into_iter().map(Err).collect())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl WebSearchExecutor for ScriptedWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted web search outcome")
    }

    fn provider_name(&self) -> &'static str {
        "openai_responses"
    }
}

#[derive(Clone)]
pub(super) struct LogWriter(pub(super) Arc<Mutex<Vec<u8>>>);

pub(super) struct LogGuard(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogGuard {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogGuard;

    fn make_writer(&'a self) -> Self::Writer {
        LogGuard(self.0.clone())
    }
}

#[async_trait]
impl WebSearchExecutor for LargeWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Ok(WebSearchOutcome {
            answer: "昨日 AI 新闻重点。".repeat(220),
            sources: (0..8)
                .map(|index| WebSearchSource {
                    title: format!("来源 {index} {}", "标题".repeat(80)),
                    url: format!("https://example.com/news/{index}?query={}", "a".repeat(260)),
                    snippet: "公开报道摘要。".repeat(180),
                })
                .collect(),
            provider: "tavily".to_owned(),
            elapsed_ms: 12,
        })
    }

    fn provider_name(&self) -> &'static str {
        "tavily"
    }
}

pub(super) fn test_context() -> ToolContext {
    ToolContext {
        task_id: "task-1".to_owned(),
        actor: ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "test".to_owned(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: None,
        tool_round: None,
        retry_of: None,
        execution_deadline: None,
    }
}
