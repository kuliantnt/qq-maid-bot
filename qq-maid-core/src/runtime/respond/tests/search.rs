use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use qq_maid_common::input_part::QuotedMessageContext;
use qq_maid_llm::provider::ToolCallingProtocol;
use qq_maid_llm::web_search::{
    WebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource,
};
use tokio::sync::mpsc;

use super::support::*;
use crate::error::LlmError;

struct LongAnswerWebSearchExecutor;

#[derive(Default)]
struct RecordingWebSearchExecutor {
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
}

impl RecordingWebSearchExecutor {
    fn requests(&self) -> Arc<Mutex<Vec<WebSearchRequest>>> {
        self.requests.clone()
    }
}

#[async_trait]
impl WebSearchExecutor for LongAnswerWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Ok(WebSearchOutcome {
            answer: format!("长结果 {} {}", req.query, "内容".repeat(3000)),
            sources: vec![WebSearchSource {
                title: "长结果来源".to_owned(),
                url: "https://example.test/long-search".to_owned(),
                snippet: "用于覆盖 ToolRegistry 输出截断的回归测试".to_owned(),
            }],
            provider: "long-answer-query".to_owned(),
            elapsed_ms: 42,
        })
    }

    fn provider_name(&self) -> &'static str {
        "long-answer-query"
    }
}

#[async_trait]
impl WebSearchExecutor for RecordingWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(recording_search_outcome(&req.query, "recording-query"))
    }

    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        delta_tx
            .send(format!("stream answer: {}", req.query))
            .await
            .map_err(|err| LlmError::provider(format!("stream delta failed: {err}"), "test"))?;
        Ok(recording_search_outcome(
            &req.query,
            "recording-stream-query",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "recording-query"
    }
}

#[tokio::test]
async fn natural_search_agent_can_call_web_search_without_router_rewrite() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "web_search",
            r#"{"query":"台风巴威","raw_question":"可以联网查一下","max_results":null,"context_size":null}"#,
            "根据联网结果回答",
        );
    let inspector = provider.clone();
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_query_weather_train_models_and_options(
            provider,
            None,
            Arc::new(executor),
            Arc::new(MockWeatherExecutor::new()),
            Arc::new(MockTrainExecutor::new()),
            TestModelOptions {
                todo_model: None,
                memory_model: None,
                compact_model: None,
                translation_model: None,
            },
            TestToolCallingOptions {
                enabled: true,
                group_enabled: false,
                group_enabled_tools: None,
            },
        );

    let response = service
        .respond(private_message("可以联网查一下"))
        .await
        .unwrap();

    assert!(response.text.as_deref().is_some_and(
        |text| text.contains("web answer: 台风巴威") && text.contains("根据联网结果回答")
    ));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].query, "台风巴威");
    assert!(inspector.requests().iter().all(|request| {
        request.metadata.get("purpose").map(String::as_str) != Some("search_query_rewrite")
    }));

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["respond_route"], "agent_runtime");
    assert_eq!(diagnostics["route_domains"], serde_json::json!(["search"]));
    assert_eq!(diagnostics["used_search"], true);
    assert_eq!(
        diagnostics["agent_executed_tools"],
        serde_json::json!(["web_search"])
    );
}

fn recording_search_outcome(query: &str, provider: &str) -> WebSearchOutcome {
    WebSearchOutcome {
        answer: format!("web answer: {query}"),
        sources: vec![WebSearchSource {
            title: "记录来源".to_owned(),
            url: "https://example.test/search".to_owned(),
            snippet: "用于记录最终搜索 query".to_owned(),
        }],
        provider: provider.to_owned(),
        elapsed_ms: 9,
    }
}

#[tokio::test]
async fn web_search_command_executes_web_search_tool() {
    let service = test_service();

    let response = service.respond(message("/查 keyword")).await.unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("web answer: keyword")
    );
    assert!(
        response
            .markdown
            .as_deref()
            .unwrap()
            .contains("web answer: keyword")
    );
    assert_eq!(response.diagnostics.unwrap()["used_search"], true);
    assert_eq!(response.command.as_deref(), Some("web_search"));
}

#[tokio::test]
async fn web_search_command_long_answer_uses_untruncated_tool_result() {
    let (service, _base) = test_service_with_provider_base_title_and_query(
        MockProvider::new(),
        None,
        Arc::new(LongAnswerWebSearchExecutor),
    );

    let response = service.respond(message("/查 long keyword")).await.unwrap();

    let text = response.text.as_deref().unwrap();
    assert!(text.starts_with("【联网查询】"));
    assert!(text.contains("长结果 long keyword"));
    assert!(!text.contains("没查到明确结果"));
    assert!(text.chars().count() <= 1500);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["query_provider"], "long-answer-query");
    assert_eq!(diagnostics["search_tool"], "web_search");
}

#[tokio::test]
async fn web_search_stream_executes_tool_stream_and_forwards_deltas() {
    let query_calls = Arc::new(AtomicUsize::new(0));
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let (service, _base) = test_service_with_provider_base_title_and_query(
        MockProvider::new(),
        None,
        Arc::new(StreamOnlyWebSearchExecutor {
            deltas: vec!["你".to_owned(), "好".to_owned()],
            query_calls: query_calls.clone(),
            stream_calls: stream_calls.clone(),
        }),
    );
    let deltas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected = deltas.clone();

    let response = service
        .respond_stream(message("/查 keyword"), move |delta| {
            let collected = collected.clone();
            Box::pin(async move {
                collected.lock().unwrap().push(delta);
                Ok(())
            })
        })
        .await
        .unwrap();

    assert_eq!(
        deltas.lock().unwrap().as_slice(),
        ["正在联网查询中…\n\n", "你", "好"]
    );
    assert_eq!(response.text.as_deref(), Some("【联网查询】\n\n你好"));
    assert_eq!(query_calls.load(Ordering::SeqCst), 0);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.diagnostics.unwrap()["search_tool"], "web_search");
}

#[tokio::test]
async fn web_search_command_accepts_compact_chinese_form_without_space() {
    let service = test_service();

    let response = service.respond(message("/查今日ai圈新闻")).await.unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("web answer: 今日ai圈新闻")
    );
    assert!(
        response
            .markdown
            .as_deref()
            .unwrap()
            .contains("web answer: 今日ai圈新闻")
    );
    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert_eq!(response.diagnostics.unwrap()["used_search"], true);
}

#[tokio::test]
async fn web_search_command_returns_visible_error_on_query_failure() {
    let (service, _base) = test_service_with_provider_base_title_and_query(
        MockProvider::new(),
        None,
        Arc::new(FailingWebSearchExecutor {
            err: LlmError::http("OpenAI web query request failed"),
        }),
    );

    let response = service.respond(message("/查 keyword")).await.unwrap();

    assert!(response.ok);
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("联网查询服务暂时不可用"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("联网查询服务暂时不可用"))
    );
    assert_eq!(response.command.as_deref(), Some("web_search"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_search"], true);
    assert_eq!(diagnostics["query_error_code"], "http_error");
    assert_eq!(diagnostics["query_error_stage"], "http");
}

#[tokio::test]
async fn web_search_command_rejects_empty_argument() {
    let service = test_service();

    let response = service.respond(message("/查")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("用法：/查 关键词")
    );
}

#[tokio::test]
async fn web_search_command_rewrites_overlong_argument_before_querying() {
    let provider = MockProvider::with_search_query_rewrite_replies(vec![Ok(
        "Rust E0502 borrow checker Vec push immutable borrow 1.75",
    )]);
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider.clone(), None, Arc::new(executor));
    let query = format!(
        "{} Rust 编译报错排查，最后的关键限制是版本 1.75 和 E0502",
        "背景说明".repeat(80)
    );

    let response = service
        .respond(message(&format!("/查 {query}")))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("web_search"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].query,
        "Rust E0502 borrow checker Vec push immutable borrow 1.75"
    );
    assert!(
        requests[0]
            .raw_question
            .as_deref()
            .unwrap()
            .contains("E0502")
    );
    assert_eq!(
        provider.requests()[0]
            .metadata
            .get("purpose")
            .map(String::as_str),
        Some("search_query_rewrite")
    );
}

#[tokio::test]
async fn web_search_command_keeps_short_query_without_rewrite() {
    let provider = MockProvider::new();
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider.clone(), None, Arc::new(executor));

    service.respond(message("/查 Rust 新闻")).await.unwrap();

    assert_eq!(requests.lock().unwrap()[0].query, "Rust 新闻");
    assert!(
        provider.requests().is_empty(),
        "短 query 不应额外调用模型 rewrite"
    );
}

#[tokio::test]
async fn web_search_command_keeps_clear_short_query_with_quote_without_rewrite() {
    let provider = MockProvider::new();
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider.clone(), None, Arc::new(executor));
    let mut req = message("/查 Rust 新闻");
    req.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        text_summary: Some("一段引用上下文，不应让明确短 query 多打一轮模型。".to_owned()),
        ..QuotedMessageContext::default()
    });

    service.respond(req).await.unwrap();

    assert_eq!(requests.lock().unwrap()[0].query, "Rust 新闻");
    assert!(
        provider.requests().is_empty(),
        "明确短 query 即使带引用也不应额外调用模型 rewrite"
    );
}

#[tokio::test]
async fn web_search_command_rewrites_short_followup_with_quoted_context() {
    let provider = MockProvider::with_search_query_rewrite_replies(vec![Ok(
        "Cloudflare D1 binding DB not configured Wrangler 4.22.0",
    )]);
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
    let mut req = message("/查 帮我查询一下");
    req.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        text_summary: Some(format!(
            "{}\n错误：Cloudflare D1 binding DB is not configured\n版本：Wrangler 4.22.0",
            "无关上下文".repeat(120)
        )),
        ..QuotedMessageContext::default()
    });

    service.respond(req).await.unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0].query,
        "Cloudflare D1 binding DB not configured Wrangler 4.22.0"
    );
    assert!(
        requests[0]
            .raw_question
            .as_deref()
            .unwrap()
            .contains("引用消息上下文")
    );
}

#[tokio::test]
async fn web_search_command_accepts_numeric_version_rewrite_outputs() {
    for reply in [
        "1.75 Rust E0502 borrow checker Vec push",
        "4.22.0 Wrangler D1 binding DB not configured",
        "qq-maid-bot 联网查询 200 字限制",
    ] {
        let provider = MockProvider::with_search_query_rewrite_replies(vec![Ok(reply)]);
        let executor = RecordingWebSearchExecutor::default();
        let requests = executor.requests();
        let (service, _base) =
            test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
        let query = format!(
            "帮我查询一下 {} 最后条件 Rust E0502 Vec push Wrangler D1 binding DB",
            "上下文 ".repeat(90)
        );

        service
            .respond(message(&format!("/查 {query}")))
            .await
            .unwrap();

        assert_eq!(requests.lock().unwrap()[0].query, reply);
    }
}

#[tokio::test]
async fn web_search_command_falls_back_when_rewrite_model_fails() {
    let provider =
        MockProvider::with_search_query_rewrite_replies(vec![Err(LlmError::timeout("rewrite"))]);
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
    let query = format!(
        "帮我查一下 {} 关键错误码 TAIL-KEY-9876",
        "很多背景 ".repeat(80)
    );

    service
        .respond(message(&format!("/查 {query}")))
        .await
        .unwrap();

    let query = requests.lock().unwrap()[0].query.clone();
    assert!(query.chars().count() <= 200);
    assert!(query.contains("TAIL-KEY-9876"));
    assert!(!query.contains("帮我查一下"));
}

#[tokio::test]
async fn web_search_command_fallback_extracts_key_terms_from_quoted_context() {
    let provider =
        MockProvider::with_search_query_rewrite_replies(vec![Err(LlmError::timeout("rewrite"))]);
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
    let mut req = message("/查 整理下上下文查询一下");
    req.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        text_summary: Some(format!(
            "{}\n标题：OpenAI Responses API web_search 报错\nURL：https://platform.openai.com/docs/guides/tools-web-search\n错误码：invalid_request_error 400\n版本：gpt-5-mini",
            "普通讨论 ".repeat(100)
        )),
        ..QuotedMessageContext::default()
    });

    service.respond(req).await.unwrap();

    let query = requests.lock().unwrap()[0].query.clone();
    assert!(query.chars().count() <= 200);
    assert!(query.contains("OpenAI Responses API web_search"));
    assert!(query.contains("https://platform.openai.com/docs/guides/tools-web-search"));
    assert!(query.contains("invalid_request_error 400"));
    assert!(!query.contains("整理下上下文查询一下"));
}

#[tokio::test]
async fn web_search_command_falls_back_when_rewrite_output_is_invalid() {
    for reply in [
        String::new(),
        "x".repeat(201),
        "我将搜索 Rust E0502".to_owned(),
        "我来帮你联网查询 Rust E0502".to_owned(),
        "- Rust E0502".to_owned(),
        "1. Rust E0502".to_owned(),
    ] {
        let provider = MockProvider::with_search_query_rewrite_replies(vec![Ok(reply.as_str())]);
        let executor = RecordingWebSearchExecutor::default();
        let requests = executor.requests();
        let (service, _base) =
            test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
        let query = format!(
            "帮我查询一下 {} 最后条件 Rust E0502 Vec push",
            "上下文 ".repeat(90)
        );

        service
            .respond(message(&format!("/查 {query}")))
            .await
            .unwrap();

        let final_query = requests.lock().unwrap()[0].query.clone();
        assert!(final_query.chars().count() <= 200, "{reply}");
        assert!(final_query.contains("Rust E0502 Vec push"), "{reply}");
        assert!(!final_query.contains("我将搜索"), "{reply}");
        assert!(!final_query.contains("我来帮你联网查询"), "{reply}");
        assert!(!final_query.starts_with("- "), "{reply}");
        assert!(!final_query.starts_with("1. "), "{reply}");
    }
}

#[tokio::test]
async fn web_search_stream_rewrites_overlong_argument_before_querying() {
    let provider = MockProvider::with_search_query_rewrite_replies(vec![Ok(
        "Tokio timeout JoinHandle abort stream regression",
    )]);
    let executor = RecordingWebSearchExecutor::default();
    let requests = executor.requests();
    let (service, _base) =
        test_service_with_provider_base_title_and_query(provider, None, Arc::new(executor));
    let deltas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected = deltas.clone();
    let query = format!(
        "联网查询 {} 关键限制 Tokio JoinHandle abort stream regression",
        "长背景 ".repeat(90)
    );

    service
        .respond_stream(message(&format!("/查 {query}")), move |delta| {
            let collected = collected.clone();
            Box::pin(async move {
                collected.lock().unwrap().push(delta);
                Ok(())
            })
        })
        .await
        .unwrap();

    assert_eq!(
        requests.lock().unwrap()[0].query,
        "Tokio timeout JoinHandle abort stream regression"
    );
    assert!(
        deltas
            .lock()
            .unwrap()
            .iter()
            .any(|delta| delta.contains("stream answer"))
    );
}

#[tokio::test]
async fn web_search_command_surfaces_timeout_error() {
    let (service, _base) = test_service_with_provider_base_title_and_query(
        MockProvider::new(),
        None,
        Arc::new(FailingWebSearchExecutor {
            err: LlmError::timeout("query"),
        }),
    );

    let response = service.respond(message("/查 keyword")).await.unwrap();

    assert!(response.ok);
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("联网查询超时了"));
    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert_eq!(response.diagnostics.unwrap()["query_error_code"], "timeout");
}

#[tokio::test]
async fn web_search_command_keeps_private_and_group_paths_equivalent() {
    let private_service = test_service();
    let group_service = test_service();

    let private = private_service
        .respond(message("/查 keyword"))
        .await
        .unwrap();
    let group = group_service.respond(message("/查 keyword")).await.unwrap();

    assert_eq!(private.command, group.command);
    assert_eq!(private.diagnostics.unwrap()["used_search"], true);
    assert_eq!(group.diagnostics.unwrap()["used_search"], true);
}
