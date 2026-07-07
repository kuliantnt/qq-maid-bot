use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::Value;
use tokio::time::{Duration, sleep};

use super::support::*;
use crate::{
    error::LlmError,
    provider::types::ChatRequest,
    runtime::session::{DEFAULT_SESSION_TITLE, SessionMeta},
};

async fn wait_for_session_title(
    service: &crate::runtime::respond::RustRespondService,
    title: &str,
) {
    for _ in 0..50 {
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        if session.title == title {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, title);
}

async fn wait_for_title_request_count(inspector: &MockProvider, expected: usize) {
    for _ in 0..50 {
        let count = inspector
            .requests()
            .iter()
            .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
            .count();
        if count == expected {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    let count = inspector
        .requests()
        .iter()
        .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .count();
    assert_eq!(count, expected);
}

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
async fn help_all_lists_public_commands_by_module() {
    let response = test_service().respond(message("/help ALL")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    for heading in [
        "💬 对话",
        "✅ 待办",
        "📰 RSS / Atom",
        "🌤 天气",
        "🔎 联网查询",
        "🌐 翻译",
        "🧠 长期记忆",
        "🗂 会话",
        "🩺 状态与诊断",
    ] {
        assert!(text.contains(heading), "missing help heading: {heading}");
        assert!(
            markdown.contains(&format!("## {heading}")),
            "missing markdown help heading: {heading}"
        );
    }
    for command in [
        "/todo undo",
        "/rss add",
        "/rss delete",
        "/rss test",
        "/memory edit",
        "/resume",
        "/ping",
    ] {
        assert!(text.contains(command), "missing help command: {command}");
    }
    assert!(text.chars().count() <= 1800);
    assert_unimplemented_rss_commands_absent(&text);
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
        "/rss add RSS地址 [名称]",
        "/rss delete 编号或订阅ID",
        "/rss test RSS地址",
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
    assert!(text.contains("Tool 调用"));
    assert!(text.contains("自然语言"));
    assert!(markdown.starts_with("# ✅ 待办帮助"));
    assert!(markdown.contains("`/todo done`"));
    assert!(markdown.contains("Tool 调用"));
}

#[tokio::test]
async fn unknown_help_module_returns_available_modules() {
    let response = test_service().respond(message("/help abc")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("未找到帮助模块：abc"));
    assert!(text.contains("可用模块："));
    assert!(text.contains("rss"));
    assert!(text.contains("输入 /help 查看功能总览"));
    assert!(markdown.contains("未找到帮助模块：`abc`"));
    assert!(markdown.contains("`rss`"));
    assert!(markdown.contains("输入 `/help` 查看功能总览"));
}

#[tokio::test]
async fn set_display_name_roundtrip_and_unset() {
    let service = test_service();

    let response = service.respond(message("/set 昵称 脸脸")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("展示名已设置"));
    assert!(text.contains("脸脸"));
    assert!(text.contains("不代表现实身份认证"));

    let response = service.respond(message("/set 昵称")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("当前展示名"));
    assert!(text.contains("脸脸"));

    let response = service.respond(message("/unset 昵称")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("unset"));
    assert!(text.contains("展示名已清除"));

    let response = service.respond(message("/set 昵称")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("还没有设置展示名"));
}

#[tokio::test]
async fn set_display_name_rejects_invalid_values() {
    let service = test_service();

    let response = service
        .respond(message(&format!("/set 昵称 {}", "a".repeat(33))))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("展示名无效"));
    assert!(text.contains("32 个字符以内"));

    let response = service.respond(message("/set 昵称    ")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("还没有设置展示名") || text.contains("用法"));
}

#[tokio::test]
async fn set_display_name_rejects_missing_current_user_id() {
    let service = test_service();
    service.respond(message("A 先创建群会话")).await.unwrap();

    let mut req = message("/set 昵称 无身份用户");
    req.user_id = None;
    let response = service.respond(req).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("展示名设置失败"));
    assert!(text.contains("缺少稳定身份"));

    let response = service.respond(message("/set 昵称")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("还没有设置展示名"));
    assert!(!text.contains("无身份用户"));
}

#[tokio::test]
async fn manual_display_name_overrides_platform_name_in_message_context() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service.respond(message("/set 昵称 脸脸")).await.unwrap();

    let req = message_with_actor_context("你知道我是谁吗", "group:g1", "g1", "u1", "平台昵称");
    service.respond(req).await.unwrap();
    let joined = last_chat_request_text(&inspector);
    assert!(joined.contains("昵称=脸脸"));
    assert!(joined.contains("昵称来源=manual"));
    assert!(!joined.contains("昵称=平台昵称"));
}

#[tokio::test]
async fn manual_display_name_uses_request_user_id_when_message_context_actor_missing() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(message_in_scope("/set 昵称 雪雪", "group:g1", "u1", "g1"))
        .await
        .unwrap();

    let mut req = message_in_scope("我是谁？", "group:g1", "u1", "g1");
    // 模拟成员详情接口不可用或旧入口未能给 LLM 上下文补 actor：权威 req.user_id 仍应可用于读取本地展示名。
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        actor: None,
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "group".to_owned(),
            id: Some("g1".to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    service.respond(req).await.unwrap();
    let joined = last_chat_request_text(&inspector);
    assert!(joined.contains("昵称=雪雪"));
    assert!(joined.contains("昵称来源=manual"));
    assert!(joined.contains("稳定ID=u1"));

    service
        .respond(message_in_scope("/unset 昵称", "group:g1", "u1", "g1"))
        .await
        .unwrap();
    let mut req = message_in_scope("我是谁？", "group:g1", "u1", "g1");
    req.message_context = None;
    service.respond(req).await.unwrap();
    let joined = last_chat_request_text(&inspector);
    assert!(!joined.contains("昵称=雪雪"));
    assert!(!joined.contains("昵称来源=manual"));
}

#[tokio::test]
async fn manual_display_name_does_not_grant_group_management_permission() {
    let service = test_service();

    service
        .respond(message_in_scope("/set 昵称 群主", "group:g1", "u2", "g1"))
        .await
        .unwrap();

    let mut req = message_in_scope(
        "/rss add http://127.0.0.1:9/feed.xml 测试订阅",
        "group:g1",
        "u2",
        "g1",
    );
    req.group_member_role = Some("member".to_owned());
    let response = service.respond(req).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("group_admin_required"));
    assert!(response.text.unwrap().contains("群主或管理员"));
}

#[tokio::test]
async fn group_manual_display_names_are_isolated_by_current_actor_user_id() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    // A 先创建群聊 conversation session，随后 B 的 /set 仍必须绑定到本轮发言人 B。
    service
        .respond(message_with_actor_context(
            "A 先发言",
            "group:g1",
            "g1",
            "u1",
            "平台A",
        ))
        .await
        .unwrap();
    service
        .respond(message_in_scope("/set 昵称 小A", "group:g1", "u1", "g1"))
        .await
        .unwrap();
    service
        .respond(message_in_scope("/set 昵称 小B", "group:g1", "u2", "g1"))
        .await
        .unwrap();

    let response = service
        .respond(message_in_scope("/set 昵称", "group:g1", "u2", "g1"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("当前展示名"));
    assert!(text.contains("小B"));
    assert!(!text.contains("小A"));

    service
        .respond(message_with_actor_context(
            "B 问一下",
            "group:g1",
            "g1",
            "u2",
            "平台B",
        ))
        .await
        .unwrap();
    let joined = last_chat_request_text(&inspector);
    assert!(joined.contains("昵称=小B"));
    assert!(joined.contains("昵称来源=manual"));
    assert!(!joined.contains("昵称=平台B"));
    assert!(!joined.contains("昵称=小A"));

    service
        .respond(message_in_scope("/unset 昵称", "group:g1", "u2", "g1"))
        .await
        .unwrap();

    let response = service
        .respond(message_in_scope("/set 昵称", "group:g1", "u1", "g1"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("当前展示名"));
    assert!(text.contains("小A"));
    assert!(!text.contains("小B"));

    let response = service
        .respond(message_in_scope("/set 昵称", "group:g1", "u2", "g1"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("还没有设置展示名"));
    assert!(!text.contains("小A"));
}

fn message_with_actor_context(
    text: &str,
    scope_key: &str,
    group_id: &str,
    user_id: &str,
    platform_name: &str,
) -> crate::runtime::respond::RespondRequest {
    let mut req = message_in_scope(text, scope_key, user_id, group_id);
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        actor: Some(qq_maid_common::identity_context::MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(platform_name.to_owned()),
            display_name_source: Some("event".to_owned()),
            source: qq_maid_common::identity_context::IdentitySource::Event,
            ..Default::default()
        }),
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "group".to_owned(),
            id: Some(group_id.to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    req
}

fn last_chat_request_text(inspector: &MockProvider) -> String {
    request_text(
        inspector
            .requests()
            .iter()
            .rev()
            .find(|req| req.metadata.get("purpose").map(String::as_str) != Some("session_title"))
            .expect("missing chat request"),
    )
}

fn request_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| {
            let mut texts = vec![message.content.as_str()];
            for part in &message.content_parts {
                if let qq_maid_common::input_part::MessageInputPart::Text { text, .. } = part {
                    texts.push(text.as_str());
                }
            }
            texts
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_unimplemented_rss_commands_absent(text: &str) {
    for command in ["/rss refresh", "/rss enable", "/rss disable", "/rss edit"] {
        assert!(
            !text.contains(command),
            "unimplemented RSS command leaked into help: {command}"
        );
    }
}

#[tokio::test]
async fn resume_without_argument_lists_recent_sessions() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/resume")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("最近会话"));
    assert!(text.contains("旧话题"));
    assert!(text.contains("使用 /resume 1 恢复"));
    assert!(markdown.contains("最近会话"));
    assert!(markdown.contains("1. 旧话题"));
}

#[tokio::test]
async fn resume_number_restores_selected_session() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/resume 1")).await.unwrap();

    assert!(response.text.unwrap().contains("已恢复会话：旧话题"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("- 话题："))
    );
    assert_eq!(response.command.as_deref(), Some("resume"));
}

#[tokio::test]
async fn chinese_resume_alias_matches_resume() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/恢复")).await.unwrap();

    assert!(response.text.unwrap().contains("旧话题"));
}

#[tokio::test]
async fn list_is_deprecated_alias() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/list")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("最近会话"));
    assert!(text.contains("已不推荐"));
    assert!(markdown.contains("提示：/list 已不推荐"));
}

#[tokio::test]
async fn new_without_argument_creates_default_title() {
    let service = test_service();

    service.respond(message("/new")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert!(session.state.get("current_topic").is_none());
}

#[tokio::test]
async fn new_with_argument_keeps_user_title() {
    let service = test_service();

    service.respond(message("/new 示例材料")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "示例材料");
    assert_eq!(
        session.state.get("current_topic").and_then(Value::as_str),
        Some("示例材料")
    );
}

#[tokio::test]
async fn first_chat_does_not_use_raw_user_text_as_title() {
    let service = test_service();
    let user_text = "整理一下今天的部署方案，顺便确认启动脚本和环境变量说明";

    service.respond(message(user_text)).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert!(session.state.get("current_topic").is_none());
}

#[tokio::test]
async fn title_model_absent_disables_auto_title_and_rename_generation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    service.respond(message("第一条部署问题")).await.unwrap();
    service.respond(message("第二条日志线索")).await.unwrap();
    let rename = service.respond(message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert_eq!(rename.text.as_deref(), Some("当前未配置标题生成模型。"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn auto_title_retries_after_failure_and_uses_per_call_model() {
    let provider =
        MockProvider::with_title_replies(vec![Ok(DEFAULT_SESSION_TITLE), Ok("部署排障")]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条部署问题")).await.unwrap();
    service.respond(message("第二条日志线索")).await.unwrap();
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );

    service.respond(message("第三条确认方案")).await.unwrap();
    wait_for_session_title(&service, "部署排障").await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "部署排障");
    assert!(
        !serde_json::to_string(&session)
            .unwrap()
            .contains("title-model")
    );

    let requests = inspector.requests();
    let title_requests = requests
        .iter()
        .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .collect::<Vec<_>>();
    assert_eq!(title_requests.len(), 2);
    assert!(
        title_requests
            .iter()
            .all(|req| req.model.as_deref() == Some("title-model"))
    );
    assert!(
        requests
            .iter()
            .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("chat"))
            .all(|req| req.model.as_deref() == Some("mock-model"))
    );
}

#[tokio::test]
async fn auto_title_delay_does_not_block_chat_response() {
    let provider = MockProvider::with_title_replies(vec![Ok("后台标题")])
        .with_title_delay(Duration::from_millis(300));
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    let response = tokio::time::timeout(
        Duration::from_millis(100),
        service.respond(message("第二条触发标题")),
    )
    .await
    .expect("chat response should not wait for auto title")
    .unwrap();

    assert_eq!(response.text.as_deref(), Some("回复：第二条触发标题"));
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );
    wait_for_session_title(&service, "后台标题").await;
}

#[tokio::test]
async fn delayed_auto_title_preserves_messages_saved_after_snapshot() {
    let provider =
        MockProvider::with_title_replies(vec![Ok("后台标题"), Ok("后台标题"), Ok("后台标题")])
            .with_title_delay(Duration::from_millis(250));
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    service.respond(message("第二条触发标题")).await.unwrap();
    service.respond(message("第三条继续聊天")).await.unwrap();
    service.respond(message("第四条继续聊天")).await.unwrap();

    wait_for_session_title(&service, "后台标题").await;
    sleep(Duration::from_millis(300)).await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();

    assert_eq!(session.title, "后台标题");
    assert_eq!(
        session
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "第一条",
            "回复：第一条",
            "第二条触发标题",
            "回复：第二条触发标题",
            "第三条继续聊天",
            "回复：第三条继续聊天",
            "第四条继续聊天",
            "回复：第四条继续聊天",
        ]
    );
}

#[tokio::test]
async fn delayed_auto_title_does_not_overwrite_manual_rename() {
    let provider = MockProvider::with_title_replies(vec![Ok("后台标题")])
        .with_title_delay(Duration::from_millis(250));
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    service.respond(message("第二条触发标题")).await.unwrap();
    wait_for_title_request_count(&inspector, 1).await;
    service.respond(message("/rename 手工标题")).await.unwrap();

    sleep(Duration::from_millis(350)).await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();

    assert_eq!(session.title, "手工标题");
}

#[tokio::test]
async fn auto_title_failure_does_not_fail_chat_response() {
    let provider = MockProvider::with_title_replies(vec![Err(LlmError::provider(
        "title blocked",
        "provider",
    ))]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    let response = service.respond(message("第二条")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("回复：第二条"));
    wait_for_title_request_count(&inspector, 1).await;
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );
}

#[tokio::test]
async fn internal_flows_use_configured_models() {
    let provider = MockProvider::new();
    let inspector = provider.clone();
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Some("todo-internal-model".to_owned()),
        Some("memory-internal-model".to_owned()),
        Some("compact-internal-model".to_owned()),
    );

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    service
        .respond(message_in_scope("/记 喜欢清淡口味", "group:g2", "u2", "g2"))
        .await
        .unwrap();

    let compact_meta = SessionMeta::new(
        "group:g3",
        Some("u3".to_owned()),
        Some("g3".to_owned()),
        None,
        None,
        "qq_official",
    );
    let mut session = service
        .session_store
        .get_or_create_active(&compact_meta)
        .unwrap();
    service
        .session_store
        .append_exchange(&mut session, "上一轮用户消息", "上一轮助手回复")
        .unwrap();
    service
        .respond(message_in_scope("/compact", "group:g3", "u3", "g3"))
        .await
        .unwrap();

    let requests = inspector.requests();
    // Todo 写操作已统一交给 Tool Loop，不再走内部 todo_parse 模型；
    // 这里仅保留仍存在的内部模型路由校验。
    assert!(requests.iter().any(|req| {
        req.metadata.get("purpose").map(String::as_str) == Some("memory_draft")
            && req.model.as_deref() == Some("memory-internal-model")
    }));
    assert!(requests.iter().any(|req| {
        req.metadata.get("purpose").map(String::as_str) == Some("compact")
            && req.model.as_deref() == Some("compact-internal-model")
    }));
}

#[tokio::test]
async fn auto_title_stops_after_fourth_user_message() {
    let provider = MockProvider::with_title_replies(vec![
        Ok(DEFAULT_SESSION_TITLE),
        Ok(DEFAULT_SESSION_TITLE),
        Ok(DEFAULT_SESSION_TITLE),
    ]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    for text in ["第一条", "第二条", "第三条", "第四条", "第五条"] {
        service.respond(message(text)).await.unwrap();
    }
    wait_for_title_request_count(&inspector, 3).await;

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert_eq!(
        inspector
            .requests()
            .iter()
            .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
            .count(),
        3
    );
}

#[tokio::test]
async fn auto_title_does_not_overwrite_manual_title() {
    let provider = MockProvider::with_title_replies(Vec::<Result<&str, LlmError>>::new());
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("第一条部署问题")).await.unwrap();
    service.respond(message("第二条日志线索")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "手动标题");
    assert!(
        inspector.requests().iter().all(|req| {
            req.metadata.get("purpose").map(String::as_str) != Some("session_title")
        })
    );
}

#[tokio::test]
async fn rename_without_argument_can_generate_and_overwrite_title() {
    let provider = MockProvider::with_title_replies(vec![Ok("自动新标题")]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("讨论部署日志")).await.unwrap();
    let response = service.respond(message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(response.text.as_deref(), Some("已重命名为：自动新标题"));
    assert_eq!(session.title, "自动新标题");
    assert_eq!(
        session.state.get("current_topic").and_then(Value::as_str),
        Some("自动新标题")
    );
    let title_request = inspector
        .requests()
        .into_iter()
        .find(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .unwrap();
    assert_eq!(title_request.model.as_deref(), Some("title-model"));
    assert!(title_request.messages.iter().any(|message| {
        message.content.contains("用户：讨论部署日志")
            && message.content.contains("助手：回复：讨论部署日志")
    }));
}

#[tokio::test]
async fn rename_without_argument_keeps_title_on_generation_failure() {
    let provider = MockProvider::with_title_replies(vec![Ok(DEFAULT_SESSION_TITLE)]);
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("讨论部署日志")).await.unwrap();
    let response = service.respond(message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(
        response.text.as_deref(),
        Some("当前内容还不够生成标题，先保持原标题。")
    );
    assert_eq!(session.title, "手动标题");
}

#[tokio::test]
async fn resume_list_displays_default_for_dirty_titles() {
    let service = test_service();
    let meta = test_meta();
    for title in [
        "<faceType=1 faceId=2>",
        "faceId=123",
        r#"ext="eyJxxx""#,
        "[CQ:face,id=1]",
    ] {
        let mut session = service.session_store.create(&meta, "旧会话", true).unwrap();
        session.title = title.to_owned();
        service.session_store.save(&mut session).unwrap();
    }
    service.respond(message("/new 当前会话")).await.unwrap();

    let text = service
        .respond(message("/resume"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(text.matches(DEFAULT_SESSION_TITLE).count() >= 4);
    assert!(!text.contains("faceType"));
    assert!(!text.contains("faceId"));
    assert!(!text.contains("ext=\"eyJ"));
    assert!(!text.contains("[CQ:"));
}
