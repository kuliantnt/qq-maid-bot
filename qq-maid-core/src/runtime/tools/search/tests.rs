use std::{
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;

use qq_maid_llm::{
    tool::{DEFAULT_TOOL_OUTPUT_MAX_CHARS, DEFAULT_TOOL_TIMEOUT, ToolRegistry},
    web_search::WebSearchExecutor,
};

use super::*;

#[derive(Clone, Default)]
struct MockWebSearchExecutor {
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
    stream_calls: Arc<AtomicUsize>,
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
}

struct EmptyWebSearchExecutor;

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

struct FailingWebSearchExecutor;

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

struct LargeWebSearchExecutor;

#[derive(Clone)]
struct ScriptedWebSearchExecutor {
    outcomes: Arc<Mutex<VecDeque<Result<WebSearchOutcome, LlmError>>>>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedWebSearchExecutor {
    fn failures(errors: Vec<LlmError>) -> Self {
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
struct LogWriter(Arc<Mutex<Vec<u8>>>);

struct LogGuard(Arc<Mutex<Vec<u8>>>);

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

fn test_context() -> ToolContext {
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

#[tokio::test]
async fn web_search_tool_reuses_query_executor() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let stream_calls = executor.stream_calls.clone();
    let tool = WebSearchTool::new(Arc::new(executor));

    let output = tool
        .execute(
            test_context(),
            json!({
                "query": "Rust 新闻",
                "raw_question": "/查 Rust 新闻",
                "max_results": 3,
                "context_size": "medium",
                "topic": "news",
                "time_range": "week",
                "model_override": "gpt-search",
            }),
        )
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "Rust 新闻");
    assert_eq!(requests[0].raw_question.as_deref(), Some("/查 Rust 新闻"));
    assert_eq!(requests[0].max_results, Some(3));
    assert_eq!(requests[0].context_size.as_deref(), Some("medium"));
    assert_eq!(requests[0].topic.as_deref(), Some("news"));
    assert_eq!(requests[0].time_range.as_deref(), Some("week"));
    assert_eq!(requests[0].model_override.as_deref(), Some("gpt-search"));
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(output.value["ok"], true);
    assert_eq!(output.value["result_count"], 1);
    assert_eq!(output.value["answer"], "answer: Rust 新闻");
    assert_eq!(output.value["sources"][0]["url"], "https://example.com");
}

#[tokio::test]
async fn tavily_empty_outcome_is_completed_without_tool_execution_failure() {
    let tool = WebSearchTool::new(Arc::new(EmptyWebSearchExecutor))
        .with_backend_override(WebSearchBackend::Tavily);

    let output = tool
        .execute(
            test_context(),
            json!({
                "query": "今日 AI 新闻",
                "raw_question": "今日 AI 新闻",
                "max_results": null,
                "context_size": null,
                "topic": null,
                "time_range": null,
                "research_targets": null,
            }),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], false);
    assert_eq!(output.value["execution_succeeded"], true);
    assert_eq!(output.value["backend"], "tavily");
    assert_eq!(output.value["error"]["code"], "empty_result");
    assert_eq!(
        output.value["error"]["message"],
        WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE
    );
    assert_eq!(output.value["result_count"], 0);
}

#[tokio::test]
async fn tavily_execution_failure_remains_retryable_tool_failure() {
    let tool = WebSearchTool::new(Arc::new(FailingWebSearchExecutor))
        .with_backend_override(WebSearchBackend::Tavily);
    let mut context = test_context();
    context.tool_call_id = Some("agent-call".to_owned());

    let output = tool
        .execute(
            context,
            json!({
                "query": "今日 AI 新闻",
                "raw_question": "今日 AI 新闻",
                "max_results": null,
                "context_size": null,
                "topic": null,
                "time_range": null,
                "research_targets": null,
            }),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], false);
    assert_eq!(output.value["execution_succeeded"], false);
    assert_eq!(output.value["backend"], "tavily");
    assert_eq!(output.value["error"]["code"], "tavily_auth_error");
    assert_eq!(
        output.value["error"]["message"],
        "Tavily rejected the configured API key"
    );
    assert_eq!(output.value["error"]["stage"], "tavily_http");
}

fn agent_search_argument_value() -> Value {
    json!({
        "query": "公开资料",
        "raw_question": "联网查询公开资料",
        "max_results": null,
        "context_size": null,
        "topic": null,
        "time_range": null,
        "research_targets": null,
    })
}

fn upstream_error(status: u16) -> LlmError {
    let code = match status {
        429 => "rate_limited",
        500..=599 => "upstream_unavailable",
        _ => "http_error",
    };
    LlmError::new(code, format!("upstream returned HTTP {status}"), "http")
        .with_upstream_status(status)
        .with_upstream_context("codexauv", "grok-4.5")
}

async fn execute_scripted_failures(errors: Vec<LlmError>) -> (ToolOutput, usize) {
    let executor = ScriptedWebSearchExecutor::failures(errors);
    let calls = executor.calls.clone();
    let tool = WebSearchTool::new(Arc::new(executor))
        .with_backend_override(WebSearchBackend::ProviderNative)
        .with_model_override("codexauv:grok-4.5".to_owned());
    let mut context = test_context();
    context.tool_call_id = Some("task-1:call-search".to_owned());
    let output = tool
        .execute(context, agent_search_argument_value())
        .await
        .unwrap();
    (output, calls.load(Ordering::SeqCst))
}

#[tokio::test]
async fn deterministic_web_search_failures_are_not_retried() {
    for (status, kind) in [
        (400, "upstream_bad_request"),
        (401, "authentication_failed"),
        (403, "permission_denied"),
    ] {
        let (output, calls) =
            execute_scripted_failures(vec![upstream_error(status), upstream_error(status)]).await;
        assert_eq!(calls, 1, "HTTP {status} must not retry");
        assert_eq!(output.value["attempts"], 1);
        assert_eq!(output.value["error"]["kind"], kind);
        assert_eq!(output.value["error"]["retriable"], false);
        assert_eq!(output.value["error"]["upstream_status"], status);
    }

    let missing = LlmError::new(
        "web_search_not_configured",
        "search credential is missing",
        "config",
    );
    let (output, calls) = execute_scripted_failures(vec![missing.clone(), missing]).await;
    assert_eq!(calls, 1);
    assert_eq!(output.value["error"]["kind"], "missing_configuration");
    assert_eq!(output.value["error"]["retriable"], false);
}

#[tokio::test]
async fn transient_web_search_failures_have_one_bounded_retry() {
    for error in [
        upstream_error(429),
        upstream_error(503),
        LlmError::new("timeout", "upstream timed out", "http")
            .with_upstream_context("codexauv", "grok-4.5"),
        LlmError::new("http_error", "connection reset", "http")
            .with_upstream_context("codexauv", "grok-4.5"),
    ] {
        let (output, calls) =
            execute_scripted_failures(vec![error.clone(), error.clone(), error]).await;
        assert_eq!(calls, WEB_SEARCH_MAX_ATTEMPTS);
        assert_eq!(output.value["attempts"], WEB_SEARCH_MAX_ATTEMPTS);
        assert_eq!(output.value["error"]["retriable"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn web_search_failure_log_is_classified_and_secret_free() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(LogWriter(bytes.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let error = LlmError::new(
        "http_error",
        "upstream rejected super-secret-key and private query",
        "http",
    )
    .with_upstream_status(400)
    .with_upstream_context("codexauv", "grok-4.5");

    let (_output, calls) = execute_scripted_failures(vec![error]).await;
    assert_eq!(calls, 1);
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "tool_name",
        "tool_call_id",
        "attempt",
        "duration_ms",
        "error_kind",
        "retriable",
        "backend",
        "upstream_status",
        "provider",
        "model",
        "failure_layer",
    ] {
        assert!(logs.contains(field), "missing log field {field}: {logs}");
    }
    assert!(logs.contains("upstream_bad_request"));
    assert!(logs.contains("codexauv"));
    assert!(logs.contains("grok-4.5"));
    assert!(!logs.contains("super-secret-key"));
    assert!(!logs.contains("private query"));
}

#[tokio::test(flavor = "current_thread")]
async fn web_search_success_log_keeps_attempt_chain_without_content() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(LogWriter(bytes.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let tool = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()))
        .with_backend_override(WebSearchBackend::ProviderNative)
        .with_model_override("safe-search-model".to_owned());
    let mut context = test_context();
    context.tool_call_id = Some("safe-tool-call".to_owned());

    let output = tool
        .execute(
            context,
            json!({"query": "private query content", "raw_question": "private prompt body"}),
        )
        .await
        .unwrap();
    assert_eq!(output.value["ok"], true);

    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "tool_name",
        "tool_call_id",
        "attempt",
        "duration_ms",
        "error_kind",
        "retriable",
        "backend",
        "upstream_status",
        "provider",
        "model",
        "failure_layer",
    ] {
        assert!(logs.contains(field), "missing log field {field}: {logs}");
    }
    assert!(logs.contains("safe-tool-call"));
    assert!(logs.contains("mock-query"));
    assert!(logs.contains("safe-search-model"));
    assert!(!logs.contains("private query content"));
    assert!(!logs.contains("private prompt body"));
    assert!(!logs.contains("answer:"));
}

#[tokio::test]
async fn large_search_result_keeps_structured_evidence_through_tool_registry() {
    const OUTPUT_MAX_CHARS: usize = 1_200;
    let registry = ToolRegistry::new()
        .with_limits(DEFAULT_TOOL_TIMEOUT, OUTPUT_MAX_CHARS)
        .register(
            WebSearchTool::new(Arc::new(LargeWebSearchExecutor))
                .with_backend_override(WebSearchBackend::Tavily)
                .with_output_max_chars(OUTPUT_MAX_CHARS),
        )
        .unwrap();
    let mut context = test_context();
    context.tool_call_id = Some("agent-call".to_owned());

    let serialized = registry
        .execute_json(
            &context,
            WEB_SEARCH_TOOL_NAME,
            r#"{"query":"昨日 AI 新闻","raw_question":"昨日 AI 新闻"}"#,
        )
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["execution_succeeded"], true);
    assert_eq!(output["result_count"], 8);
    assert_ne!(output["answer"], "");
    assert!(!output["sources"].as_array().unwrap().is_empty());
    assert_ne!(output["truncated"], true);
    assert!(serialized.chars().count() <= OUTPUT_MAX_CHARS);

    let rendered = crate::runtime::respond::search_flow::format_web_search_tool_reply(&output);
    assert!(rendered.contains("昨日 AI 新闻重点"));
    assert!(!rendered.contains("没查到明确结果"));
}

#[test]
fn compact_search_result_drops_oversized_source_instead_of_truncating_url() {
    const PREVIOUS_URL_MAX_CHARS: usize = 300;
    const OUTPUT_MAX_CHARS: usize = 420;
    let oversized_url = format!("https://example.com/{}", "a".repeat(PREVIOUS_URL_MAX_CHARS));
    let output = web_search_tool_output(
        &WebSearchOutcome {
            answer: "搜索结果".repeat(100),
            sources: vec![
                WebSearchSource {
                    title: "超长链接来源".to_owned(),
                    url: oversized_url.clone(),
                    snippet: "摘要".repeat(100),
                },
                WebSearchSource {
                    title: "可保留来源".to_owned(),
                    url: "https://example.com/valid".to_owned(),
                    snippet: "有效摘要".to_owned(),
                },
            ],
            provider: "mock-query".to_owned(),
            elapsed_ms: 12,
        },
        "tavily",
        OUTPUT_MAX_CHARS,
    );

    let urls = output["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|source| source["url"].as_str())
        .collect::<Vec<_>>();
    assert!(!urls.contains(&oversized_url.as_str()));
    assert_eq!(urls, ["https://example.com/valid"]);
    assert!(urls.iter().all(|url| !url.contains('…')));
    assert!(serialized_value_chars(&output) <= OUTPUT_MAX_CHARS);
}

#[test]
fn web_search_tool_empty_outcome_is_structured_failure_not_success_evidence() {
    let output = web_search_tool_output(
        &WebSearchOutcome {
            answer: " \n ".to_owned(),
            sources: Vec::new(),
            provider: "mock-query".to_owned(),
            elapsed_ms: 12,
        },
        "tavily",
        DEFAULT_TOOL_OUTPUT_MAX_CHARS,
    );

    assert_eq!(output["ok"], false);
    assert_eq!(output["execution_succeeded"], true);
    assert_eq!(output["result_count"], 0);
    assert_eq!(output["error"]["code"], "empty_result");
    assert_eq!(output["error"]["stage"], "web_search");
    assert_eq!(
        output["error"]["message"],
        WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE
    );
}

#[test]
fn web_search_tool_is_read_only_and_deduplicates_normalized_query() {
    let tool = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()));

    assert_eq!(tool.effect(), ToolEffect::ReadOnly);
    assert!(tool.cache_terminal_failures());
    let default_key = tool
        .deduplication_key(&json!({"query": " Rust   News "}))
        .unwrap();
    assert_eq!(
        default_key,
        tool.deduplication_key(&json!({
            "query": "rust news",
            "raw_question": "RUST NEWS",
            "max_results": DEFAULT_MAX_RESULTS,
            "context_size": "low"
        }))
        .unwrap()
    );
    assert_eq!(
        default_key,
        tool.deduplication_key(&json!({
            "query": "rust news",
            "raw_question": null,
            "max_results": null,
            "context_size": null
        }))
        .unwrap()
    );
    assert_ne!(
        default_key,
        tool.deduplication_key(&json!({"query": "rust news", "max_results": 3}))
            .unwrap()
    );
    assert_ne!(
        default_key,
        tool.deduplication_key(&json!({"query": "rust news", "context_size": "high"}))
            .unwrap()
    );
    assert_ne!(
        default_key,
        tool.deduplication_key(&json!({
            "query": "rust news",
            "topic": "general"
        }))
        .unwrap()
    );
    assert_ne!(
        default_key,
        tool.deduplication_key(&json!({
            "query": "rust news",
            "raw_question": "different context"
        }))
        .unwrap()
    );
}

#[test]
fn web_search_tool_requires_context_complete_query() {
    let description = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()))
        .metadata()
        .description;

    assert!(description.contains("补全省略的搜索主体"));
    assert!(description.contains("脱离聊天上下文后仍可独立理解"));
    assert!(description.contains("不要先搜索泛化问题"));
}

struct DelayedStreamExecutor {
    first_delta_delay: Duration,
    completion_delay: Duration,
}

#[async_trait]
impl WebSearchExecutor for DelayedStreamExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::provider(
            "agent web search must use streaming",
            "test",
        ))
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        tokio::time::sleep(self.first_delta_delay).await;
        let _ = delta_tx.send("首字".to_owned()).await;
        tokio::time::sleep(self.completion_delay).await;
        Ok(WebSearchOutcome {
            answer: format!("answer: {}", req.query),
            sources: Vec::new(),
            provider: "delayed-stream".to_owned(),
            elapsed_ms: 0,
        })
    }

    fn provider_name(&self) -> &'static str {
        "delayed-stream"
    }
}

fn agent_search_arguments() -> &'static str {
    r#"{"query":"台风巴威","raw_question":"台风到哪里了","max_results":null,"context_size":null}"#
}

#[tokio::test]
async fn agent_web_search_times_out_only_before_first_activity() {
    let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
        first_delta_delay: Duration::from_millis(5),
        completion_delay: Duration::from_millis(30),
    }))
    .with_timeouts(WebSearchTimeouts {
        first_activity: Duration::from_millis(10),
        idle: Duration::from_millis(50),
        absolute: Duration::from_millis(100),
    });
    let registry = ToolRegistry::new()
        .with_limits(Duration::from_millis(10), DEFAULT_TOOL_OUTPUT_MAX_CHARS)
        .register(tool)
        .unwrap();

    let output = registry
        .execute_json(
            &test_context(),
            WEB_SEARCH_TOOL_NAME,
            agent_search_arguments(),
        )
        .await
        .unwrap();

    assert!(output.contains("answer: 台风巴威"));
}

#[tokio::test]
async fn agent_web_search_rejects_missing_first_activity() {
    let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
        first_delta_delay: Duration::from_millis(30),
        completion_delay: Duration::ZERO,
    }))
    .with_timeouts(WebSearchTimeouts {
        first_activity: Duration::from_millis(10),
        idle: Duration::from_millis(50),
        absolute: Duration::from_millis(100),
    });
    let registry = ToolRegistry::new().register(tool).unwrap();

    let err = registry
        .execute_json(
            &test_context(),
            WEB_SEARCH_TOOL_NAME,
            agent_search_arguments(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "timeout");
    assert_eq!(err.message, "web search first activity timed out");
    assert_eq!(err.stage, "web_search_first_activity");
}

#[tokio::test]
async fn agent_web_search_rejects_idle_stream_after_first_activity() {
    let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
        first_delta_delay: Duration::ZERO,
        completion_delay: Duration::from_millis(30),
    }))
    .with_timeouts(WebSearchTimeouts {
        first_activity: Duration::from_millis(10),
        idle: Duration::from_millis(5),
        absolute: Duration::from_millis(100),
    });

    let err = tool
        .execute(
            test_context(),
            serde_json::from_str(agent_search_arguments()).unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "web_search_idle");
}

#[tokio::test]
async fn explicit_search_stream_times_out_when_idle_after_first_delta() {
    let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
        first_delta_delay: Duration::ZERO,
        completion_delay: Duration::from_millis(30),
    }))
    .with_timeouts(WebSearchTimeouts {
        first_activity: Duration::from_millis(10),
        idle: Duration::from_millis(5),
        absolute: Duration::from_millis(100),
    });
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let captured = deltas.clone();

    let err = tool
        .query_stream_with_handler(
            WebSearchToolRequest {
                query: "台风巴威".to_owned(),
                raw_question: Some("/查 台风巴威".to_owned()),
                max_results: None,
                context_size: None,
                topic: None,
                time_range: None,
                backend_override: None,
                model_override: None,
            },
            Some(Box::new(move |delta| {
                let captured = captured.clone();
                Box::pin(async move {
                    captured.lock().unwrap().push(delta);
                    Ok(())
                })
            })),
        )
        .await
        .unwrap_err();

    assert_eq!(*deltas.lock().unwrap(), ["首字"]);
    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "web_search_idle");
}

struct HeartbeatStreamExecutor;

#[async_trait]
impl WebSearchExecutor for HeartbeatStreamExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        unreachable!("test requires streaming")
    }

    async fn query_stream(
        &self,
        _req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        loop {
            let _ = delta_tx.send("活动".to_owned()).await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn provider_name(&self) -> &'static str {
        "heartbeat"
    }
}

#[tokio::test]
async fn agent_web_search_enforces_absolute_timeout_despite_activity() {
    let tool =
        WebSearchTool::new(Arc::new(HeartbeatStreamExecutor)).with_timeouts(WebSearchTimeouts {
            first_activity: Duration::from_millis(10),
            idle: Duration::from_millis(10),
            absolute: Duration::from_millis(20),
        });

    let err = tool
        .execute(
            test_context(),
            serde_json::from_str(agent_search_arguments()).unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "web_search_absolute");
}

#[tokio::test]
async fn agent_web_search_caps_absolute_timeout_at_execution_deadline() {
    let tool =
        WebSearchTool::new(Arc::new(HeartbeatStreamExecutor)).with_timeouts(WebSearchTimeouts {
            first_activity: Duration::from_secs(1),
            idle: Duration::from_secs(1),
            absolute: Duration::from_secs(1),
        });
    let mut context = test_context();
    context.execution_deadline = Some(Instant::now() + Duration::from_millis(15));
    let started = Instant::now();

    let err = tool
        .execute(
            context,
            serde_json::from_str(agent_search_arguments()).unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(err.stage, "web_search_absolute");
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn web_search_tool_rejects_empty_query_without_calling_executor() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let tool = WebSearchTool::new(Arc::new(executor));

    let err = tool
        .execute(
            test_context(),
            json!({
                "query": " ",
                "raw_question": null,
                "max_results": null,
                "context_size": null,
                "model_override": null,
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert_eq!(requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn web_search_tool_rejects_overlong_query_without_calling_executor() {
    let executor = MockWebSearchExecutor::default();
    let requests = executor.requests.clone();
    let tool = WebSearchTool::new(Arc::new(executor));

    let err = tool
        .execute(
            test_context(),
            json!({
                "query": "a".repeat(WEB_SEARCH_QUERY_MAX_LENGTH + 1),
                "raw_question": null,
                "max_results": null,
                "context_size": null,
                "model_override": null,
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
    assert_eq!(err.message, "query is too long");
    assert_eq!(requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn web_search_tool_rejects_invalid_options() {
    let tool = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()));

    let err = tool
        .execute(
            test_context(),
            json!({
                "query": "Rust",
                "raw_question": null,
                "max_results": 99,
                "context_size": null,
                "model_override": null,
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_tool_arguments");

    let err = tool
        .execute(
            test_context(),
            json!({
                "query": "Rust",
                "raw_question": null,
                "max_results": null,
                "context_size": "huge",
                "model_override": null,
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_tool_arguments");

    for (field, value) in [("topic", "sports"), ("time_range", "quarter")] {
        let mut arguments = json!({"query": "Rust"});
        arguments[field] = json!(value);
        let err = tool.execute(test_context(), arguments).await.unwrap_err();
        assert_eq!(err.code, "bad_tool_arguments");
    }
}

mod research;
