use super::support::*;
use super::*;

#[tokio::test]
async fn create_tool_with_reminder_writes_notification_outbox() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let output = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "明天提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    // 到期与提醒解耦：纯提醒创建不再回填 due_at。
    assert_eq!(output["created"]["due_at"], serde_json::Value::Null);
    assert_eq!(
        output["created"]["reminder_at"].as_str(),
        Some("2099-01-01 09:30")
    );
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_type, "todo");
    assert_eq!(tasks[0].kind, "todo_reminder");
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Pending
    );
    assert_eq!(tasks[0].scheduled_at, "2099-01-01T09:30:00+08:00");
    assert!(
        tasks[0].payload["text"]
            .as_str()
            .unwrap()
            .contains("待办提醒")
    );
    assert!(
        tasks[0].payload["fallback_text"]
            .as_str()
            .unwrap()
            .starts_with("⏰ 待办提醒")
    );
    assert!(
        tasks[0].payload["text"]
            .as_str()
            .unwrap()
            .contains("检查日志")
    );
}

#[tokio::test]
async fn create_tool_due_date_without_reminder_does_not_default_to_nine_oclock_outbox() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );

    let output = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "周五前写周报，不提醒我",
                "title": "写周报",
                "detail": null,
                "due_date": "2099-01-02",
                "due_at": null,
                "reminder_at": null,
                "time_precision": "date"
            }),
        )
        .await
        .unwrap()
        .value;
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(todo.due_date.as_deref(), Some("2099-01-02"));
    assert_eq!(todo.reminder_at, None);
    assert!(
        tasks.is_empty(),
        "截止日期不能自动派生成 09:00 reminder outbox"
    );
}

#[tokio::test]
async fn edit_tool_reschedules_pending_reminder_cancels_old_outbox_task() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-pending-reminder".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-pending-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "改到后天上午九点半提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-02 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();
    let old_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-01T09:30:00+08:00")
        .unwrap();
    let new_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-02T09:30:00+08:00")
        .unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        old_task.status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
    assert_eq!(
        new_task.status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn edit_tool_reschedules_sent_reminder_with_new_outbox_task() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let mut create_context = test_context();
    create_context.tool_call_id = Some("create-reminder".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "提醒我检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();
    let first_task = notification_store.list_all_for_test().unwrap()[0].clone();
    notification_store.mark_sent(first_task.id).unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "改到后天上午九点半提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-02 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let tasks = notification_store.list_all_for_test().unwrap();
    let new_task = tasks
        .iter()
        .find(|task| task.scheduled_at == "2099-01-02T09:30:00+08:00")
        .unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Sent
    );
    assert_eq!(
        new_task.status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn edit_tool_allows_unrelated_edit_when_existing_reminder_is_past() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: Some("2020-01-01 09:30:00".to_owned()),
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    list_tool
        .execute(test_context(), json!({"status": "pending"}))
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-title-with-past-reminder".to_owned());
    let output = edit_tool
        .execute(
            edit_context,
            json!({
                "number": 1,
                "reference": null,
                "raw_text": "标题改成检查网关日志",
                "title": "检查网关日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &item.id).unwrap().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(updated.title, "检查网关日志");
    assert_eq!(notification_store.list_all_for_test().unwrap().len(), 0);
}

#[tokio::test]
async fn complete_tool_cancels_pending_reminder_task() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "检查日志",
                "title": "检查日志",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap();
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let mut session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "待办列表", vec![todo.id.clone()]);
    session_store.save(&mut session).unwrap();
    let complete_tool =
        CompleteTodoTool::new(todo_store, session_store, notification_store.clone());

    complete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "selection_text": null, "reference": null}),
        )
        .await
        .unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
}
