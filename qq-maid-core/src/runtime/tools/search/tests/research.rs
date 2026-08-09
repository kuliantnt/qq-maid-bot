use super::*;

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
    assert!(
        requests
            .iter()
            .all(|request| request.max_results == Some(2))
    );
}

#[tokio::test]
async fn multi_entity_research_clamps_every_target_to_server_limit() {
    let executor = ResearchExecutor::default();
    let requests = executor.requests.clone();
    let tool = WebSearchTool::new(Arc::new(executor));
    let mut arguments = research_arguments(&[
        ("目标一", "目标一公开资料"),
        ("目标二", "目标二公开资料"),
        ("目标三", "目标三公开资料"),
    ]);
    arguments["max_results"] = json!(10);

    tool.execute(test_context(), arguments).await.unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.max_results == Some(5))
    );
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
