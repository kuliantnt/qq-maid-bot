use super::support::*;
use super::*;

fn list_arguments(
    status: &str,
    date_range_text: Option<&str>,
    time_filter: Option<&str>,
    keyword: Option<&str>,
) -> Value {
    json!({
        "status": status,
        "due_date": null,
        "date_range_text": date_range_text,
        "time_filter": time_filter,
        "keyword": keyword,
    })
}

#[tokio::test]
async fn list_tool_returns_ten_items_with_real_total_and_truncation_flag() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    for index in 1..=11 {
        todo_store
            .create(&owner, tool_test_draft(&format!("第 {index} 条")))
            .unwrap();
    }

    let output = ListTodoTool::new(todo_store, session_store)
        .execute(test_context(), list_arguments("pending", None, None, None))
        .await
        .unwrap()
        .value;

    assert_eq!(output["count"], 10);
    assert_eq!(output["total_count"], 11);
    assert_eq!(output["displayed_count"], 10);
    assert_eq!(output["truncated"], true);
}

#[tokio::test]
async fn list_tool_combines_tomorrow_status_and_keyword_filters() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let tomorrow = (today + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let day_after = (today + chrono::Duration::days(2))
        .format("%Y-%m-%d")
        .to_string();
    for (title, date) in [
        ("项目 A 报告", tomorrow.as_str()),
        ("项目 B 报告", tomorrow.as_str()),
        ("项目 A 后续", day_after.as_str()),
    ] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    due_date: Some(date.to_owned()),
                    time_precision: TodoTimePrecision::Date,
                    ..tool_test_draft(title)
                },
            )
            .unwrap();
    }

    let output = ListTodoTool::new(todo_store, session_store)
        .execute(
            test_context(),
            list_arguments("pending", Some("明天"), None, Some("项目 A")),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["total_count"], 1);
    assert_eq!(output["items"][0]["title"], "项目 A 报告");
}

#[tokio::test]
async fn list_tool_filters_recurring_true_false_and_null() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let tomorrow = (qq_maid_common::time_context::request_time_context().local_date()
        + chrono::Duration::days(1))
    .format("%Y-%m-%d")
    .to_string();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "项目周期复盘".to_owned(),
                due_date: Some(tomorrow.clone()),
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::Daily,
                recurrence_interval_days: 1,
                recurrence_interval: 1,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                ..tool_test_draft("项目周期复盘")
            },
        )
        .unwrap();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "项目一次复盘".to_owned(),
                due_date: Some(tomorrow),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("项目一次复盘")
            },
        )
        .unwrap();
    let tool = ListTodoTool::new(todo_store, session_store);
    let arguments = |recurring: Value| {
        json!({
            "status": "pending",
            "due_date": null,
            "date_range_text": "明天",
            "time_filter": null,
            "keyword": "项目",
            "recurring": recurring,
        })
    };

    let recurring = tool
        .execute(test_context(), arguments(json!(true)))
        .await
        .unwrap()
        .value;
    let one_off = tool
        .execute(test_context(), arguments(json!(false)))
        .await
        .unwrap()
        .value;
    let unrestricted = tool
        .execute(test_context(), arguments(Value::Null))
        .await
        .unwrap()
        .value;
    let legacy_unrestricted = tool
        .execute(
            test_context(),
            json!({
                "status": "pending",
                "due_date": null,
                "date_range_text": "明天",
                "time_filter": null,
                "keyword": "项目",
            }),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(recurring["total_count"], 1);
    assert_eq!(recurring["items"][0]["title"], "项目周期复盘");
    assert_eq!(one_off["total_count"], 1);
    assert_eq!(one_off["items"][0]["title"], "项目一次复盘");
    assert_eq!(unrestricted["total_count"], 2);
    assert_eq!(unrestricted["items"], legacy_unrestricted["items"]);
}

#[tokio::test]
async fn list_tool_rejects_completed_or_all_with_overdue_before_querying() {
    for status in ["completed", "all"] {
        let (todo_store, session_store, _notification_store, _owner) = test_stores();
        let err = ListTodoTool::new(todo_store, session_store)
            .execute(
                test_context(),
                list_arguments(status, None, Some("overdue"), None),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_request");
        assert_eq!(err.message, "逾期筛选只适用于未完成待办。");
    }
}

#[tokio::test]
async fn list_tool_all_uses_board_order_for_task_local_numbers_without_user_snapshot_pollution() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    todo_store
        .set_items_for_test(&owner, &tool_order_items())
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = list_tool
        .execute(test_context(), json!({"status":"all"}))
        .await
        .unwrap()
        .value;

    let titles = output["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec!["明天事项", "后天事项", "无时间事项", "较新归档", "较早归档"]
    );
    assert_eq!(output["items"][0]["visible_number"], 1);

    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(
        session.last_todo_query.is_none(),
        "list_todos 是 Agent 内部查询，不应污染用户可见编号快照"
    );

    let completed = complete_tool
        .execute(test_context(), json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;
    assert_eq!(completed["completed"][0]["title"], "明天事项");
}

#[tokio::test]
async fn list_tool_due_date_filters_items_and_keeps_task_local_numbers() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let no_time = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "无时间".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let today = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "今天事项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-07-03".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "明天事项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-07-04".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    assert_ne!(no_time.id, today.id);

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();
    let output = list_tool
        .execute(
            context.clone(),
            json!({"status":"pending", "due_date":"2026-07-03"}),
        )
        .await
        .unwrap()
        .value;
    assert_eq!(output["due_date"], "2026-07-03");
    assert_eq!(output["count"], 1);
    assert_eq!(output["items"][0]["title"], "今天事项");

    let completed = complete_tool
        .execute(context, json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;
    assert_eq!(completed["completed"][0]["title"], "今天事项");
}

#[tokio::test]
async fn list_tool_date_range_text_is_normalized_by_rust_context() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let ctx = qq_maid_common::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    for (title, date) in [
        ("范围外事项", before_range),
        ("昨天事项", yesterday),
        ("今天事项", today),
    ] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some(date.format("%Y-%m-%d").to_string()),
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::Date,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let output = list_tool
        .execute(
            test_context(),
            json!({"status":"pending", "due_date": null, "date_range_text":"这两天"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["date_range_text"], "这两天");
    assert_eq!(output["date_range_field"], "planned");
    assert_eq!(
        output["due_start"],
        yesterday.format("%Y-%m-%d").to_string()
    );
    assert_eq!(output["due_end"], today.format("%Y-%m-%d").to_string());
    assert_eq!(output["count"], 2);
    assert_eq!(output["items"][0]["title"], "昨天事项");
    assert_eq!(output["items"][1]["title"], "今天事项");
}

#[tokio::test]
async fn list_tool_completed_date_range_uses_completed_at_not_planned_date() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let ctx = qq_maid_common::time_context::request_time_context();
    let today = ctx.local_date();
    let yesterday = today - chrono::Duration::days(1);
    let before_range = today - chrono::Duration::days(2);
    let completed_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "昨天完成但计划较早".to_owned(),
                due_date: Some(before_range.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("昨天完成但计划较早")
            },
        )
        .unwrap();
    let planned_in_range = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "计划昨天但完成较早".to_owned(),
                due_date: Some(yesterday.format("%Y-%m-%d").to_string()),
                time_precision: TodoTimePrecision::Date,
                ..tool_test_draft("计划昨天但完成较早")
            },
        )
        .unwrap();
    todo_store.complete(&owner, &completed_in_range.id).unwrap();
    todo_store.complete(&owner, &planned_in_range.id).unwrap();
    let mut items = todo_store.list_all(&owner).unwrap();
    for item in &mut items {
        if item.id == completed_in_range.id {
            item.completed_at = Some(format!("{}T10:00:00+08:00", yesterday.format("%Y-%m-%d")));
        } else if item.id == planned_in_range.id {
            item.completed_at = Some(format!(
                "{}T10:00:00+08:00",
                before_range.format("%Y-%m-%d")
            ));
        }
    }
    todo_store.set_items_for_test(&owner, &items).unwrap();

    let output = ListTodoTool::new(todo_store.clone(), session_store.clone())
        .execute(
            test_context(),
            json!({"status":"completed", "due_date": null, "date_range_text":"这两天"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["date_range_field"], "completed_at");
    assert_eq!(output["count"], 1);
    assert_eq!(output["items"][0]["title"], "昨天完成但计划较早");
}

#[tokio::test]
async fn list_tool_rejects_due_date_and_date_range_text_together() {
    let (todo_store, session_store, _notification_store, _owner) = test_stores();

    let err = ListTodoTool::new(todo_store.clone(), session_store.clone())
        .execute(
            test_context(),
            json!({"status":"pending", "due_date": "2026-07-01", "date_range_text":"本周"}),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_request");
    assert!(err.message.contains("不能同时传入"));
}

#[tokio::test]
async fn get_tool_uses_task_local_number_without_user_snapshot_pollution() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    todo_store
        .set_items_for_test(&owner, &tool_order_items())
        .unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());
    let context = test_context();

    list_tool
        .execute(context.clone(), json!({"status":"all"}))
        .await
        .unwrap();
    let output = get_tool
        .execute(
            context,
            json!({"number": 1, "numbers": null, "selection_text": null, "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "明天事项");
    assert_eq!(output["item"]["visible_number"], 1);
    let session = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(
        session.last_todo_query.is_none(),
        "get_todo 不应把 Agent 内部查询编号写成用户可见编号快照"
    );
}

#[tokio::test]
async fn get_tool_selection_text_reuses_single_selector() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    for title in ["第一条", "第二条"] {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
            )
            .unwrap();
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());
    let context = test_context();
    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let output = get_tool
        .execute(
            context,
            json!({"number": null, "numbers": null, "selection_text": "第2条", "reference": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "第二条");
    assert_eq!(output["item"]["visible_number"], 2);
}

#[tokio::test]
async fn get_tool_reference_last_uses_last_todo_action_without_writes() {
    let (todo_store, session_store, _notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "刚创建的事项".to_owned(),
                detail: Some("需要查详情".to_owned()),
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
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
    session.remember_last_todo_action(&owner.key, &item, "created");
    session_store.save(&mut session).unwrap();
    let get_tool = GetTodoTool::new(todo_store.clone(), session_store.clone());

    let output = get_tool
        .execute(
            test_context(),
            json!({"number": null, "numbers": null, "selection_text": null, "reference": "last"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["item"]["title"], "刚创建的事项");
    assert_eq!(output["item"]["reference"], "last");
    let saved = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(saved.pending_operation.is_none());
    assert!(saved.last_todo_query.is_none());
    assert_eq!(
        saved.last_todo_action.expect("missing last action").item_id,
        item.id
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}
