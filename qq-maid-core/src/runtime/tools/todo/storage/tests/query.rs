use super::*;

#[test]
fn list_by_due_date_matches_date_and_datetime_but_excludes_no_time() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let target_date = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();

    let no_time = store
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
    let date_only = store
        .create(
            &owner,
            TodoItemDraft {
                title: "日期型".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-10".to_owned()),
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
    let datetime = store
        .create(
            &owner,
            TodoItemDraft {
                title: "带时间".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-10 09:30:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let local_midnight = store
        .create(
            &owner,
            TodoItemDraft {
                title: "本地零点".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-09T16:00:00+00:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    store
        .create(
            &owner,
            TodoItemDraft {
                title: "次日零点".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-10T16:00:00+00:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    let items = store
        .list_by_due_date(&owner, TodoStatus::Pending, target_date)
        .unwrap();
    let ids = items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            local_midnight.id.as_str(),
            date_only.id.as_str(),
            datetime.id.as_str()
        ]
    );
    assert!(!ids.contains(&no_time.id.as_str()));
}

#[test]
fn private_reminder_owner_query_collapses_same_target_scopes_and_filters_non_private_pending() {
    let store = test_store();
    let private_owner = TodoStore::owner(Some("u1"), "private:u1");
    let same_target_owner = TodoStore::owner(Some("u1"), "private: u1");
    let group_owner = TodoStore::owner(Some("u1"), "group:g1");
    let completed_owner = TodoStore::owner(Some("u2"), "private:u2");

    store
        .create(
            &private_owner,
            TodoItemDraft {
                title: "私聊提醒 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-15".to_owned()),
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
    store
        .create(
            &same_target_owner,
            TodoItemDraft {
                title: "私聊提醒 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-16".to_owned()),
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
    let group_item = store
        .create(
            &group_owner,
            TodoItemDraft {
                title: "群待办".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-17".to_owned()),
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
    let completed_item = store
        .create(
            &completed_owner,
            TodoItemDraft {
                title: "已完成私聊".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-18".to_owned()),
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
    store
        .complete(&completed_owner, &completed_item.id)
        .unwrap();

    assert!(
        store
            .list_private_reminder_owners()
            .unwrap()
            .candidates
            .is_empty()
    );
    store
        .set_daily_reminder_enabled(&private_owner, true)
        .unwrap();
    store
        .set_daily_reminder_enabled(&same_target_owner, true)
        .unwrap();
    let owners = store.list_private_reminder_owners().unwrap();

    assert_eq!(owners.skipped.len(), 0);
    assert_eq!(owners.candidates.len(), 1);
    assert_eq!(owners.candidates[0].owner_key, "u1");
    assert_eq!(owners.candidates[0].private_target_id, "u1");
    assert_eq!(
        owners.candidates[0]
            .private_scope_keys
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["private:u1".to_owned(), "private: u1".to_owned(),])
    );

    let pending = store
        .list_pending_for_private_scopes(
            &owners.candidates[0].owner_key,
            &owners.candidates[0].private_scope_keys,
        )
        .unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>(),
        vec!["私聊提醒 A", "私聊提醒 B"]
    );
    assert!(pending.iter().all(|item| item.id != group_item.id));
}

#[test]
fn private_reminder_owner_query_reports_conflicts_and_invalid_scopes() {
    let store = test_store();
    let conflict_a = TodoStore::owner(Some("u2"), "private:u2");
    let conflict_b = TodoStore::owner(Some("u2"), "private:other");
    let invalid_owner = TodoStore::owner(Some("u3"), "private:");

    for owner in [&conflict_a, &conflict_b, &invalid_owner] {
        store
            .create(
                owner,
                TodoItemDraft {
                    title: format!("待办-{}", owner.scope_key),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-06-15".to_owned()),
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
        store.set_daily_reminder_enabled(owner, true).unwrap();
    }

    let owners = store.list_private_reminder_owners().unwrap();

    assert!(owners.candidates.is_empty());
    assert_eq!(owners.skipped.len(), 2);
    let skipped_by_owner = owners
        .skipped
        .iter()
        .map(|item| (item.owner_key.as_str(), item))
        .collect::<BTreeMap<_, _>>();

    let conflict = skipped_by_owner.get("u2").unwrap();
    assert_eq!(
        conflict.reason,
        TodoReminderOwnerSkipReason::ConflictingPrivateTargets
    );
    assert_eq!(
        conflict.parsed_target_ids,
        vec!["other".to_owned(), "u2".to_owned()]
    );

    let invalid = skipped_by_owner.get("u3").unwrap();
    assert_eq!(
        invalid.reason,
        TodoReminderOwnerSkipReason::InvalidPrivateScope
    );
    assert!(invalid.parsed_target_ids.is_empty());
}

#[test]
fn completed_at_filter_uses_shanghai_date() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let today = fixed_context().local_date();
    let yesterday = today - Duration::days(1);
    let before_yesterday = today - Duration::days(2);

    let old = store
        .create(
            &owner,
            TodoItemDraft {
                title: "旧完成".to_owned(),
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
    let local_yesterday = store
        .create(
            &owner,
            TodoItemDraft {
                title: "上海昨天完成".to_owned(),
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
    let due_old_completed_today = store
        .create(
            &owner,
            TodoItemDraft {
                title: "截止早但今天完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-01-01".to_owned()),
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
    let missing_completed_at = store
        .create(
            &owner,
            TodoItemDraft {
                title: "缺完成时间".to_owned(),
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
    for item in [
        &old,
        &local_yesterday,
        &due_old_completed_today,
        &missing_completed_at,
    ] {
        store.complete(&owner, &item.id).unwrap();
    }

    let mut items = store.list_all(&owner).unwrap();
    for item in &mut items {
        // 本测试关注完成时间过滤；created_at 固定为同一值，
        // 避免测试运行跨秒时影响 list_all 的创建时间倒序断言。
        item.created_at = "2026-06-10T00:00:00+08:00".to_owned();
        item.updated_at = item.created_at.clone();
        if item.id == old.id {
            item.completed_at = Some(completed_at_on(before_yesterday, 8));
        } else if item.id == local_yesterday.id {
            item.completed_at = Some("2026-06-08T20:30:00+00:00".to_owned());
        } else if item.id == due_old_completed_today.id {
            item.completed_at = Some(completed_at_on(today, 1));
        } else if item.id == missing_completed_at.id {
            item.completed_at = None;
        }
    }
    store.set_items_for_test(&owner, &items).unwrap();

    let yesterday_before = store.list_completed_before(&owner, yesterday).unwrap();
    assert_eq!(
        yesterday_before
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![old.id.as_str()]
    );

    let up_to_yesterday = store.list_completed_before(&owner, today).unwrap();
    assert_eq!(
        up_to_yesterday
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![old.id.as_str(), local_yesterday.id.as_str()]
    );

    let completed = store.list_completed(&owner).unwrap();
    assert_eq!(
        completed
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            due_old_completed_today.id.as_str(),
            local_yesterday.id.as_str(),
            old.id.as_str(),
            missing_completed_at.id.as_str()
        ]
    );
    assert!(
        completed
            .iter()
            .all(|item| item.status == TodoStatus::Completed)
    );

    let listed_all = store.list_all(&owner).unwrap();
    assert_eq!(
        listed_all
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            old.id.as_str(),
            local_yesterday.id.as_str(),
            due_old_completed_today.id.as_str(),
            missing_completed_at.id.as_str()
        ]
    );
}

#[test]
fn delete_completed_by_ids_filters_owner_scope_and_status_in_transaction() {
    let store = test_store();
    let fixture = seed_delete_by_status_fixture(&store, TodoStatus::Completed);

    assert_delete_by_status_keeps_filters(&store, &fixture, TodoStatus::Completed);
}

#[test]
fn shared_query_defaults_to_ten_and_reports_total_count() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    for index in 1..=12 {
        store
            .create(&owner, draft_with_title(&format!("第 {index} 条")))
            .unwrap();
    }

    let page = store.query_todos(&owner, &TodoQuery::default()).unwrap();

    assert_eq!(page.total_count, 12);
    assert_eq!(page.items.len(), TODO_QUERY_DEFAULT_LIMIT);
    assert_eq!(page.limit, TODO_QUERY_DEFAULT_LIMIT);
}

#[test]
fn shared_query_rejects_non_pending_overdue_statuses() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let now = qq_maid_common::time_context::parse_local_datetime_for_comparison(
        fixed_context().current_time(),
    )
    .unwrap();

    for status in [TodoQueryStatus::Completed, TodoQueryStatus::All] {
        let err = store
            .query_todos(
                &owner,
                &TodoQuery {
                    status,
                    time: Some(TodoQueryTimeFilter::Overdue { now }),
                    ..TodoQuery::default()
                },
            )
            .unwrap_err();
        assert_eq!(err.code(), "bad_request");
        assert_eq!(err.message(), "逾期筛选只适用于未完成待办。");
    }
}

#[test]
fn shared_query_combines_time_status_keyword_and_keeps_scope_isolation() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let other = TodoStore::owner(Some("u2"), "private:u2");
    let ctx = fixed_context();
    let now = qq_maid_common::time_context::parse_local_datetime_for_comparison(ctx.current_time())
        .unwrap();

    let create = |owner: &TodoOwner,
                  title: &str,
                  detail: Option<&str>,
                  due_date: Option<&str>,
                  due_at: Option<&str>| {
        store
            .create(
                owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: detail.map(str::to_owned),
                    due_date: due_date.map(str::to_owned),
                    due_at: due_at.map(str::to_owned),
                    time_precision: if due_at.is_some() {
                        TodoTimePrecision::DateTime
                    } else if due_date.is_some() {
                        TodoTimePrecision::Date
                    } else {
                        TodoTimePrecision::None
                    },
                    ..draft_with_title(title)
                },
            )
            .unwrap()
    };

    let overdue = create(&owner, "逾期报告", None, Some("2026-06-09"), None);
    let today = create(&owner, "今天事项", None, Some("2026-06-10"), None);
    let utc_today = create(
        &owner,
        "UTC 跨日事项",
        None,
        None,
        Some("2026-06-09T16:30:00+00:00"),
    );
    let tomorrow = create(
        &owner,
        "项目 A 报告",
        Some("提交报销报告"),
        Some("2026-06-11"),
        None,
    );
    let completed = create(&owner, "项目 A 已完成", None, Some("2026-06-12"), None);
    let no_due = create(&owner, "无日期事项", None, None, None);
    create(&other, "项目 A 报告", None, Some("2026-06-11"), None);
    store.complete(&owner, &completed.id).unwrap();

    let today_page = store
        .query_todos(
            &owner,
            &TodoQuery {
                time: Some(TodoQueryTimeFilter::DateRange {
                    start: ctx.local_date(),
                    end: ctx.local_date(),
                    field: TodoListDateField::Planned,
                }),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(
        today_page
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![today.id.as_str(), utc_today.id.as_str()]
    );

    let combined = store
        .query_todos(
            &owner,
            &TodoQuery {
                time: Some(TodoQueryTimeFilter::DateRange {
                    start: ctx.local_date() + Duration::days(1),
                    end: ctx.local_date() + Duration::days(1),
                    field: TodoListDateField::Planned,
                }),
                keyword: Some("项目 A 报告".to_owned()),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(combined.total_count, 1);
    assert_eq!(combined.items[0].id, tomorrow.id);

    let week = store
        .query_todos(
            &owner,
            &TodoQuery {
                status: TodoQueryStatus::All,
                time: Some(TodoQueryTimeFilter::DateRange {
                    start: ctx.local_date() - Duration::days(2),
                    end: ctx.local_date() + Duration::days(4),
                    field: TodoListDateField::Planned,
                }),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(week.total_count, 5);

    let overdue_page = store
        .query_todos(
            &owner,
            &TodoQuery {
                time: Some(TodoQueryTimeFilter::Overdue { now }),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(overdue_page.items[0].id, overdue.id);
    assert_eq!(overdue_page.items[1].id, utc_today.id);
    assert_eq!(overdue_page.total_count, 2);

    let no_due_page = store
        .query_todos(
            &owner,
            &TodoQuery {
                time: Some(TodoQueryTimeFilter::NoDueDate),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(no_due_page.items[0].id, no_due.id);

    let completed_page = store
        .query_todos(
            &owner,
            &TodoQuery {
                status: TodoQueryStatus::Completed,
                keyword: Some("项目 A".to_owned()),
                ..TodoQuery::default()
            },
        )
        .unwrap();
    assert_eq!(completed_page.total_count, 1);
    assert_eq!(completed_page.items[0].id, completed.id);
}
