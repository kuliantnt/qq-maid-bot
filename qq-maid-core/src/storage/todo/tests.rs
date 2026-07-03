use super::*;
use crate::util::time_context::RequestTimeContext;
use chrono::{Duration, FixedOffset, TimeZone};

fn test_store() -> TodoStore {
    TodoStore::new(SqliteDatabase::open_temp("qq-maid-todo-test", TODO_MIGRATIONS).unwrap())
}

fn fixed_context() -> RequestTimeContext {
    let offset = FixedOffset::east_opt(8 * 3600).unwrap();
    RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 10, 9, 0, 0).unwrap())
}

fn completed_at_on(date: NaiveDate, hour: u32) -> String {
    format!("{}T{hour:02}:00:00+08:00", date.format("%Y-%m-%d"))
}

#[test]
fn infers_common_chinese_dates() {
    let ctx = fixed_context();

    assert_eq!(
        infer_due_date_from_text("三天后检查日志", &ctx).unwrap(),
        ("2026-06-13".to_owned(), TodoTimePrecision::Date)
    );
    assert_eq!(
        infer_due_date_from_text("下周一处理", &ctx).unwrap(),
        ("2026-06-15".to_owned(), TodoTimePrecision::Date)
    );
    assert_eq!(
        infer_due_date_from_text("周五提交", &ctx).unwrap(),
        ("2026-06-12".to_owned(), TodoTimePrecision::Inferred)
    );
    assert_eq!(
        infer_due_date_from_text("6月15号提醒", &ctx).unwrap(),
        ("2026-06-15".to_owned(), TodoTimePrecision::Date)
    );
    assert_eq!(
        infer_due_date_from_text("月底复盘", &ctx).unwrap(),
        ("2026-06-30".to_owned(), TodoTimePrecision::Inferred)
    );
}

#[test]
fn store_isolates_owners_and_soft_deletes() {
    let store = test_store();
    let owner_a = TodoStore::owner(Some("u1"), "group:g1");
    let owner_b = TodoStore::owner(Some("u2"), "group:g1");
    let item = store
        .create(
            &owner_a,
            TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: Some("交通查询".to_owned()),
                raw_text: Some("/todo add 检查日志".to_owned()),
                due_date: Some("2026-06-15".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();

    assert_eq!(item.id, "1");
    assert_eq!(store.list_pending(&owner_b).unwrap().len(), 0);
    assert_eq!(store.search_pending(&owner_a, "交通").unwrap()[0].id, "1");

    store.cancel(&owner_a, "1").unwrap();
    assert!(store.list_pending(&owner_a).unwrap().is_empty());
    let all_items = store.list_all(&owner_a).unwrap();
    assert_eq!(all_items.len(), 1);
    assert_eq!(all_items[0].status, TodoStatus::Cancelled);
    let cancelled = all_items.iter().find(|item| item.id == "1").unwrap();
    assert_eq!(cancelled.status, TodoStatus::Cancelled);
    assert!(cancelled.cancelled_at.is_some());
}

#[test]
fn sqlite_ids_are_stable_and_not_reused_after_soft_delete() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = store
        .create(
            &owner,
            TodoItemDraft {
                title: "第一条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    store.cancel(&owner, &first.id).unwrap();
    let second = store
        .create(
            &owner,
            TodoItemDraft {
                title: "第二条".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    assert_ne!(first.id, second.id);
    assert!(second.id.parse::<i64>().unwrap() > first.id.parse::<i64>().unwrap());
}

#[test]
fn create_many_rolls_back_when_later_draft_is_invalid() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let err = store
        .create_many(
            &owner,
            vec![
                TodoItemDraft {
                    title: "第一条有效待办".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                },
                TodoItemDraft {
                    title: "   ".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    reminder_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            ],
        )
        .unwrap_err();

    assert_eq!(err.code(), "bad_request");
    assert!(store.list_pending(&owner).unwrap().is_empty());
}

#[test]
fn sqlite_store_persists_after_reopen_without_json_todo_dir() {
    let base = std::env::temp_dir().join(format!("qq-maid-todo-reopen-{}", uuid::Uuid::new_v4()));
    let path = base.join("app.db");
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let database = SqliteDatabase::open(&path, TODO_MIGRATIONS).unwrap();
    let store = TodoStore::new(database);
    let created = store
        .create(
            &owner,
            TodoItemDraft {
                title: "重开后仍存在".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    drop(store);

    let reopened = TodoStore::new(SqliteDatabase::open(&path, TODO_MIGRATIONS).unwrap());
    assert_eq!(reopened.list_pending(&owner).unwrap()[0].id, created.id);

    let legacy_todo_dir = base.join("todos");
    assert!(!legacy_todo_dir.exists());
}

#[test]
fn pending_list_sorts_by_due_time_then_id_without_changing_all_view() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

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
            },
        )
        .unwrap();
    let later_datetime = store
        .create(
            &owner,
            TodoItemDraft {
                title: "15号中午".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-15 12:30:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
            },
        )
        .unwrap();
    let earlier_date = store
        .create(
            &owner,
            TodoItemDraft {
                title: "14号全天".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-14".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    let same_day_date_only = store
        .create(
            &owner,
            TodoItemDraft {
                title: "15号全天".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-15".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    let same_time_a = store
        .create(
            &owner,
            TodoItemDraft {
                title: "同时间 A".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-15 12:30:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
            },
        )
        .unwrap();
    let same_time_b = store
        .create(
            &owner,
            TodoItemDraft {
                title: "同时间 B".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: Some("2026-06-15 12:30:00".to_owned()),
                reminder_at: None,
                time_precision: TodoTimePrecision::DateTime,
            },
        )
        .unwrap();

    let mut items = store.list_all(&owner).unwrap();
    for item in &mut items {
        item.created_at = match item.id.as_str() {
            id if id == no_time.id => "2026-06-01T00:00:00+08:00",
            id if id == later_datetime.id => "2026-06-06T00:00:00+08:00",
            id if id == earlier_date.id => "2026-06-05T00:00:00+08:00",
            id if id == same_day_date_only.id => "2026-06-04T00:00:00+08:00",
            id if id == same_time_a.id => "2026-06-03T00:00:00+08:00",
            id if id == same_time_b.id => "2026-06-02T00:00:00+08:00",
            _ => unreachable!("unexpected todo id"),
        }
        .to_owned();
        item.updated_at = item.created_at.clone();
    }
    store.set_items_for_test(&owner, &items).unwrap();

    let pending = store.list_pending(&owner).unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            earlier_date.id.as_str(),
            same_day_date_only.id.as_str(),
            later_datetime.id.as_str(),
            same_time_a.id.as_str(),
            same_time_b.id.as_str(),
            no_time.id.as_str()
        ]
    );

    let all = store.list_all(&owner).unwrap();
    assert_eq!(
        all.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        vec![
            later_datetime.id.as_str(),
            earlier_date.id.as_str(),
            same_day_date_only.id.as_str(),
            same_time_a.id.as_str(),
            same_time_b.id.as_str(),
            no_time.id.as_str()
        ]
    );
}

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
    let cancelled_owner = TodoStore::owner(Some("u3"), "private:u3");

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
            },
        )
        .unwrap();
    let cancelled_item = store
        .create(
            &cancelled_owner,
            TodoItemDraft {
                title: "已取消私聊".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-19".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    store
        .complete(&completed_owner, &completed_item.id)
        .unwrap();
    store.cancel(&cancelled_owner, &cancelled_item.id).unwrap();

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
                },
            )
            .unwrap();
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
fn completed_at_filter_uses_shanghai_date_and_bulk_cancel_preserves_completed_at() {
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
            },
        )
        .unwrap();
    let already_cancelled = store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    for item in [
        &old,
        &local_yesterday,
        &due_old_completed_today,
        &missing_completed_at,
        &already_cancelled,
    ] {
        store.complete(&owner, &item.id).unwrap();
    }

    let mut items = store.list_all(&owner).unwrap();
    for item in &mut items {
        // 本测试关注完成时间过滤和软删除语义；created_at 固定为同一值，
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
        } else if item.id == already_cancelled.id {
            item.status = TodoStatus::Cancelled;
            item.completed_at = Some(completed_at_on(before_yesterday, 9));
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

    let original_completed_at = up_to_yesterday[0].completed_at.clone();
    let outcome = store
        .cancel_completed_by_ids(
            &owner,
            &[
                old.id.clone(),
                already_cancelled.id.clone(),
                "999".to_owned(),
            ],
        )
        .unwrap();
    assert_eq!(outcome.cancelled.len(), 1);
    assert_eq!(outcome.cancelled[0].id, old.id);
    assert_eq!(outcome.skipped_ids.len(), 2);

    let all = store.list_all(&owner).unwrap();
    let cancelled = all.iter().find(|item| item.id == old.id).unwrap();
    assert_eq!(cancelled.status, TodoStatus::Cancelled);
    assert_eq!(cancelled.completed_at, original_completed_at);
    assert!(cancelled.cancelled_at.is_some());
    let listed_all = store.list_all(&owner).unwrap();
    assert!(
        listed_all
            .iter()
            .any(|item| item.status == TodoStatus::Cancelled)
    );
    assert_eq!(
        listed_all
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            old.id.as_str(),
            local_yesterday.id.as_str(),
            due_old_completed_today.id.as_str(),
            missing_completed_at.id.as_str(),
            already_cancelled.id.as_str()
        ]
    );
}

#[test]
fn delete_cancelled_by_ids_filters_owner_scope_and_status_in_transaction() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let other_owner = TodoStore::owner(Some("u2"), "group:g1");

    let pending = store
        .create(
            &owner,
            TodoItemDraft {
                title: "未完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let completed = store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let cancelled = store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let other_cancelled = store
        .create(
            &other_owner,
            TodoItemDraft {
                title: "其他用户已取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    store.complete(&owner, &completed.id).unwrap();
    store.cancel(&owner, &cancelled.id).unwrap();
    store.cancel(&other_owner, &other_cancelled.id).unwrap();

    let outcome = store
        .delete_cancelled_by_ids(
            &owner,
            &[
                pending.id.clone(),
                completed.id.clone(),
                cancelled.id.clone(),
                other_cancelled.id.clone(),
                "999".to_owned(),
            ],
        )
        .unwrap();

    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.skipped_ids.len(), 4);
    let own_items = store.list_all(&owner).unwrap();
    assert!(own_items.iter().all(|item| item.id != cancelled.id));
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == pending.id)
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == completed.id)
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        store.list_all(&other_owner).unwrap()[0].status,
        TodoStatus::Cancelled
    );
}

#[test]
fn delete_completed_by_ids_filters_owner_scope_and_status_in_transaction() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let other_owner = TodoStore::owner(Some("u2"), "group:g1");

    let pending = store
        .create(
            &owner,
            TodoItemDraft {
                title: "未完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let completed = store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let cancelled = store
        .create(
            &owner,
            TodoItemDraft {
                title: "已取消".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    let other_completed = store
        .create(
            &other_owner,
            TodoItemDraft {
                title: "其他用户已完成".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    store.complete(&owner, &completed.id).unwrap();
    store.cancel(&owner, &cancelled.id).unwrap();
    store.complete(&other_owner, &other_completed.id).unwrap();

    let outcome = store
        .delete_completed_by_ids(
            &owner,
            &[
                pending.id.clone(),
                completed.id.clone(),
                cancelled.id.clone(),
                other_completed.id.clone(),
                "999".to_owned(),
            ],
        )
        .unwrap();

    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.skipped_ids.len(), 4);
    let own_items = store.list_all(&owner).unwrap();
    assert!(own_items.iter().all(|item| item.id != completed.id));
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == pending.id)
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == cancelled.id)
            .unwrap()
            .status,
        TodoStatus::Cancelled
    );
    assert_eq!(
        store.list_all(&other_owner).unwrap()[0].status,
        TodoStatus::Completed
    );
}
