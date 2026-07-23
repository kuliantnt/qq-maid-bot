use super::*;
use crate::{
    runtime::{
        notification::{NotificationBeforePushPause, NotificationWorker, NotificationWorkerConfig},
        push::{PushError, PushIntent, PushResult, PushSink},
        respond::{RespondRequest, common::empty_respond_request},
        tools::todo::sync_reminder_task,
    },
    storage::notification::NotificationStatus,
};
use async_trait::async_trait;
use qq_maid_common::identity_context::ConversationKind;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct CountingPushSink {
    calls: AtomicUsize,
}

#[async_trait]
impl PushSink for CountingPushSink {
    async fn push(&self, _intent: PushIntent) -> Result<PushResult, PushError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PushResult { message_id: None })
    }
}

fn group_request(text: &str, group_id: &str, user_id: &str, role: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: format!("group:{group_id}"),
        conversation_kind: ConversationKind::Group,
        conversation_id: Some(group_id.to_owned()),
        user_id: Some(user_id.to_owned()),
        group_member_role: Some(role.to_owned()),
        group_id: Some(group_id.to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

fn stable_group_request(
    text: &str,
    account_id: &str,
    group_id: &str,
    user_id: &str,
    role: &str,
) -> RespondRequest {
    RespondRequest {
        scope_key: format!("platform:qq_official:account:{account_id}:group:{group_id}"),
        account_id: Some(account_id.to_owned()),
        ..group_request(text, group_id, user_id, role)
    }
}

#[tokio::test]
async fn group_owner_and_admin_can_list_all_current_group_creators() {
    let service = test_service();
    let owner_a = TodoStore::owner(Some("u1"), "group:g1");
    let owner_b = TodoStore::owner(Some("u2"), "group:g1");
    let other_group = TodoStore::owner(Some("u2"), "group:g2");
    let private = TodoStore::owner(Some("u2"), "private:u2");
    service
        .task_store
        .create(&owner_a, draft("群主创建的 Todo"))
        .unwrap();
    service
        .task_store
        .create(&owner_b, draft("成员创建的提醒"))
        .unwrap();
    service
        .task_store
        .create(&other_group, draft("其他群 Todo"))
        .unwrap();
    service
        .task_store
        .create(&private, draft("私聊 Todo"))
        .unwrap();
    service
        .display_name_store()
        .set("group:g1", "u2", "小乙")
        .unwrap();

    for (user_id, role) in [("u1", "owner"), ("u3", "admin")] {
        let response = service
            .respond(group_request("/todo group", "g1", user_id, role))
            .await
            .unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_group_list"));
        let text = response.text.unwrap();
        assert!(text.contains("群主创建的 Todo"));
        assert!(text.contains("成员创建的提醒"));
        assert!(text.contains("创建者：小乙"));
        assert!(!text.contains("其他群 Todo"));
        assert!(!text.contains("私聊 Todo"));
    }
}

#[tokio::test]
async fn ordinary_member_and_private_chat_cannot_use_group_todo_management() {
    let service = test_service();
    let denied = service
        .respond(group_request("/todo group", "g1", "u2", "member"))
        .await
        .unwrap();
    assert_eq!(denied.command.as_deref(), Some("group_admin_required"));
    assert!(denied.text.as_deref().unwrap().contains("群主或管理员"));

    let private = service
        .respond(private_message("/todo group"))
        .await
        .unwrap();
    assert_eq!(
        private.command.as_deref(),
        Some("todo_group_scope_required")
    );
}

#[tokio::test]
async fn group_list_uses_full_platform_account_and_group_scope() {
    let service = test_service();
    let current_scope = "platform:qq_official:account:bot-1:group:g1";
    let other_account_scope = "platform:qq_official:account:bot-2:group:g1";
    service
        .task_store
        .create(
            &TodoStore::owner(Some("u2"), current_scope),
            draft("当前账号群 Todo"),
        )
        .unwrap();
    service
        .task_store
        .create(
            &TodoStore::owner(Some("u2"), other_account_scope),
            draft("其他账号同群 ID Todo"),
        )
        .unwrap();

    let response = service
        .respond(stable_group_request(
            "/todo group",
            "bot-1",
            "g1",
            "admin-1",
            "admin",
        ))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("当前账号群 Todo"));
    assert!(!text.contains("其他账号同群 ID Todo"));
}

#[tokio::test]
async fn group_admin_delete_cancels_claimed_reminder_before_push() {
    let service = test_service();
    let creator = TodoStore::owner(Some("u2"), "group:g1");
    let item = service
        .task_store
        .create(
            &creator,
            TodoItemDraft {
                reminder_at: Some("2099-07-24 09:00:00".to_owned()),
                ..draft("成员的长期提醒")
            },
        )
        .unwrap();
    sync_reminder_task(&service.notification_store, &creator, &item).unwrap();
    let task = service
        .notification_store
        .list_all_for_test()
        .unwrap()
        .pop()
        .unwrap();
    service
        .notification_store
        .database()
        .connection()
        .unwrap()
        .execute(
            "UPDATE notification_outbox
             SET scheduled_at = '2020-01-01T09:00:00+08:00'
             WHERE id = ?1",
            [task.id],
        )
        .unwrap();

    let sink = Arc::new(CountingPushSink::default());
    let pause = NotificationBeforePushPause::default();
    let worker = NotificationWorker::new(
        service.notification_store.clone(),
        sink.clone(),
        NotificationWorkerConfig::default(),
    )
    .with_before_push_pause_for_test(pause.clone());
    let worker_task = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        pause.wait_until_reached(),
    )
    .await
    .expect("worker should pause after claiming and before push");

    service
        .respond(group_request("/todo group", "g1", "u1", "owner"))
        .await
        .unwrap();
    let deleted = service
        .respond(group_request("/todo group delete 1", "g1", "u1", "owner"))
        .await
        .unwrap();
    pause.resume();
    let stats = worker_task.await.unwrap().unwrap();

    assert_eq!(deleted.command.as_deref(), Some("todo_group_delete"));
    assert!(deleted.text.as_deref().unwrap().contains("对应提醒已取消"));
    assert_eq!(sink.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stats.cancelled_count, 1);
    assert_eq!(stats.sent_count, 0);
    assert_eq!(stats.failed_count, 0);
    assert_eq!(stats.invalid_payload_count, 0);
    assert_eq!(stats.lease_lost_count, 0);
    assert!(
        service
            .task_store
            .get_by_id(&creator, &item.id)
            .unwrap()
            .is_none()
    );
    let tasks = service.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);
    assert!(tasks[0].locked_by.is_none());
    assert!(tasks[0].locked_at.is_none());
    assert!(tasks[0].sent_at.is_none());
    assert!(tasks[0].next_attempt_at.is_none());
    assert!(tasks[0].last_error.is_none());
}

#[tokio::test]
async fn group_admin_delete_without_reminder_uses_conditional_message() {
    let service = test_service();
    let creator = TodoStore::owner(Some("u2"), "group:g1");
    let item = service
        .task_store
        .create(&creator, draft("没有提醒的群 Todo"))
        .unwrap();
    service
        .respond(group_request("/todo group", "g1", "u1", "owner"))
        .await
        .unwrap();

    let deleted = service
        .respond(group_request("/todo group delete 1", "g1", "u1", "owner"))
        .await
        .unwrap();

    assert!(
        deleted
            .text
            .as_deref()
            .unwrap()
            .contains("如有对应提醒，也已取消")
    );
    assert!(
        service
            .task_store
            .get_by_id(&creator, &item.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn group_delete_snapshot_cannot_cross_actor_group_or_role_change() {
    let service = test_service();
    let group_a_owner = TodoStore::owner(Some("creator-a"), "group:g1");
    let group_b_owner = TodoStore::owner(Some("creator-b"), "group:g2");
    let item_a = service
        .task_store
        .create(&group_a_owner, draft("A 群 Todo"))
        .unwrap();
    let item_b = service
        .task_store
        .create(&group_b_owner, draft("B 群 Todo"))
        .unwrap();
    service
        .respond(group_request("/todo group", "g1", "admin-a", "admin"))
        .await
        .unwrap();

    let other_actor = service
        .respond(group_request(
            "/todo group delete 1",
            "g1",
            "admin-b",
            "admin",
        ))
        .await
        .unwrap();
    assert_eq!(
        other_actor.command.as_deref(),
        Some("todo_group_snapshot_required")
    );
    let other_group = service
        .respond(group_request(
            "/todo group delete 1",
            "g2",
            "admin-a",
            "admin",
        ))
        .await
        .unwrap();
    assert_eq!(
        other_group.command.as_deref(),
        Some("todo_group_snapshot_required")
    );
    let downgraded = service
        .respond(group_request(
            "/todo group delete 1",
            "g1",
            "admin-a",
            "member",
        ))
        .await
        .unwrap();
    assert_eq!(downgraded.command.as_deref(), Some("group_admin_required"));
    assert!(
        service
            .task_store
            .get_by_id(&group_a_owner, &item_a.id)
            .unwrap()
            .is_some()
    );
    assert!(
        service
            .task_store
            .get_by_id(&group_b_owner, &item_b.id)
            .unwrap()
            .is_some()
    );
}
