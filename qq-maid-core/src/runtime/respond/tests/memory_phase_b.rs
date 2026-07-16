use super::{super::interaction_state::respond_interaction_meta, support::*};

use crate::runtime::tools::memory::{
    CreateMemoryRequest, MemoryActor, MemoryOperations, MemoryQuery, MemoryTarget,
};

fn actor(user: &str, group: Option<&str>, admin: bool) -> MemoryActor {
    MemoryActor::from_context(
        Some(user.to_owned()),
        Some(user.to_owned()),
        group.map(str::to_owned),
        admin,
    )
    .unwrap()
}

fn active_count(
    service: &crate::runtime::respond::RustRespondService,
    actor: &MemoryActor,
    target: MemoryTarget,
) -> usize {
    MemoryOperations::new(service.memory_store.clone())
        .list(actor, MemoryQuery::active(target))
        .unwrap()
        .len()
}

#[tokio::test]
async fn private_personal_memory_writes_directly_without_pending() {
    let service = test_service();
    let request = private_message("/memory 我不喜欢太长的回复");
    let response = service.respond(request).await.unwrap();
    assert!(response.text.unwrap().contains("🧠 已记住"));

    let user = actor("u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        1
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());

    // 新增没有 Pending；确认词只能作为普通聊天，不能重复写入。
    service.respond(private_message("确认")).await.unwrap();
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        1
    );
}

#[tokio::test]
async fn group_profile_and_public_memory_are_routed_to_exact_targets() {
    let service = test_service();
    let profile = service
        .respond(message("/memory 在这个群叫我棒冰"))
        .await
        .unwrap();
    assert!(profile.text.unwrap().contains("范围：当前群画像"));

    let user = actor("u1", Some("g1"), true);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        1
    );
    assert_eq!(active_count(&service, &user, MemoryTarget::group("g1")), 0);

    let group = service
        .respond(message("/memory group add 每周五晚上进行项目周会"))
        .await
        .unwrap();
    assert!(group.text.unwrap().contains("范围：当前群公共记忆"));
    assert_eq!(active_count(&service, &user, MemoryTarget::group("g1")), 1);
}

#[tokio::test]
async fn ambiguous_group_scope_clarifies_and_pending_is_actor_isolated() {
    let service = test_service();
    let response = service
        .respond(message("/memory 范围不明确示例"))
        .await
        .unwrap();
    assert!(response.text.unwrap().contains("保存范围不够明确"));

    // 同群 B 的“画像”不会消费、修订或确认 A 的 interaction pending。
    service
        .respond(message_in_scope("画像", "group:g1", "u2", "g1"))
        .await
        .unwrap();
    let u1 = actor("u1", Some("g1"), true);
    assert_eq!(
        active_count(&service, &u1, MemoryTarget::group_profile("g1", "u1")),
        0
    );

    let saved = service.respond(message("画像")).await.unwrap();
    assert!(saved.text.unwrap().contains("范围：当前群画像"));
    assert_eq!(
        active_count(&service, &u1, MemoryTarget::group_profile("g1", "u1")),
        1
    );
}

#[tokio::test]
async fn sensitive_group_instruction_is_rejected_without_pending() {
    let service = test_service();
    let request = message("/memory 在这个群记住我的身份证号 11010519491231002X");
    let interaction_meta = respond_interaction_meta(&request);
    let response = service.respond(request).await.unwrap();
    assert!(response.text.unwrap().contains("不创建可提交草稿"));
    assert!(
        service
            .session_store
            .get_active(&interaction_meta)
            .unwrap()
            .unwrap()
            .pending_operation
            .is_none()
    );
}

#[tokio::test]
async fn direct_create_does_not_leave_confirmation_pending() {
    let service = test_service();
    let saved = service
        .respond(private_message("/memory 我不喜欢太长的回复"))
        .await
        .unwrap();
    assert!(saved.text.unwrap().contains("🧠 已记住"));

    let user = actor("u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        1
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn profile_opt_out_blocks_writes_until_explicit_reauthorization() {
    let service = test_service();
    service
        .respond(message("/memory 在这个群叫我棒冰"))
        .await
        .unwrap();
    let stop = service
        .respond(message("/memory profile stop"))
        .await
        .unwrap();
    assert!(stop.text.unwrap().contains("停止当前群继续保存"));
    service.respond(message("确认")).await.unwrap();

    let user = actor("u1", Some("g1"), true);
    let target = MemoryTarget::group_profile("g1", "u1");
    assert_eq!(active_count(&service, &user, target.clone()), 0);

    let blocked = service
        .respond(message("/memory profile 在这个群叫我雪糕"))
        .await
        .unwrap();
    assert!(blocked.text.unwrap().contains("已停止当前群保存群内画像"));
    assert_eq!(active_count(&service, &user, target.clone()), 0);

    service
        .respond(message("/memory profile enable"))
        .await
        .unwrap();
    let enabled = service.respond(message("确认")).await.unwrap();
    assert!(enabled.text.unwrap().contains("已重新授权"));

    service
        .respond(message("/memory profile 在这个群叫我雪糕"))
        .await
        .unwrap();
    assert_eq!(active_count(&service, &user, target), 1);
}

#[tokio::test]
async fn list_and_detail_are_friendly_without_internal_fields_or_id() {
    let service = test_service();
    service
        .respond(private_message("/memory 我不喜欢太长的回复"))
        .await
        .unwrap();
    let user = actor("u1", None, false);
    let records = MemoryOperations::new(service.memory_store.clone())
        .list(&user, MemoryQuery::active(MemoryTarget::personal("u1")))
        .unwrap();
    let internal_id = records[0].id.clone();
    let short_id = internal_id.chars().take(8).collect::<String>();

    let list = service
        .respond(private_message("/memory"))
        .await
        .unwrap()
        .text
        .unwrap();
    for expected in ["🧠 个人记忆", "共 1 条", "1 ", "可用操作", "/记忆 查看 1"] {
        assert!(list.contains(expected), "列表缺少字段：{expected}");
    }
    for internal in ["preference", "private", "active", "owner_key", "scope_key"] {
        assert!(!list.contains(internal), "列表泄露内部字段：{internal}");
    }
    assert!(!list.contains(&internal_id));
    assert!(!list.contains(&short_id));

    let detail = service
        .respond(private_message("/memory show 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    for expected in ["🧠 记忆详情", "范围：个人记忆", "内容：", "创建："] {
        assert!(detail.contains(expected), "详情缺少字段：{expected}");
    }
    for internal in ["preference", "private", "active", "owner_key", "scope_key"] {
        assert!(!detail.contains(internal), "详情泄露内部字段：{internal}");
    }
    assert!(!detail.contains(&internal_id));
    assert!(!detail.contains(&short_id));
}

#[tokio::test]
async fn clear_freezes_objects_and_requires_confirmation() {
    let service = test_service();
    for content in ["第一条待清空记忆", "第二条待清空记忆"] {
        service
            .respond(private_message(&format!("/memory personal {content}")))
            .await
            .unwrap();
    }
    let user = actor("u1", None, false);
    let target = MemoryTarget::personal("u1");
    assert_eq!(active_count(&service, &user, target.clone()), 2);

    let prepared = service
        .respond(private_message("/memory clear"))
        .await
        .unwrap();
    assert!(prepared.text.unwrap().contains("将清空个人中的 2 条"));
    assert_eq!(active_count(&service, &user, target.clone()), 2);

    let confirmed = service.respond(private_message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("已清空个人中的 2 条"));
    assert_eq!(active_count(&service, &user, target), 0);
}

#[tokio::test]
async fn clear_rejects_confirmation_when_target_changed_after_preparation() {
    let service = test_service();
    service
        .respond(private_message("/memory personal 第一条待清空记忆"))
        .await
        .unwrap();
    service
        .respond(private_message("/memory clear"))
        .await
        .unwrap();

    // 模拟准备后由另一路径新增对象；旧确认不能扩大到用户未看见的新对象。
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: None,
            content: "并发新增记忆".to_owned(),
            source_text: "test seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let failed = service.respond(private_message("确认")).await.unwrap();
    assert!(failed.text.unwrap().contains("执行失败"));
    let user = actor("u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        2
    );
}

#[tokio::test]
async fn onebot_account_scopes_do_not_share_group_profile_or_list_numbers() {
    let service = test_service();
    let mut account_a = message_in_scope(
        "/memory profile 在这个群叫我账号A",
        "platform:onebot11:account:bot-a:group:g1",
        "u1",
        "g1",
    );
    account_a.platform = "onebot11".to_owned();
    account_a.account_id = Some("bot-a".to_owned());
    service.respond(account_a.clone()).await.unwrap();

    let mut account_b = message_in_scope(
        "/memory profile 在这个群叫我账号B",
        "platform:onebot11:account:bot-b:group:g1",
        "u1",
        "g1",
    );
    account_b.platform = "onebot11".to_owned();
    account_b.account_id = Some("bot-b".to_owned());
    service.respond(account_b.clone()).await.unwrap();

    account_a.content = "/memory profile list".to_owned();
    let list_a = service.respond(account_a).await.unwrap().text.unwrap();
    assert!(list_a.contains("账号A"));
    assert!(!list_a.contains("账号B"));

    account_b.content = "/memory profile list".to_owned();
    let list_b = service.respond(account_b).await.unwrap().text.unwrap();
    assert!(list_b.contains("账号B"));
    assert!(!list_b.contains("账号A"));
}
