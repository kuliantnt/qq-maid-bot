use super::support::*;
use super::*;

#[tokio::test]
async fn create_tool_accepts_minute_recurrence() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store, session_store, notification_store);

    let output = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每隔 5 分钟提醒我检查状态",
                "title": "检查状态",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "every_n_minutes",
                "recurrence_interval": 5,
                "recurrence_unit": "minute",
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(
        output["created"]["recurrence_kind"].as_str(),
        Some("every_n_minutes")
    );
    assert_eq!(output["created"]["recurrence_interval"].as_u64(), Some(5));
    assert_eq!(
        output["created"]["recurrence_unit"].as_str(),
        Some("minute")
    );
    assert_eq!(
        output["created"]["recurrence_interval_days"].as_u64(),
        Some(0)
    );
    assert_eq!(output["created"]["due_at"], serde_json::Value::Null);
    assert_eq!(
        output["created"]["reminder_at"].as_str(),
        Some("2099-01-01 09:30")
    );
}

#[tokio::test]
async fn create_tool_infers_first_reminder_for_periodic_reminder() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );
    let before = chrono::Utc::now().with_timezone(&qq_maid_common::time_context::shanghai_offset());

    let output = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每五分钟提醒我一下，要起来走走",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": null,
                "recurrence_interval": null,
                "recurrence_unit": null,
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap()
        .value;
    let after = chrono::Utc::now().with_timezone(&qq_maid_common::time_context::shanghai_offset());
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let reminder = qq_maid_common::time_context::parse_local_datetime_for_comparison(
        todo.reminder_at.as_deref().unwrap(),
    )
    .unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(todo.title, "起来走走");
    assert_eq!(todo.due_at, None);
    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMinutes
    );
    assert_eq!(todo.recurrence_interval, 5);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Minute
    );
    assert!(reminder >= before + chrono::Duration::minutes(5) - chrono::Duration::seconds(1));
    assert!(reminder <= after + chrono::Duration::minutes(5) + chrono::Duration::seconds(1));
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source_id, todo.id);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn create_tool_infers_first_reminder_for_chinese_hour_interval() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);
    let before = chrono::Utc::now().with_timezone(&qq_maid_common::time_context::shanghai_offset());

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每两小时提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": null,
                "recurrence_interval": null,
                "recurrence_unit": null,
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();
    let after = chrono::Utc::now().with_timezone(&qq_maid_common::time_context::shanghai_offset());
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    let reminder = qq_maid_common::time_context::parse_local_datetime_for_comparison(
        todo.reminder_at.as_deref().unwrap(),
    )
    .unwrap();

    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::EveryNHours
    );
    assert_eq!(todo.recurrence_interval, 2);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Hour
    );
    assert!(reminder >= before + chrono::Duration::hours(2) - chrono::Duration::seconds(1));
    assert!(reminder <= after + chrono::Duration::hours(2) + chrono::Duration::seconds(1));
}

#[tokio::test]
async fn create_tool_infers_first_reminder_for_arabic_minute_interval() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每 5 分钟提醒我起来走走",
                "title": "起来走走",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": null,
                "recurrence_interval": null,
                "recurrence_unit": null,
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();
    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();

    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMinutes
    );
    assert_eq!(todo.recurrence_interval, 5);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Minute
    );
    assert!(todo.reminder_at.is_some());
}

#[tokio::test]
async fn create_tool_recurring_error_message_hides_internal_nulls() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    let err = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每天写日报",
                "title": "写日报",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": null,
                "recurrence_interval": null,
                "recurrence_unit": null,
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code.as_str(), "bad_request");
    assert!(!err.message.contains("null"), "{}", err.message);
    assert!(!err.message.contains("None"), "{}", err.message);
    assert!(!err.message.contains("Option"), "{}", err.message);
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn create_tool_rejects_invalid_minute_recurrence_arguments() {
    for (recurrence_interval, recurrence_unit, expected) in [
        (json!(0), json!("minute"), "positive integer"),
        (json!(-1), json!("minute"), "positive integer"),
        (serde_json::Value::Null, json!("minute"), "正整数"),
        (json!(5), json!("second"), "minute/hour/day/week/month/year"),
    ] {
        let (todo_store, session_store, notification_store, owner) = test_stores();
        let create_tool =
            CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

        let err = create_tool
            .execute(
                test_context(),
                json!({
                    "items": null,
                    "content": "每隔 5 分钟提醒我检查状态",
                    "title": "检查状态",
                    "detail": null,
                    "due_date": null,
                    "due_at": null,
                    "reminder_at": "2099-01-01 09:30",
                    "time_precision": null,
                    "recurrence_kind": "every_n_minutes",
                    "recurrence_interval": recurrence_interval,
                    "recurrence_unit": recurrence_unit,
                    "recurrence_interval_days": null
                }),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err.code.as_str(), "bad_tool_arguments" | "bad_request"),
            "{}",
            err.code
        );
        assert!(err.message.contains(expected), "{}", err.message);
        assert!(todo_store.list_pending(&owner).unwrap().is_empty());
    }
}

#[tokio::test]
async fn create_tool_rejects_ambiguous_recurrence_phrase() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    let err = create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每隔几天提醒我复盘",
                "title": "复盘",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code.as_str(), "bad_request");
    assert!(err.message.contains("每隔 3 天"));
    assert!(todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn create_tool_explicit_none_skips_recurrence_inference_from_content() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(todo_store.clone(), session_store, notification_store);

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "明天提醒我：不要每天喝奶茶",
                "title": "不要每天喝奶茶",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "none",
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();

    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::None
    );
    assert_eq!(todo.recurrence_interval_days, 0);
    assert_eq!(todo.recurrence_interval, 0);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Day
    );
}

#[tokio::test]
async fn edit_tool_explicit_none_skips_recurrence_inference_from_raw_text() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
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
    create_context.tool_call_id = Some("create-recurring".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("clear-recurrence".to_owned());
    edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "不要每天提醒了，保留这次提醒",
                "title": null,
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null,
                "recurrence_kind": "none",
                "recurrence_interval_days": null
            }),
        )
        .await
        .unwrap();

    let todo = todo_store.list_pending(&owner).unwrap()[0].clone();
    assert_eq!(
        todo.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::None
    );
    assert_eq!(todo.recurrence_interval_days, 0);
    assert_eq!(todo.recurrence_interval, 0);
    assert_eq!(
        todo.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Day
    );
    assert_eq!(todo.reminder_at.as_deref(), Some("2099-01-01 09:30"));
}

#[tokio::test]
async fn edit_tool_sets_weekly_monthly_yearly_unit_when_only_kind_is_provided() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
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

    for (index, kind, expected_unit) in [
        (
            "weekly",
            "weekly",
            crate::runtime::tools::todo::TodoRecurrenceUnit::Week,
        ),
        (
            "monthly",
            "monthly",
            crate::runtime::tools::todo::TodoRecurrenceUnit::Month,
        ),
        (
            "yearly",
            "yearly",
            crate::runtime::tools::todo::TodoRecurrenceUnit::Year,
        ),
    ] {
        let mut create_context = test_context();
        create_context.tool_call_id = Some(format!("create-{index}"));
        create_tool
            .execute(
                create_context,
                json!({
                    "items": null,
                    "content": format!("提醒我做 {index} 检查"),
                    "title": format!("{index} 检查"),
                    "detail": null,
                    "due_date": null,
                    "due_at": null,
                    "reminder_at": "2099-01-01 09:30",
                    "time_precision": null,
                    "recurrence_kind": "none",
                    "recurrence_interval": null,
                    "recurrence_unit": null,
                    "recurrence_interval_days": null
                }),
            )
            .await
            .unwrap();

        let mut edit_context = test_context();
        edit_context.tool_call_id = Some(format!("edit-{index}"));
        edit_tool
            .execute(
                edit_context,
                json!({
                    "number": null,
                    "reference": "last",
                    "raw_text": format!("改成 {kind} 重复"),
                    "title": null,
                    "detail": null,
                    "due_date": null,
                    "due_at": null,
                    "reminder_at": null,
                    "time_precision": null,
                    "recurrence_kind": kind,
                    "recurrence_interval": null,
                    "recurrence_unit": null,
                    "recurrence_interval_days": null
                }),
            )
            .await
            .unwrap();

        let updated = todo_store
            .list_pending(&owner)
            .unwrap()
            .into_iter()
            .find(|item| item.title == format!("{index} 检查"))
            .unwrap();
        assert_eq!(updated.recurrence_interval, 1, "{kind}");
        assert_eq!(updated.recurrence_unit, expected_unit, "{kind}");
        assert_eq!(updated.recurrence_interval_days, 0, "{kind}");
    }
}

#[tokio::test]
async fn complete_tool_advances_recurring_todo_and_reschedules_reminder() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
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

    let output = complete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "selection_text": null, "reference": null}),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["completed"].as_array().map(Vec::len), Some(0));
    assert_eq!(output["advanced"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        output["advanced"][0]["reminder_at"].as_str(),
        Some("2099-01-02 09:30")
    );
    assert_eq!(
        output["advanced"][0]["next_reminder_at"].as_str(),
        Some("2099-01-03 09:30")
    );
    assert_eq!(updated.status, TodoStatus::Pending);
    assert_eq!(updated.reminder_at.as_deref(), Some("2099-01-02 09:30"));
    // 到期与提醒解耦：纯提醒重复任务推进时不产生 due_at。
    assert_eq!(updated.due_at, None);
    assert_eq!(
        updated.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::Daily
    );
    assert_eq!(updated.recurrence_interval_days, 1);
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
    assert_eq!(
        tasks[1].status,
        crate::storage::notification::NotificationStatus::Pending
    );
    assert_eq!(tasks[1].scheduled_at, "2099-01-02T09:30:00+08:00");
}

#[tokio::test]
async fn manage_recurring_reminder_skip_next_advances_without_completing() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let manage_tool = ManageRecurringReminderTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
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

    let output = manage_tool
        .execute(
            test_context(),
            json!({
                "numbers": [1],
                "selection_text": null,
                "reference": null,
                "action": "skip_next"
            }),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["advanced"].as_array().map(Vec::len), Some(1));
    assert_eq!(updated.status, TodoStatus::Pending);
    assert_eq!(updated.reminder_at.as_deref(), Some("2099-01-02 09:30"));
    assert_eq!(
        updated.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::Daily
    );
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
    assert_eq!(
        tasks[1].status,
        crate::storage::notification::NotificationStatus::Pending
    );
}

#[tokio::test]
async fn manage_recurring_reminder_disable_recurrence_keeps_pending_todo() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let create_tool = CreateTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let manage_tool = ManageRecurringReminderTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    create_tool
        .execute(
            test_context(),
            json!({
                "items": null,
                "content": "每天提醒我喝水",
                "title": "喝水",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "reminder_at": "2099-01-01 09:30",
                "time_precision": null,
                "recurrence_kind": "daily",
                "recurrence_interval_days": 1
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

    let output = manage_tool
        .execute(
            test_context(),
            json!({
                "numbers": [1],
                "selection_text": null,
                "reference": null,
                "action": "disable_recurrence"
            }),
        )
        .await
        .unwrap()
        .value;
    let updated = todo_store.get_by_id(&owner, &todo.id).unwrap().unwrap();
    let tasks = notification_store.list_all_for_test().unwrap();

    assert_eq!(output["ok"], true);
    assert_eq!(output["disabled"].as_array().map(Vec::len), Some(1));
    assert_eq!(updated.status, TodoStatus::Pending);
    assert_eq!(
        updated.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::None
    );
    assert_eq!(updated.recurrence_interval, 0);
    assert_eq!(updated.recurrence_interval_days, 0);
    assert_eq!(updated.reminder_at, None);
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].status,
        crate::storage::notification::NotificationStatus::Cancelled
    );
}
