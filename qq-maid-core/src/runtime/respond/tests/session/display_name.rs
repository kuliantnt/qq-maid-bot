use super::super::support::*;
use super::support::{last_chat_request_text, message_with_actor_context};

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
async fn nn_alias_reuses_set_display_name_flow() {
    let service = test_service();

    let response = service.respond(message("/nn emmm")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("展示名已设置"));
    assert!(text.contains("emmm"));

    let response = service.respond(message("/nn")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("当前展示名"));
    assert!(text.contains("emmm"));

    let response = service.respond(message(".nn")).await.unwrap();
    let text = response.text.unwrap();
    assert_eq!(response.command.as_deref(), Some("set"));
    assert!(text.contains("当前展示名"));
    assert!(text.contains("emmm"));
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
        current_actor_ref: None,
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
