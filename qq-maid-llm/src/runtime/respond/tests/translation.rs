use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::support::*;

#[test]
fn parse_translation_command_basic_cases() {
    use super::super::translation_flow::parse_translation_command;

    // 非中文 → 默认译成简体中文
    let cmd = parse_translation_command("/翻译 hello").unwrap();
    assert_eq!(cmd.source_text, "hello");
    assert_eq!(cmd.target_language, "简体中文");
    assert_eq!(cmd.action, "translation");

    // 中文 → 默认译成英语
    let cmd = parse_translation_command("/翻译 你好").unwrap();
    assert_eq!(cmd.target_language, "英语");

    // 紧跟语言词
    let cmd = parse_translation_command("/翻译日语 hello").unwrap();
    assert_eq!(cmd.target_language, "日语");
    assert_eq!(cmd.source_text, "hello");

    // 翻译成…形式
    let cmd = parse_translation_command("/翻译成英语 你好").unwrap();
    assert_eq!(cmd.target_language, "英语");
    assert_eq!(cmd.source_text, "你好");

    // 无正文 → source_text 为空
    let cmd = parse_translation_command("/翻译").unwrap();
    assert!(cmd.source_text.is_empty());

    // 无斜杠 → None
    assert!(parse_translation_command("翻译 hello").is_none());
    assert!(parse_translation_command("hello").is_none());
}

#[tokio::test]
async fn translation_command_calls_provider_and_returns_formatted_reply() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    let response = service.respond(message("/翻译 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    let text = response.text.unwrap();
    assert!(text.contains("【翻译·简体中文】"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_translation"], true);
    assert_eq!(diagnostics["target_language"], "简体中文");
}

#[tokio::test]
async fn translation_command_with_explicit_language() {
    let service = test_service();

    let response = service.respond(message("/翻译日语 hello")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("translation"));
    assert!(response.text.unwrap().contains("【翻译·日语】"));
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
