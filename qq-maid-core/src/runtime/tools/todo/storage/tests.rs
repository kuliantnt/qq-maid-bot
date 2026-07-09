use super::*;
use chrono::{Duration, FixedOffset, TimeZone};
use qq_maid_common::time_context::RequestTimeContext;

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

fn draft_with_title(title: &str) -> TodoItemDraft {
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
    }
}

struct DeleteByStatusFixture {
    owner: TodoOwner,
    other_owner: TodoOwner,
    pending_id: String,
    matching_id: String,
    protected_other_status_id: String,
    other_owner_matching_id: String,
    protected_other_status: TodoStatus,
}

fn create_todo_for_test(store: &TodoStore, owner: &TodoOwner, title: &str) -> TodoItem {
    store.create(owner, draft_with_title(title)).unwrap()
}

fn seed_delete_by_status_fixture(
    store: &TodoStore,
    delete_status: TodoStatus,
) -> DeleteByStatusFixture {
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let other_owner = TodoStore::owner(Some("u2"), "group:g1");

    let pending = create_todo_for_test(store, &owner, "未完成");
    let completed = create_todo_for_test(store, &owner, "已完成");
    let other_owner_item = create_todo_for_test(store, &other_owner, "其他用户同状态");

    store.complete(&owner, &completed.id).unwrap();
    match &delete_status {
        TodoStatus::Completed => store.complete(&other_owner, &other_owner_item.id).unwrap(),
        TodoStatus::Pending => {
            unreachable!("terminal delete fixture only covers archived statuses")
        }
    };

    let (matching_id, protected_other_status_id, protected_other_status) = match delete_status {
        TodoStatus::Completed => (completed.id, pending.id.clone(), TodoStatus::Pending),
        TodoStatus::Pending => {
            unreachable!("terminal delete fixture only covers archived statuses")
        }
    };

    DeleteByStatusFixture {
        owner,
        other_owner,
        pending_id: pending.id,
        matching_id,
        protected_other_status_id,
        other_owner_matching_id: other_owner_item.id,
        protected_other_status,
    }
}

fn delete_by_status_for_test(
    store: &TodoStore,
    owner: &TodoOwner,
    status: TodoStatus,
    ids: &[String],
) -> TodoBulkDeleteOutcome {
    match status {
        TodoStatus::Completed => store.delete_completed_by_ids(owner, ids),
        TodoStatus::Pending => store.delete_pending_by_ids(owner, ids),
    }
    .unwrap()
}

fn assert_delete_by_status_keeps_filters(
    store: &TodoStore,
    fixture: &DeleteByStatusFixture,
    delete_status: TodoStatus,
) {
    let outcome = delete_by_status_for_test(
        store,
        &fixture.owner,
        delete_status.clone(),
        &[
            fixture.pending_id.clone(),
            fixture.matching_id.clone(),
            fixture.protected_other_status_id.clone(),
            fixture.other_owner_matching_id.clone(),
            "999".to_owned(),
        ],
    );

    assert_eq!(outcome.deleted_count, 1);
    assert_eq!(outcome.skipped_ids.len(), 4);

    let own_items = store.list_all(&fixture.owner).unwrap();
    assert!(own_items.iter().all(|item| item.id != fixture.matching_id));
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == fixture.pending_id)
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        own_items
            .iter()
            .find(|item| item.id == fixture.protected_other_status_id)
            .unwrap()
            .status,
        fixture.protected_other_status
    );
    assert_eq!(
        store.list_all(&fixture.other_owner).unwrap()[0].status,
        delete_status
    );
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
fn enrich_draft_time_from_text_uses_daypart_default_datetime() {
    let ctx = fixed_context();
    let mut draft = draft_with_title("检查发布清单");

    enrich_draft_time_from_text(&mut draft, "下午检查发布清单", &ctx);

    assert_eq!(draft.due_date.as_deref(), Some("2026-06-10"));
    assert_eq!(draft.due_at.as_deref(), Some("2026-06-10 15:00:00"));
    assert_eq!(draft.time_precision, TodoTimePrecision::DateTime);
}

#[test]
fn enrich_draft_time_from_text_preserves_date_and_daypart_combo() {
    let ctx = fixed_context();
    let mut draft = draft_with_title("项目 A");

    enrich_draft_time_from_text(&mut draft, "周四下午项目 A 完成初稿", &ctx);

    assert_eq!(draft.due_date.as_deref(), Some("2026-06-11"));
    assert_eq!(draft.due_at.as_deref(), Some("2026-06-11 15:00:00"));
    assert_eq!(draft.time_precision, TodoTimePrecision::DateTime);
}

#[test]
fn enrich_draft_time_from_text_does_not_override_explicit_datetime() {
    let ctx = fixed_context();
    let mut draft = draft_with_title("开会");
    draft.due_at = Some("2026-06-11 10:00:00".to_owned());
    draft.time_precision = TodoTimePrecision::DateTime;

    enrich_draft_time_from_text(&mut draft, "明天 10 点开会", &ctx);

    assert_eq!(draft.due_at.as_deref(), Some("2026-06-11 10:00:00"));
    assert_eq!(draft.time_precision, TodoTimePrecision::DateTime);
}

#[test]
fn enrich_draft_time_from_text_sets_relative_minute_reminder_only() {
    let ctx = fixed_context();
    let mut draft = draft_with_title("喝水");

    enrich_draft_time_from_text(&mut draft, "10 分钟后提醒我喝水", &ctx);

    assert_eq!(draft.reminder_at.as_deref(), Some("2026-06-10 09:10:00"));
    assert_eq!(draft.due_date, None);
    assert_eq!(draft.due_at, None);
    assert_eq!(draft.time_precision, TodoTimePrecision::DateTime);
}

#[test]
fn store_isolates_owners_and_deletes() {
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    assert_eq!(item.id, "1");
    assert_eq!(store.list_pending(&owner_b).unwrap().len(), 0);
    assert_eq!(store.search_pending(&owner_a, "交通").unwrap()[0].id, "1");

    let deleted = store.delete_by_ids(&owner_a, &["1".to_owned()]).unwrap();
    assert_eq!(deleted.deleted_count, 1);
    assert!(store.list_pending(&owner_a).unwrap().is_empty());
    let all_items = store.list_all(&owner_a).unwrap();
    assert!(all_items.is_empty());
}

#[test]
fn sqlite_ids_are_stable_and_not_reused_after_delete() {
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    let deleted = store
        .delete_by_ids(&owner, std::slice::from_ref(&first.id))
        .unwrap();
    assert_eq!(deleted.deleted_count, 1);
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                    recurrence_interval_days: 0,
                    recurrence_interval: 0,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                },
                TodoItemDraft {
                    title: "   ".to_owned(),
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
            ],
        )
        .unwrap_err();

    assert_eq!(err.code(), "bad_request");
    assert!(store.list_pending(&owner).unwrap().is_empty());
}

#[test]
fn explicit_due_at_is_not_overwritten_by_reminder() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let item = store
        .create(
            &owner,
            TodoItemDraft {
                title: "装宽带".to_owned(),
                detail: None,
                raw_text: Some("1 月 1 日 9:30 提醒，10 点上门装宽带".to_owned()),
                due_date: None,
                due_at: Some("2099-01-01 10:00:00".to_owned()),
                reminder_at: Some("2099-01-01 09:30:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    // 到期与提醒解耦：显式 due_at 保持原值，不被 reminder_at 覆盖。
    assert_eq!(item.reminder_at.as_deref(), Some("2099-01-01 09:30:00"));
    assert_eq!(item.due_at.as_deref(), Some("2099-01-01 10:00:00"));
}

#[test]
fn reminder_only_create_keeps_due_at_empty() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let item = store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查日志".to_owned(),
                detail: None,
                raw_text: Some("明天 9:30 提醒我检查日志".to_owned()),
                due_date: None,
                due_at: None,
                reminder_at: Some("2099-01-01 09:30:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();

    // 到期与提醒解耦：纯提醒待办不再自动回填 due_at / due_date。
    assert_eq!(item.reminder_at.as_deref(), Some("2099-01-01 09:30:00"));
    assert_eq!(item.due_at, None);
    assert_eq!(item.due_date, None);
}

#[test]
fn edit_can_explicitly_clear_recurrence_even_when_text_mentions_daily() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = store
        .create(
            &owner,
            TodoItemDraft {
                title: "喝水".to_owned(),
                detail: None,
                raw_text: Some("每天 9 点提醒我喝水".to_owned()),
                due_date: None,
                due_at: None,
                reminder_at: Some("2099-01-01 09:00:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::Daily,
                recurrence_interval_days: 1,
                recurrence_interval: 1,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            },
        )
        .unwrap();
    assert_eq!(
        item.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::Daily
    );

    let patch = crate::runtime::tools::todo::TodoEditPatch {
        recurrence_kind: Some(crate::runtime::tools::todo::TodoRecurrenceKind::None),
        ..Default::default()
    };
    let draft = crate::runtime::tools::todo::edit_patch::apply_to_draft(
        TodoItemDraft::from_item(&item, "不要每天提醒了"),
        &patch,
        "不要每天提醒了",
    );
    let updated = store.edit(&owner, &item.id, draft).unwrap();

    assert_eq!(
        updated.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::None
    );
    assert_eq!(updated.recurrence_interval_days, 0);
}

#[test]
fn create_normalizes_recurrence_from_text_and_structured_fields() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let cases = [
        (
            "每天 9 点提醒我喝水",
            crate::runtime::tools::todo::TodoRecurrenceKind::Daily,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            1,
            1,
        ),
        (
            "每周提醒我复盘",
            crate::runtime::tools::todo::TodoRecurrenceKind::Weekly,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Week,
            1,
            0,
        ),
        (
            "每月提醒我交房租",
            crate::runtime::tools::todo::TodoRecurrenceKind::Monthly,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Month,
            1,
            0,
        ),
        (
            "每年提醒我体检",
            crate::runtime::tools::todo::TodoRecurrenceKind::Yearly,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Year,
            1,
            0,
        ),
        (
            "每隔 3 个月提醒我检查账单",
            crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMonths,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Month,
            3,
            0,
        ),
        (
            "每分钟报一次时间",
            crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMinutes,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Minute,
            1,
            0,
        ),
        (
            "每隔 30 分钟检查一次状态",
            crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMinutes,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Minute,
            30,
            0,
        ),
        (
            "每 2 小时提醒我休息",
            crate::runtime::tools::todo::TodoRecurrenceKind::EveryNHours,
            crate::runtime::tools::todo::TodoRecurrenceUnit::Hour,
            2,
            0,
        ),
    ];

    for (raw_text, kind, unit, interval, interval_days) in cases {
        let item = store
            .create(
                &owner,
                TodoItemDraft {
                    title: raw_text.to_owned(),
                    raw_text: Some(raw_text.to_owned()),
                    reminder_at: Some("2099-01-01 09:00:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                    recurrence_kind: kind.clone(),
                    recurrence_unit: unit,
                    recurrence_interval: interval,
                    recurrence_interval_days: interval_days,
                    ..draft_with_title(raw_text)
                },
            )
            .unwrap();
        assert_eq!(item.recurrence_kind, kind, "{raw_text}");
        assert_eq!(item.recurrence_unit, unit, "{raw_text}");
        assert_eq!(item.recurrence_interval, interval, "{raw_text}");
        assert_eq!(item.recurrence_interval_days, interval_days, "{raw_text}");
    }
}

#[test]
fn recurrence_normalize_rejects_mismatched_every_n_unit_and_too_large_interval() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let zero_minute = store
        .create(
            &owner,
            TodoItemDraft {
                title: "每 0 分钟检查状态".to_owned(),
                reminder_at: Some("2099-01-01 09:00:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMinutes,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Minute,
                ..draft_with_title("每 0 分钟检查状态")
            },
        )
        .unwrap_err();
    assert_eq!(zero_minute.code(), "bad_request");
    assert!(zero_minute.message().contains("正整数"));

    let mismatch = store
        .create(
            &owner,
            TodoItemDraft {
                title: "每隔 3 个月检查账单".to_owned(),
                due_date: Some("2099-01-01".to_owned()),
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::EveryNMonths,
                recurrence_interval: 3,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                ..draft_with_title("每隔 3 个月检查账单")
            },
        )
        .unwrap_err();
    assert_eq!(mismatch.code(), "bad_request");
    assert!(mismatch.message().contains("单位与重复规则不一致"));

    let too_large = store
        .create(
            &owner,
            TodoItemDraft {
                title: "每隔 6 年检查证件".to_owned(),
                due_date: Some("2099-01-01".to_owned()),
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::EveryNYears,
                recurrence_interval: 6,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Year,
                ..draft_with_title("每隔 6 年检查证件")
            },
        )
        .unwrap_err();
    assert_eq!(too_large.code(), "bad_request");
    assert!(too_large.message().contains("最多支持 5 年"));
}

#[test]
fn complete_many_with_recurrence_rolls_back_when_later_advance_fails() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let normal = store.create(&owner, draft_with_title("普通待办")).unwrap();
    let recurring = store.create(&owner, draft_with_title("重复待办")).unwrap();
    let mut items = store.list_pending(&owner).unwrap();
    for item in &mut items {
        if item.id == recurring.id {
            item.recurrence_kind = crate::runtime::tools::todo::TodoRecurrenceKind::Daily;
            item.recurrence_interval = 1;
            item.recurrence_interval_days = 1;
            item.recurrence_unit = crate::runtime::tools::todo::TodoRecurrenceUnit::Day;
            item.due_date = None;
            item.due_at = None;
            item.reminder_at = None;
        }
    }
    store.set_items_for_test(&owner, &items).unwrap();

    let err = store
        .complete_by_ids_with_recurrence(&owner, &[normal.id.clone(), recurring.id.clone()])
        .unwrap_err();

    assert_eq!(err.code(), "bad_request");
    assert!(err.message().contains("缺少可推进的时间字段"));
    assert_eq!(
        store.get_by_id(&owner, &normal.id).unwrap().unwrap().status,
        TodoStatus::Pending
    );
    assert_eq!(
        store
            .get_by_id(&owner, &recurring.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[test]
fn create_without_time_keeps_due_fields_empty() {
    let store = test_store();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    let item = store
        .create(
            &owner,
            TodoItemDraft {
                title: "有空再看".to_owned(),
                detail: None,
                raw_text: Some("有空再看".to_owned()),
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

    assert_eq!(item.due_date, None);
    assert_eq!(item.due_at, None);
    assert_eq!(item.reminder_at, None);
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
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
