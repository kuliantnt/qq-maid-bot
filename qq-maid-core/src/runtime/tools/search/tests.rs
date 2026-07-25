use std::{
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

#[derive(Clone, Default)]
struct ResearchExecutor {
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[async_trait]
impl WebSearchExecutor for ResearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        unreachable!("research test requires streaming")
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        struct ActiveGuard(Arc<AtomicUsize>);
        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        self.requests.lock().unwrap().push(req.clone());
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _guard = ActiveGuard(self.active.clone());
        if req.query.contains("失败") {
            return Err(LlmError::provider("simulated research failure", "provider"));
        }
        if req.query.contains("超时") {
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        if req.query.contains("无结果") {
            return Ok(WebSearchOutcome {
                answer: String::new(),
                sources: Vec::new(),
                provider: "research-mock".to_owned(),
                elapsed_ms: 15,
            });
        }
        let _ = delta_tx.send("事实".to_owned()).await;
        let long_result = req.query.contains("长结果");
        Ok(WebSearchOutcome {
            answer: if long_result {
                "事实".repeat(1000)
            } else {
                format!("{} 的可核实事实", req.query)
            },
            sources: vec![WebSearchSource {
                title: "研究来源".to_owned(),
                url: if long_result {
                    format!("https://example.test/{}", "long".repeat(100))
                } else {
                    format!("https://example.test/{}", req.query)
                },
                snippet: "公开资料摘要".to_owned(),
            }],
            provider: "research-mock".to_owned(),
            elapsed_ms: 15,
        })
    }

    fn provider_name(&self) -> &'static str {
        "research-mock"
    }
}

fn research_arguments(queries: &[(&str, &str)]) -> Value {
    json!({
        "query": null,
        "raw_question": "对比这些项目",
        "max_results": 2,
        "context_size": "low",
        "comparison_dimensions": ["功能", "优缺点"],
        "research_targets": queries.iter().map(|(entity, query)| json!({
            "entity": entity,
            "query": query,
            "assumption": if *entity == "Hermes" {
                Some("指 Nous Research 的 Hermes Agent")
            } else {
                None
            },
        })).collect::<Vec<_>>(),
        "model_override": "model-from-tool-arguments"
    })
}

#[tokio::test]
async fn multi_entity_research_runs_independent_searches_with_bounded_concurrency() {
    let executor = ResearchExecutor::default();
    let requests = executor.requests.clone();
    let max_active = executor.max_active.clone();
    let tool = WebSearchTool::new(Arc::new(executor))
        .with_backend_override(WebSearchBackend::Tavily)
        .with_model_override("gemini:server-search-model".to_owned())
        .with_timeouts(WebSearchTimeouts {
            first_activity: Duration::from_millis(50),
            idle: Duration::from_millis(50),
            absolute: Duration::from_millis(100),
        });
    let mut context = test_context();
    context.tool_call_id = Some("agent-call".to_owned());

    let output = tool
        .execute(
            context,
            research_arguments(&[
                ("AstrBot", "AstrBot 功能"),
                ("Hermes", "Hermes Agent 功能"),
                ("OpenClaw", "OpenClaw 功能"),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], true);
    assert_eq!(output.value["mode"], "multi_entity_research");
    assert_eq!(output.value["successful"], 3);
    assert_eq!(output.value["failed"], 0);
    assert_eq!(output.value["results"][1]["entity"], "Hermes");
    assert_eq!(
        output.value["results"][1]["assumption"],
        "指 Nous Research 的 Hermes Agent"
    );
    assert!(max_active.load(Ordering::SeqCst) > 1);
    assert!(max_active.load(Ordering::SeqCst) <= ops::WEB_SEARCH_RESEARCH_CONCURRENCY);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request.model_override.as_deref() == Some("gemini:server-search-model")
    }));
    assert!(
        requests
            .iter()
            .all(|request| request.backend_override == Some(WebSearchBackend::Tavily))
    );
    assert!(requests.iter().all(|request| {
        request
            .raw_question
            .as_deref()
            .is_some_and(|question| question.contains("不要在本次搜索中生成跨实体对比"))
    }));
}

#[tokio::test]
async fn multi_entity_research_returns_partial_results() {
    let tool = WebSearchTool::new(Arc::new(ResearchExecutor::default())).with_timeouts(
        WebSearchTimeouts {
            first_activity: Duration::from_millis(20),
            idle: Duration::from_millis(20),
            absolute: Duration::from_millis(40),
        },
    );

    let output = tool
        .execute(
            test_context(),
            research_arguments(&[
                ("成功项", "成功查询"),
                ("失败项", "失败查询"),
                ("超时项", "超时查询"),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], true);
    assert_eq!(output.value["successful"], 1);
    assert_eq!(output.value["failed"], 2);
    assert_eq!(output.value["results"][0]["status"], "success");
    assert_eq!(output.value["results"][1]["status"], "failed");
    assert_eq!(output.value["results"][2]["status"], "timeout");
}

#[tokio::test]
async fn multi_entity_research_reports_all_failed_without_tool_error() {
    let tool = WebSearchTool::new(Arc::new(ResearchExecutor::default()));

    let output = tool
        .execute(
            test_context(),
            research_arguments(&[("失败一", "失败查询一"), ("失败二", "失败查询二")]),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], false);
    assert_eq!(output.value["successful"], 0);
    assert_eq!(output.value["failed"], 2);
    assert_eq!(output.value["error"]["code"], "provider_error");
    assert_eq!(output.value["error"]["stage"], "provider");
    assert!(
        output.value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["status"] == "failed")
    );
}

#[tokio::test]
async fn multi_entity_research_all_empty_results_keep_execution_success() {
    let tool = WebSearchTool::new(Arc::new(ResearchExecutor::default()));

    let output = tool
        .execute(
            test_context(),
            research_arguments(&[("空结果一", "无结果查询一"), ("空结果二", "无结果查询二")]),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], false);
    assert_eq!(output.value["execution_succeeded"], true);
    assert_eq!(output.value["result_count"], 0);
    assert_eq!(output.value["error"]["code"], "empty_result");
    assert_eq!(output.value["error"]["stage"], "web_search");
    assert_eq!(
        output.value["error"]["message"],
        WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE
    );
    assert!(
        output.value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["error"]["code"] == "empty_result")
    );
}

#[tokio::test]
async fn multi_entity_research_real_failure_is_not_masked_by_empty_results() {
    let tool = WebSearchTool::new(Arc::new(ResearchExecutor::default()));

    let output = tool
        .execute(
            test_context(),
            research_arguments(&[("空结果", "无结果查询"), ("失败项", "失败查询")]),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], false);
    assert_ne!(output.value["execution_succeeded"], true);
    assert_eq!(output.value["result_count"], 0);
    assert_eq!(output.value["error"]["code"], "provider_error");
    assert_eq!(output.value["error"]["stage"], "provider");
}

#[tokio::test]
async fn multi_entity_research_keeps_max_batch_output_structured_under_default_limit() {
    let tool = WebSearchTool::new(Arc::new(ResearchExecutor::default()));

    let output = tool
        .execute(
            test_context(),
            research_arguments(&[
                ("实体一", "长结果一"),
                ("实体二", "长结果二"),
                ("实体三", "长结果三"),
                ("实体四", "长结果四"),
                ("实体五", "长结果五"),
            ]),
        )
        .await
        .unwrap();
    let serialized = serde_json::to_string(&output.value).unwrap();

    assert!(serialized.chars().count() <= DEFAULT_TOOL_OUTPUT_MAX_CHARS);
    assert_eq!(output.value["results"].as_array().unwrap().len(), 5);
    assert!(
        output.value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["status"] == "success" && result["sources"] == json!([]))
    );
}
