use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use qq_maid_llm::provider::types::ChatRole;

use super::support::*;

#[test]
fn parse_translation_command_basic_cases() {
    use super::super::translation_flow::parse_translation_command;

    // 非中文 → 默认译成简体中文
    let cmd = parse_translation_command("/翻译 hello").unwrap();
    assert_eq!(cmd.source_text, "hello");
    assert_eq!(cmd.target_language, "简体中文");
    assert_eq!(cmd.action, "translation");

    // 未显式指定目标语言时，即使原文已经是中文，也默认整理为简体中文。
    let cmd = parse_translation_command("/翻译 你好").unwrap();
    assert_eq!(cmd.target_language, "简体中文");

    // 紧跟语言词
    let cmd = parse_translation_command("/翻译日语 hello").unwrap();
    assert_eq!(cmd.target_language, "日语");
    assert_eq!(cmd.source_text, "hello");

    // 翻译成…形式
    let cmd = parse_translation_command("/翻译成英语 你好").unwrap();
    assert_eq!(cmd.target_language, "英语");
    assert_eq!(cmd.source_text, "你好");

    let cmd = parse_translation_command("/翻译 繁体中文 文本").unwrap();
    assert_eq!(cmd.target_language, "繁体中文");
    assert_eq!(cmd.source_text, "文本");

    // 无正文 → source_text 为空
    let cmd = parse_translation_command("/翻译").unwrap();
    assert!(cmd.source_text.is_empty());

    // 无斜杠 → None
    assert!(parse_translation_command("翻译 hello").is_none());
    assert!(parse_translation_command("hello").is_none());
}

#[test]
fn parse_translation_command_default_target_no_longer_depends_on_chinese_text() {
    use super::super::translation_flow::parse_translation_command;

    for text in ["/翻译 hello", "/翻译 こんにちは", "/翻译 这是一段中文"] {
        let cmd = parse_translation_command(text).unwrap();
        assert_eq!(cmd.target_language, "简体中文");
    }
}

#[tokio::test]
async fn translation_command_calls_provider_and_returns_formatted_reply() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    let response = service.respond(message("/翻译 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("【翻译·简体中文】"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("【翻译·简体中文】"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_translation"], true);
    assert_eq!(diagnostics["target_language"], "简体中文");
}

#[tokio::test]
async fn translation_command_builds_shared_translation_request() {
    let provider = MockProvider::new();
    let service = test_service_with_provider(provider.clone());

    let response = service.respond(message("/翻译 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let req = &requests[0];
    assert_eq!(req.model, None);
    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].role, ChatRole::System);
    assert!(req.messages[0].content.contains("只输出译文"));
    assert!(req.messages[0].content.contains("Markdown 结构"));
    assert_eq!(req.messages[1].role, ChatRole::User);
    assert_eq!(req.messages[1].content, "hello");
    assert_eq!(req.metadata["purpose"], "translation");
    assert_eq!(req.metadata["translation_purpose"], "command");
    assert_eq!(req.metadata["target_language"], "简体中文");
    assert_eq!(req.metadata["source_chars"], "5");
}

#[tokio::test]
async fn translation_command_uses_translation_model_when_configured() {
    let provider = MockProvider::new();
    let service = test_service_with_translation_model(
        provider.clone(),
        Some("deepseek:translation-model".to_owned()),
    );

    let response = service.respond(message("/翻译 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].model.as_deref(),
        Some("deepseek:translation-model")
    );
    assert_eq!(
        response.diagnostics.unwrap()["translation_model"],
        "deepseek:translation-model"
    );
}

#[tokio::test]
async fn translation_command_with_explicit_language() {
    let service = test_service();

    let response = service.respond(message("/翻译日语 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    assert!(response.text.as_deref().unwrap().contains("【翻译·日语】"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("【翻译·日语】"))
    );
    assert_eq!(response.diagnostics.unwrap()["target_language"], "日语");
}

#[tokio::test]
async fn translation_command_empty_argument_returns_usage_without_llm() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    let response = service.respond(message("/翻译")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    assert!(response.text.unwrap().contains("用法：/翻译"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
