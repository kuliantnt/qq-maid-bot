use std::sync::Arc;

use super::support::*;
use crate::error::LlmError;

#[tokio::test]
async fn web_search_command_uses_query_executor() {
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
        Arc::new(FailingQueryExecutor {
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
