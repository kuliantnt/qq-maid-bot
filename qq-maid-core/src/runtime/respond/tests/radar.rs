use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::support::*;

#[tokio::test]
async fn radar_command_accepts_rader_alias_and_returns_overview() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::with_counter(provider_calls.clone()),
        None,
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        None,
        None,
        None,
    );

    let response = service.respond(message("/rader")).await.unwrap();
    let text = response.text.clone().unwrap();
    let markdown = response.markdown.clone().unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(text.starts_with("🛰️ AI 雷达速览"));
    assert!(text.contains("Codex：社区确认"));
    assert!(text.contains("Claude Code：正常"));
    assert!(text.contains("关键指标：额度：20x Pro 5h 281.91 / 7d 1691.46"));
    assert!(text.contains("最高模型：GPT-5.4 xhigh · IQ 90 · 略低 · 6/10"));
    assert!(text.contains("24h 评分 Fable 5 xhigh 9.10 · 样本 9"));
    assert!(text.contains("详细看 /rader codex 或 /rader claude"));
    assert!(!text.contains("未返回"));
    assert!(markdown.starts_with("# 🛰️ AI 雷达速览"));
    assert!(markdown.contains("## Codex Radar"));
    assert!(markdown.contains("## Claude Code Radar"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_radar"], true);
    assert_eq!(diagnostics["radar_target"], "all");
    assert_eq!(diagnostics["radar_provider"], "mock-radar");
}

#[tokio::test]
async fn radar_command_accepts_correct_spelling_alias() {
    let (service, _) = test_service_with_base();

    let response = service.respond(message("/radar")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(response.text.unwrap().contains("AI 雷达速览"));
}

#[tokio::test]
async fn radar_command_accepts_chinese_alias() {
    let (service, _) = test_service_with_base();

    let response = service.respond(message("/雷达")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(response.text.unwrap().contains("AI 雷达速览"));
}

#[tokio::test]
async fn radar_command_can_show_only_codex() {
    let (service, _) = test_service_with_base();

    let response = service.respond(message("/rader codex")).await.unwrap();
    let text = response.text.unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(text.starts_with("🛰️ Codex Radar"));
    assert!(text.contains("结论"));
    assert!(text.contains("Codex：社区确认"));
    assert!(text.contains("短线判断"));
    assert!(text.contains("短线概率：偏高 · 24h 36%"));
    assert!(text.contains("额度：20x Pro 5h 281.91 / 7d 1691.46"));
    assert!(text.contains("模型体感：GPT-5.5 xhigh · IQ 60 · 偏低 · 4/10"));
    assert!(text.contains("最高模型：GPT-5.4 xhigh · IQ 90 · 略低 · 6/10"));
    assert!(text.contains("完整模型列表："));
    assert!(!text.contains("community_confirmed"));
    assert!(!text.contains("reset_completed"));
    assert!(!text.contains("red"));
    assert!(!text.contains("Claude Code Radar"));
    assert_eq!(response.diagnostics.unwrap()["radar_target"], "codex");
}

#[tokio::test]
async fn radar_command_can_show_only_claude() {
    let (service, _) = test_service_with_base();

    let response = service.respond(message("/rader claude")).await.unwrap();
    let text = response.text.unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(text.starts_with("🛰️ Claude Code Radar"));
    assert!(text.contains("Claude Code：正常"));
    assert!(text.contains("额度：5h 332.29 / 7d 2270.63"));
    assert!(text.contains("用量 5h：当前 5h 共享池 已用 41% · 13:00 重置"));
    assert!(text.contains("IQ 最高模型：Fable 5 xhigh 120 · 8/10"));
    assert!(text.contains("24h 社区评分：Fable 5 xhigh 9.10 · 样本 9"));
    assert!(!text.contains("未返回"));
    assert!(!text.contains("Codex Radar"));
    assert_eq!(response.diagnostics.unwrap()["radar_target"], "claude");
}

#[tokio::test]
async fn radar_issue_codex_returns_feedback_entry_without_external_call() {
    let (service, _) = test_service_with_base();

    let response = service
        .respond(message("/rader issue codex"))
        .await
        .unwrap();
    let text = response.text.clone().unwrap();
    let markdown = response.markdown.clone().unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(text.contains("Codex Radar 反馈"));
    assert!(text.contains("反馈入口：https://codexradar.com/"));
    assert!(text.contains("当前未发现该站点公开 GitHub Issue 仓库"));
    assert!(markdown.starts_with("# Codex Radar 反馈"));
}

#[tokio::test]
async fn radar_issue_claude_returns_feedback_entry_without_external_call() {
    let (service, _) = test_service_with_base();

    let response = service
        .respond(message("/rader issue claude"))
        .await
        .unwrap();
    let text = response.text.clone().unwrap();
    let markdown = response.markdown.clone().unwrap();

    assert_eq!(response.command.as_deref(), Some("radar"));
    assert!(text.contains("Claude Code Radar 反馈"));
    assert!(text.contains("反馈入口：https://claudecoderadar.com/"));
    assert!(markdown.starts_with("# Claude Code Radar 反馈"));
}
