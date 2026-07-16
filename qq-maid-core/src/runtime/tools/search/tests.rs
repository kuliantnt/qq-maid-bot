use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;

use qq_maid_llm::{
    tool::{DEFAULT_TOOL_OUTPUT_MAX_CHARS, ToolRegistry},
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
    assert_eq!(requests[0].model_override.as_deref(), Some("gpt-search"));
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(output.value["answer"], "answer: Rust 新闻");
    assert_eq!(output.value["sources"][0]["url"], "https://example.com");
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
    assert!(
        output.value["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["status"] == "failed")
    );
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
