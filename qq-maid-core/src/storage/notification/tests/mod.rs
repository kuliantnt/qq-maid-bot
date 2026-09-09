use serde_json::json;

use super::*;

mod lease;

fn test_store() -> NotificationOutboxStore {
    let database =
        SqliteDatabase::open_temp("notification-tests", NOTIFICATION_MIGRATIONS).unwrap();
    NotificationOutboxStore::new(database)
}

fn upsert_request(dedupe_key: &str, scheduled_at: &str) -> NotificationUpsert {
    NotificationUpsert {
        source_type: "todo".to_owned(),
        source_id: "1".to_owned(),
        dedupe_key: dedupe_key.to_owned(),
        target: PushTarget::qq_official(PushTargetType::Private, "u1"),
        channel: "qq".to_owned(),
        kind: "todo_reminder".to_owned(),
        payload: json!({"message_type":"text","text":"提醒"}),
        scheduled_at: scheduled_at.to_owned(),
        max_attempts: 3,
        reactivate_cancelled: false,
    }
}

#[test]
fn upsert_reuses_dedupe_key() {
    let store = test_store();
    let first = store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2026-07-03T09:00:00+08:00",
        ))
        .unwrap();
    let second = store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2026-07-03T10:00:00+08:00",
        ))
        .unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(second.scheduled_at, "2026-07-03T10:00:00+08:00");
    assert_eq!(store.list_all_for_test().unwrap().len(), 1);
}

#[test]
fn insert_if_absent_with_finalizes_only_the_first_insert() {
    let store = test_store();
    let mut finalize_calls = 0;

    let first = store
        .insert_if_absent_with(
            upsert_request("hidden_roll:first", "2026-07-03T09:00:00+08:00"),
            || {
                finalize_calls += 1;
                Ok(json!({"message_type":"text","text":"第一次结果"}))
            },
        )
        .unwrap();
    let second = store
        .insert_if_absent_with(
            upsert_request("hidden_roll:first", "2026-07-03T10:00:00+08:00"),
            || {
                finalize_calls += 1;
                Ok(json!({"message_type":"text","text":"不应覆盖"}))
            },
        )
        .unwrap();

    assert_eq!(first, NotificationInsertOutcome::Inserted);
    assert_eq!(second, NotificationInsertOutcome::AlreadyExists);
    assert_eq!(finalize_calls, 1, "只有首次插入者可以生成最终 payload");
    let task = store
        .get_by_dedupe_key("hidden_roll:first")
        .unwrap()
        .unwrap();
    assert_eq!(task.payload["text"], "第一次结果");
    assert_eq!(task.scheduled_at, "2026-07-03T09:00:00+08:00");
}

#[test]
fn insert_if_absent_with_rolls_back_failed_finalize() {
    let store = test_store();
    let dedupe_key = "hidden_roll:finalize-failure";

    let failed = store
        .insert_if_absent_with(
            upsert_request(dedupe_key, "2026-07-03T09:00:00+08:00"),
            || Err("模拟 payload 生成失败".to_owned()),
        )
        .unwrap_err();

    assert_eq!(failed.code(), "bad_request");
    assert!(
        store.get_by_dedupe_key(dedupe_key).unwrap().is_none(),
        "finalize 失败必须回滚占位任务"
    );

    let retried = store
        .insert_if_absent_with(
            upsert_request(dedupe_key, "2026-07-03T09:00:00+08:00"),
            || Ok(json!({"message_type":"text","text":"重试结果"})),
        )
        .unwrap();
    assert_eq!(retried, NotificationInsertOutcome::Inserted);
}

#[test]
fn insert_if_absent_placeholder_is_invisible_before_finalize() {
    let store = test_store();
    let worker_store = store.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        store.insert_if_absent_with(
            upsert_request("hidden_roll:uncommitted", "2020-01-01T09:00:00+08:00"),
            || {
                started_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(json!({"message_type":"text","text":"最终结果"}))
            },
        )
    });

    started_rx.recv().unwrap();
    let claimed = worker_store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    assert!(claimed.is_empty(), "未提交占位任务不能被 Worker 领取");
    resume_tx.send(()).unwrap();

    assert_eq!(
        handle.join().unwrap().unwrap(),
        NotificationInsertOutcome::Inserted
    );
    assert_eq!(worker_store.list_all_for_test().unwrap().len(), 1);
}

#[test]
fn upsert_persists_platform_target_fields() {
    let store = test_store();
    let mut request = upsert_request("todo:1:wechat", "2026-07-03T09:00:00+08:00");
    request.target = PushTarget::new(
        "wechat_service",
        Some("gh_service".to_owned()),
        PushTargetType::Private,
        "openid-1",
    );

    let task = store.upsert(request).unwrap();

    assert_eq!(task.target.platform, "wechat_service");
    assert_eq!(task.target.account_id.as_deref(), Some("gh_service"));
    assert_eq!(task.target.target_type, PushTargetType::Private);
    assert_eq!(task.target.target_id, "openid-1");
}

#[test]
fn migration_v2_defaults_legacy_rows_to_qq_official() {
    let path = std::env::temp_dir().join(format!(
        "notification-legacy-target-{}.db",
        uuid::Uuid::new_v4()
    ));
    let legacy = SqliteDatabase::open(&path, &[NOTIFICATION_OUTBOX_SCHEMA_V1]).unwrap();
    legacy
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO notification_outbox (
                source_type, source_id, dedupe_key, target_type, target_id,
                channel, kind, payload_json, scheduled_at, status,
                created_at, updated_at
             ) VALUES (
                'todo', '1', 'todo:1:reminder', 'private', 'u1',
                'qq', 'todo_reminder', '{\"message_type\":\"text\",\"text\":\"提醒\"}',
                '2026-07-03T09:00:00+08:00', 'pending',
                '2026-07-03T08:00:00+08:00', '2026-07-03T08:00:00+08:00'
             )",
            [],
        )
        .unwrap();
    drop(legacy);

    let store =
        NotificationOutboxStore::new(SqliteDatabase::open(&path, NOTIFICATION_MIGRATIONS).unwrap());
    let task = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();

    assert_eq!(task.target.platform, QQ_OFFICIAL_PLATFORM);
    assert_eq!(task.target.account_id, None);
    assert_eq!(task.target.target_type, PushTargetType::Private);
    assert_eq!(task.target.target_id, "u1");
}

#[test]
fn upsert_keeps_cancelled_by_default() {
    let store = test_store();
    store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2099-01-01T09:00:00+08:00",
        ))
        .unwrap();
    store.cancel_by_source("todo", "1").unwrap();

    let resubmitted = store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2099-01-01T10:00:00+08:00",
        ))
        .unwrap();

    assert_eq!(resubmitted.status, NotificationStatus::Cancelled);
    assert!(resubmitted.cancelled_at.is_some());
}

#[test]
fn upsert_can_reactivate_cancelled_task() {
    let store = test_store();
    store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2099-01-01T09:00:00+08:00",
        ))
        .unwrap();
    store.cancel_by_source("todo", "1").unwrap();

    let mut request = upsert_request("todo:1:reminder", "2099-01-01T10:00:00+08:00");
    request.reactivate_cancelled = true;
    let reactivated = store.upsert(request).unwrap();

    assert_eq!(reactivated.status, NotificationStatus::Pending);
    assert_eq!(reactivated.attempts, 0);
    assert_eq!(reactivated.scheduled_at, "2099-01-01T10:00:00+08:00");
    assert!(reactivated.cancelled_at.is_none());
}

#[test]
fn claim_marks_due_task_sending_once() {
    let store = test_store();
    store
        .upsert(upsert_request(
            "todo:1:reminder",
            "2020-01-01T09:00:00+08:00",
        ))
        .unwrap();

    let claimed = store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    let second = store
        .claim_due("worker-b", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();

    assert_eq!(claimed.len(), 1);
    assert!(second.is_empty());
    assert_eq!(claimed[0].status, NotificationStatus::Sending);
    assert_eq!(claimed[0].attempts, 1);
}

#[test]
fn failed_task_retries_until_limit() {
    let store = test_store();
    let mut request = upsert_request("todo:1:reminder", "2020-01-01T09:00:00+08:00");
    request.max_attempts = 1;
    let task = store.upsert(request).unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();

    store
        .mark_failed(task.id, "worker-a", "temporary", 60)
        .unwrap();
    let failed = store.get_by_dedupe_key("todo:1:reminder").unwrap().unwrap();
    assert_eq!(failed.status, NotificationStatus::Failed);
}
