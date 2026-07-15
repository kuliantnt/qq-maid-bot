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
async fn private_personal_draft_writes_only_after_one_confirmation() {
    let service = test_service();
    let request = private_message("/memory 我不喜欢太长的回复");
    let response = service.respond(request).await.unwrap();
    assert!(response.text.unwrap().contains("目标范围：个人记忆"));

    let user = actor("u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );
    let confirmed = service.respond(private_message("可以")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("已保存个人记忆"));
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        1
    );

    // Pending 已消费；第二个确认词只能作为普通聊天，不能再次写入。
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
    assert!(profile.text.unwrap().contains("目标范围：当前群画像"));
    service.respond(message("确认")).await.unwrap();

    let user = actor("u1", Some("g1"), true);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        1
    );
    assert_eq!(active_count(&service, &user, MemoryTarget::group("g1")), 0);

    let group = service
        .respond(message("/memory 这个群每周五晚上进行项目周会"))
        .await
        .unwrap();
    assert!(group.text.unwrap().contains("目标范围：当前群组记忆"));
    service.respond(message("确认")).await.unwrap();
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

    let draft = service.respond(message("画像")).await.unwrap();
    assert!(draft.text.unwrap().contains("目标范围：当前群画像"));
    assert_eq!(
        active_count(&service, &u1, MemoryTarget::group_profile("g1", "u1")),
        0
    );
    service.respond(message("确认")).await.unwrap();
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
async fn cancelled_and_expired_drafts_cannot_write() {
    let service = test_service();
    service
        .respond(private_message("/memory 我不喜欢太长的回复"))
        .await
        .unwrap();
    let cancelled = service.respond(private_message("不记")).await.unwrap();
    assert!(cancelled.text.unwrap().contains("已取消"));

    let user = actor("u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );

    let request = private_message("/memory 我不喜欢太长的回复");
    let meta = respond_interaction_meta(&request);
    service.respond(request).await.unwrap();
    let mut session = service.session_store.get_active(&meta).unwrap().unwrap();
    let pending = session.pending_operation.as_mut().unwrap();
    let payload = pending.payload().clone();
    let display_snapshot = pending.display_snapshot().clone();
    pending
        .revise(payload, display_snapshot, "2000-01-01T00:00:00+08:00")
        .unwrap();
    service.session_store.save(&mut session).unwrap();

    let expired = service.respond(private_message("确认")).await.unwrap();
    assert!(expired.text.unwrap().contains("已过期"));
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );
}

#[tokio::test]
async fn profile_opt_out_blocks_writes_until_explicit_reauthorization() {
    let service = test_service();
    service
        .respond(message("/memory 在这个群叫我棒冰"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let stop = service
        .respond(message("/memory profile stop"))
        .await
        .unwrap();
    assert!(stop.text.unwrap().contains("停止当前群继续保存"));
    service.respond(message("确认")).await.unwrap();

    let user = actor("u1", Some("g1"), true);
    let target = MemoryTarget::group_profile("g1", "u1");
    assert_eq!(active_count(&service, &user, target.clone()), 0);

    service
        .respond(message("/memory profile 在这个群叫我雪糕"))
        .await
        .unwrap();
    let blocked = service.respond(message("确认")).await.unwrap();
    assert!(blocked.text.unwrap().contains("执行失败"));
    assert_eq!(active_count(&service, &user, target.clone()), 0);
    service.respond(message("取消")).await.unwrap();

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
    service.respond(message("确认")).await.unwrap();
    assert_eq!(active_count(&service, &user, target), 1);
}

#[tokio::test]
async fn list_and_detail_show_management_fields_without_internal_id() {
    let service = test_service();
    service
        .respond(private_message("/memory 我不喜欢太长的回复"))
        .await
        .unwrap();
    service.respond(private_message("确认")).await.unwrap();

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
    for expected in [
        "范围：个人记忆",
        "类型：preference",
        "可见性：private",
        "来源摘要：用户明确确认",
        "创建：",
        "确认：",
        "状态：active",
        "固定：否",
    ] {
        assert!(list.contains(expected), "列表缺少字段：{expected}");
    }
    assert!(!list.contains(&internal_id));
    assert!(!list.contains(&short_id));

    let detail = service
        .respond(private_message("/memory show 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    for expected in [
        "命名空间：个人记忆",
        "类型：preference",
        "可见性：private",
        "来源摘要：用户明确确认",
        "创建时间：",
        "确认时间：",
        "状态：active",
        "固定：否",
    ] {
        assert!(detail.contains(expected), "详情缺少字段：{expected}");
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
        service.respond(private_message("确认")).await.unwrap();
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
    service.respond(private_message("确认")).await.unwrap();
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
    account_a.content = "确认".to_owned();
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
    account_b.content = "确认".to_owned();
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
