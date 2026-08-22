use qq_maid_common::input_part::{MessageInputPart, TextSource};

use super::super::support::*;
use super::support::assert_unimplemented_rss_commands_absent;

#[tokio::test]
async fn help_without_argument_returns_concise_overview() {
    let response = test_service().respond(message("/help")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(text.starts_with("女仆长助手"));
    assert!(text.contains("常用功能"));
    assert!(text.contains("/help all"));
    assert!(text.contains("/help <模块>"));
    assert!(!text.contains("`/rss test RSS地址`"));
    // 纯文本侧不能带反引号，否则 QQ 纯文本渲染会吞掉命令内容
    assert!(text.contains("✅ 待办：/todo"));
    assert!(text.contains("🎲 娱乐：/roll"));
    assert!(text.contains("/r"));
    assert!(text.contains("🩺 状态：私聊发送 /ping"));
    assert!(!text.contains('`'));
    assert!(markdown.starts_with("# 女仆长助手"));
    assert!(markdown.contains("## 常用功能"));
    assert!(markdown.contains("`/help all`"));
    assert!(markdown.contains("`/help <模块>`"));
    assert!(markdown.contains("`/todo`"));
    assert!(markdown.contains("`/ping`"));
}

#[tokio::test]
async fn custom_prefix_routes_commands_and_renders_help_consistently() {
    let service = test_service_with_command_prefix("#");

    let response = service.respond(message("#help")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();
    assert_eq!(response.command.as_deref(), Some("help"));
    assert!(text.contains("#help all"));
    assert!(text.contains("✅ 待办：#todo"));
    assert!(markdown.contains("`#memory`"));
    assert!(!markdown.contains("`/help"));

    for ordinary in ["/help", "你好 #help", "##help"] {
        let planned = service.plan_core_respond(&message(ordinary)).unwrap();
        assert_ne!(
            planned.plan(),
            crate::runtime::respond::RespondPlan::Immediate
        );
        assert_ne!(
            planned.plan(),
            crate::runtime::respond::RespondPlan::CommandEvent
        );
    }
}

#[test]
fn voice_transcript_cannot_trigger_configured_command() {
    let service = test_service_with_command_prefix("#");
    let mut request = message("");
    request.content = "[语音转文字] #ops status".to_owned();
    request.input_parts = vec![MessageInputPart::Text {
        text: request.content.clone(),
        source: Some(TextSource::Transcript),
    }];

    assert!(request.effective_command_text().is_empty());
    let planned = service.plan_core_respond(&request).unwrap();
    assert_eq!(
        planned.plan(),
        crate::runtime::respond::RespondPlan::StreamingChat
    );
}

#[tokio::test]
async fn help_all_lists_public_commands_by_module() {
    let response = test_service().respond(message("/help ALL")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    for heading in [
        "💬 对话",
        "🎲 娱乐",
        "✅ 待办",
        "📰 RSS / Atom",
        "🌤 天气",
        "🔎 联网查询",
        "🌐 翻译",
        "🧠 长期记忆",
        "🗂 会话",
        "🩺 状态与诊断",
        "🛠 运维",
    ] {
        assert!(text.contains(heading), "missing help heading: {heading}");
        assert!(
            markdown.contains(&format!("## {heading}")),
            "missing markdown help heading: {heading}"
        );
    }
    for command in [
        "/todo undo",
        "/roll",
        "/r",
        "/todo daily status",
        "/rss recent",
        "/rss add",
        "/rss delete",
        "/rss test",
        "/memory edit",
        "/memory profile",
        "/memory group",
        "/resume",
        "/ping",
        "/ops",
        "/ops list",
        "/ops cancel",
        "/ops codex",
    ] {
        assert!(text.contains(command), "missing help command: {command}");
    }
    let text_len = text.chars().count();
    // 新增公开命令时允许帮助页适度增长，同时保留上限避免内容无边界膨胀。
    assert!(
        text_len <= 2000,
        "full help text has {text_len} characters, exceeding the 2000-character limit"
    );
    assert_unimplemented_rss_commands_absent(&text);
}

#[tokio::test]
async fn help_roll_describes_dice_expressions_and_limits() {
    let response = test_service().respond(message("/help roll")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    for expected in [
        "dM",
        "NdM",
        "2d6",
        "d100",
        "1d20+3",
        "1d8+1d6+4",
        "指定骰式",
        "娱乐刻度",
        "DND5E",
        "1–100",
        "64",
        "100",
    ] {
        assert!(
            text.contains(expected),
            "missing roll help text: {expected}"
        );
        assert!(
            markdown.contains(expected),
            "missing roll help markdown: {expected}"
        );
    }
}

#[tokio::test]
async fn help_memory_describes_scopes_confirmation_and_profile_opt_out() {
    let response = test_service()
        .respond(message("/help memory"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    for expected in [
        "/memory personal",
        "/memory profile",
        "/memory group 内容",
        "/memory group list 关键词",
        "profile stop|enable",
        "新增直接写入",
        "不会自动写长期记忆",
    ] {
        assert!(text.contains(expected), "missing memory help: {expected}");
        assert!(
            markdown.contains(expected),
            "missing markdown memory help: {expected}"
        );
    }
}

#[tokio::test]
async fn help_rss_describes_current_commands_and_delivery_rules() {
    let response = test_service()
        .respond(message("  /help   RSS  "))
        .await
        .unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.starts_with("📰 RSS / Atom 帮助"));
    assert!(markdown.starts_with("# 📰 RSS / Atom 帮助"));
    for expected in [
        "/rss",
        "/rss recent [数量]",
        "/rss add RSS地址 [名称]",
        "/rss delete 编号或订阅ID",
        "/rss test RSS地址",
        "默认 5 条",
        "最多 20 条",
        "不创建订阅",
        "同时支持 RSS 和 Atom",
        "不推送历史文章",
        "按系统配置周期检查",
        "实际状态更新",
        "同一版本不会重复推送",
        "翻译失败时回退到原文",
        "常见错误",
    ] {
        assert!(text.contains(expected), "missing RSS help text: {expected}");
    }
    for expected in [
        "`/rss`",
        "`/rss recent [数量]`",
        "`/rss add RSS地址 [名称]`",
        "`/rss delete 编号或订阅ID`",
        "`/rss test RSS地址`",
    ] {
        assert!(
            markdown.contains(expected),
            "missing markdown RSS help text: {expected}"
        );
    }
    assert_unimplemented_rss_commands_absent(&text);
}

#[tokio::test]
async fn chinese_help_alias_and_module_alias_are_supported() {
    let overview = test_service().respond(message("/帮助")).await.unwrap();
    assert!(overview.text.unwrap().starts_with("女仆长助手"));
    assert!(overview.markdown.unwrap().starts_with("# 女仆长助手"));

    let module = test_service().respond(message("/帮助 订阅")).await.unwrap();
    assert!(module.text.unwrap().starts_with("📰 RSS / Atom 帮助"));
    assert!(module.markdown.unwrap().starts_with("# 📰 RSS / Atom 帮助"));
}

#[tokio::test]
async fn help_todo_returns_module_details() {
    let response = test_service().respond(message("/help todo")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.starts_with("✅ 待办帮助"));
    assert!(text.contains("/todo done"));
    assert!(text.contains("/todo daily status"));
    assert!(text.contains("Tool 调用"));
    assert!(text.contains("自然语言"));
    assert!(markdown.starts_with("# ✅ 待办帮助"));
    assert!(markdown.contains("`/todo done`"));
    assert!(markdown.contains("`/todo daily status`"));
    assert!(markdown.contains("Tool 调用"));
}

#[tokio::test]
async fn help_ops_returns_module_details() {
    let response = test_service().respond(message("/help ops")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.starts_with("🛠 运维帮助"));
    assert!(markdown.starts_with("# 🛠 运维帮助"));
    for expected in [
        "/ops",
        "/ops 命令 [参数...]",
        "/ops list",
        "/ops cancel 任务ID",
        "/ops codex 任务描述",
        "默认关闭",
        "管理员白名单",
        "固定程序",
        "Notification Outbox",
        "不走 Shell",
        "不进入机器人普通聊天 LLM / Tool Loop",
        "普通配置命令不调用模型",
        "调用程序固定配置的 Codex CLI",
    ] {
        assert!(text.contains(expected), "missing ops help text: {expected}");
    }
    for expected in [
        "`/ops`",
        "`/ops 命令 [参数...]`",
        "`/ops list`",
        "`/ops cancel 任务ID`",
        "`/ops codex 任务描述`",
    ] {
        assert!(
            markdown.contains(expected),
            "missing markdown ops help text: {expected}"
        );
    }

    let alias = test_service().respond(message("/帮助 运维")).await.unwrap();
    assert!(alias.text.unwrap().starts_with("🛠 运维帮助"));
    assert!(alias.markdown.unwrap().starts_with("# 🛠 运维帮助"));

    assert!(!text.contains("中文别名"));
    assert!(!text.contains("/运维"));
    assert!(!markdown.contains("`/运维`"));
}

#[tokio::test]
async fn unknown_help_module_returns_available_modules() {
    let response = test_service().respond(message("/help abc")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("未找到帮助模块：abc"));
    assert!(text.contains("可用模块："));
    assert!(text.contains("rss"));
    assert!(text.contains("ops"));
    assert!(text.contains("输入 /help 查看功能总览"));
    assert!(markdown.contains("未找到帮助模块：`abc`"));
    assert!(markdown.contains("`rss`"));
    assert!(markdown.contains("`ops`"));
    assert!(markdown.contains("输入 `/help` 查看功能总览"));
}
