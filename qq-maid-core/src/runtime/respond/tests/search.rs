use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use qq_maid_llm::web_search::{
    WebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource,
};

use super::support::*;
use crate::error::LlmError;

struct LongAnswerWebSearchExecutor;

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
async fn web_search_command_rejects_overlong_argument() {
    let service = test_service();
    let query = "a".repeat(201);

    let response = service
        .respond(message(&format!("/查 {query}")))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("web_search"));
    assert!(response.text.as_deref().unwrap().contains("查询内容太长了"));
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
