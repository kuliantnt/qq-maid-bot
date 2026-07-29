//! Todo reminder 与 Notification Outbox 管理语义测试。

use super::*;

#[tokio::test]
async fn create_uses_verified_private_or_group_target_and_rejects_unsupported_reminder() {
    let api = TestApi::new();
    let (status, past) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": api.target_ref,
                "title": "过去时间不能创建",
                "reminder_at": "2000-01-01T09:00:00+08:00"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{past}");
    let (_, absent) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"keyword": "过去时间不能创建"}),
        )
        .await;
    assert_eq!(absent["data"]["total"], 0);

    let (private_owner, private_anchor) = api.seed_todo(
        "qq_official",
        "app-2",
        "private",
        "private-2",
        "private-2",
        "私聊锚点",
    );
    let private_ref = api.target_ref_for_item(&private_anchor.id);
    let (status, private_created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({"target_ref": private_ref, "title": "API 私聊待办"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{private_created}");
    assert_eq!(private_created["data"]["target"]["scope_type"], "private");
    assert!(
        api.todo_store
            .list_all(&private_owner)
            .unwrap()
            .iter()
            .any(|item| item.title == "API 私聊待办")
    );

    let (group_owner, group_anchor) = api.seed_todo(
        "onebot",
        "bot-2",
        "group",
        "group-2",
        "member-2",
        "群聊锚点",
    );
    let group_ref = api.target_ref_for_item(&group_anchor.id);
    let (status, group_created) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": group_ref,
                "title": "API 群聊提醒",
                "reminder_at": "2099-08-01T09:00:00+08:00"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{group_created}");
    assert_eq!(group_created["data"]["target"]["platform"], "onebot");
    assert!(
        api.todo_store
            .list_all(&group_owner)
            .unwrap()
            .iter()
            .any(|item| item.title == "API 群聊提醒")
    );
    let tasks = api.notification_store.list_all_for_test().unwrap();
    let group_task = tasks
        .iter()
        .find(|task| task.source_id == group_created["data"]["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(group_task.target.platform, "onebot11");
    assert_eq!(group_task.target.target_id, "group-2");

    let (_, wechat_anchor) = api.seed_todo(
        "wechat_service",
        "wx-2",
        "private",
        "wx-user-2",
        "wx-user-2",
        "微信锚点",
    );
    let wechat_ref = api.target_ref_for_item(&wechat_anchor.id);
    let (status, rejected) = api
        .post(
            "/api/v1/console/todo/create",
            json!({
                "target_ref": wechat_ref,
                "title": "不可投递提醒",
                "reminder_at": "2099-08-01T10:00:00+08:00"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(rejected["error"]["code"], "validation_error");
    let (_, absent) = api
        .post(
            "/api/v1/console/todo/list",
            json!({"keyword": "不可投递提醒"}),
        )
        .await;
    assert_eq!(absent["data"]["total"], 0);
}

#[tokio::test]
async fn reminder_update_replaces_outbox_and_delete_cancels_it() {
    let api = TestApi::new();
    let (_, item) = api.seed_todo(
        "qq_official",
        "app-3",
        "private",
        "user-3",
        "user-3",
        "提醒待办",
    );
    let (status, first) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T09:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T10:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);
    assert_eq!(tasks[1].status, NotificationStatus::Pending);
    assert_eq!(tasks[1].scheduled_at, "2099-08-01T10:00:00+08:00");

    let (status, _) = api
        .post("/api/v1/console/todo/delete", json!({"id": item.id}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks[1].status, NotificationStatus::Cancelled);
}

#[tokio::test]
async fn inherited_past_reminder_allows_unrelated_update_but_explicit_past_value_is_rejected() {
    let api = TestApi::new();
    let (owner, item) = api.seed_todo(
        "qq_official",
        "app-past",
        "private",
        "past-user",
        "past-user",
        "已发送的一次性提醒",
    );
    let (status, scheduled) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T09:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{scheduled}");
    let task = api.notification_store.list_all_for_test().unwrap()[0].clone();
    api.notification_store
        .claim_for_test(task.id, "management-test-worker")
        .unwrap();
    api.notification_store
        .mark_sent(task.id, "management-test-worker", task.delivered_parts)
        .unwrap();
    api.replace_reminder_at(&item.id, "2000-01-01T09:00:00+08:00");

    let (status, renamed) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "title": "仅修改标题成功"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["data"]["title"], "仅修改标题成功");
    assert_eq!(renamed["data"]["reminder_at"], "2000-01-01T09:00:00+08:00");
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, NotificationStatus::Sent);

    let (status, rejected) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2001-01-01T09:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    assert_eq!(rejected["error"]["code"], "validation_error");
    let unchanged = api.todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();
    assert_eq!(unchanged.title, "仅修改标题成功");
    assert_eq!(
        unchanged.reminder_at.as_deref(),
        Some("2000-01-01T09:00:00+08:00")
    );

    let (status, rejected_completion) = api
        .post(
            "/api/v1/console/todo/update",
            json!({
                "id": item.id,
                "reminder_at": "2002-01-01T09:00:00+08:00",
                "status": "completed"
            }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{rejected_completion}"
    );
    assert_eq!(
        api.todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        crate::runtime::tools::todo::TodoStatus::Pending
    );

    let (status, cleared) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": null}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["data"]["reminder_at"], Value::Null);
    assert_eq!(api.notification_store.list_all_for_test().unwrap().len(), 1);
}

#[tokio::test]
async fn completing_and_restoring_todo_with_past_reminder_is_atomic_and_does_not_reschedule() {
    let api = TestApi::new();
    let (owner, item) = api.seed_todo(
        "qq_official",
        "app-past-state",
        "private",
        "past-state-user",
        "past-state-user",
        "过期提醒状态转换",
    );
    let (status, scheduled) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "reminder_at": "2099-08-01T09:00:00+08:00"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{scheduled}");
    api.replace_reminder_at(&item.id, "2000-01-01T09:00:00+08:00");

    let (status, completed) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "status": "completed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["data"]["status"], "completed");
    assert_eq!(
        completed["data"]["reminder_at"],
        "2000-01-01T09:00:00+08:00"
    );
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);

    let (status, restored) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "status": "pending"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["data"]["status"], "pending");
    assert_eq!(restored["data"]["reminder_at"], "2000-01-01T09:00:00+08:00");
    let stored = api.todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        crate::runtime::tools::todo::TodoStatus::Pending
    );
    let tasks = api.notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, NotificationStatus::Cancelled);
}

#[tokio::test]
async fn recurring_completion_advances_atomically_and_reschedules_reminder() {
    let api = TestApi::new();
    let scope = conversation_scope_key(
        "qq_official",
        Some("app-recurring"),
        "private",
        "recurring-user",
    );
    let owner = TodoStore::owner(Some("recurring-user"), &scope);
    let mut draft = todo_draft("周期提醒");
    draft.due_date = Some("2099-08-01".to_owned());
    draft.reminder_at = Some("2099-08-01T09:00:00+08:00".to_owned());
    draft.recurrence_kind = TodoRecurrenceKind::Daily;
    draft.recurrence_interval_days = 1;
    let item = api.todo_store.create(&owner, draft).unwrap();

    let (status, completed) = api
        .post(
            "/api/v1/console/todo/update",
            json!({"id": item.id, "title": "周期提醒已更新", "status": "completed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["data"]["status"], "pending");
    assert_eq!(completed["data"]["title"], "周期提醒已更新");
    assert_ne!(completed["data"]["reminder_at"], item.reminder_at.unwrap());
    let stored = api.todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        crate::runtime::tools::todo::TodoStatus::Pending
    );
    assert_eq!(stored.title, "周期提醒已更新");
    assert_eq!(api.notification_store.list_all_for_test().unwrap().len(), 1);
}
