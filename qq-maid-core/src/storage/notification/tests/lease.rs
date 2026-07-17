use serde_json::json;

use super::*;

fn multipart_request(dedupe_key: &str) -> NotificationUpsert {
    NotificationUpsert {
        source_type: "ops".to_owned(),
        source_id: "ops-1".to_owned(),
        dedupe_key: dedupe_key.to_owned(),
        target: PushTarget::qq_official(PushTargetType::Private, "u1"),
        channel: "push".to_owned(),
        kind: "ops_result".to_owned(),
        payload: json!({
            "parts": [
                {"message_type":"markdown", "text":"part-1"},
                {"message_type":"markdown", "text":"part-2"}
            ]
        }),
        scheduled_at: "2020-01-01T09:00:00+08:00".to_owned(),
        max_attempts: 3,
        reactivate_cancelled: false,
    }
}

fn persist_first_part_and_retry(
    store: &NotificationOutboxStore,
    request: NotificationUpsert,
) -> NotificationTask {
    let task = store.upsert(request).unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    assert_eq!(
        store.mark_part_delivered(task.id, "worker-a", 0).unwrap(),
        NotificationWriteOutcome::Applied
    );
    assert_eq!(
        store
            .mark_failed(task.id, "worker-a", "temporary", 0)
            .unwrap(),
        NotificationWriteOutcome::Applied
    );
    store.get_by_dedupe_key(&task.dedupe_key).unwrap().unwrap()
}

#[test]
fn upsert_same_delivery_snapshot_keeps_confirmed_part_prefix() {
    let store = test_store();
    let request = multipart_request("ops:same:result");
    let retry = persist_first_part_and_retry(&store, request.clone());
    assert_eq!(retry.delivered_parts, 1);

    let resubmitted = store.upsert(request).unwrap();

    assert_eq!(resubmitted.status, NotificationStatus::Pending);
    assert_eq!(resubmitted.delivered_parts, 1);
}

#[test]
fn upsert_changed_payload_resets_confirmed_part_prefix() {
    let store = test_store();
    let mut request = multipart_request("ops:payload-change:result");
    persist_first_part_and_retry(&store, request.clone());
    request.payload = json!({
        "parts": [
            {"message_type":"markdown", "text":"new-part-1"},
            {"message_type":"markdown", "text":"new-part-2"}
        ]
    });

    let resubmitted = store.upsert(request).unwrap();

    assert_eq!(resubmitted.delivered_parts, 0);
    assert_eq!(resubmitted.payload["parts"][0]["text"], "new-part-1");
}

#[test]
fn upsert_changed_target_resets_confirmed_part_prefix() {
    let store = test_store();
    let mut request = multipart_request("ops:target-change:result");
    persist_first_part_and_retry(&store, request.clone());
    request.target = PushTarget::onebot11("bot-b", PushTargetType::Group, "group-b");

    let resubmitted = store.upsert(request).unwrap();

    assert_eq!(resubmitted.delivered_parts, 0);
    assert_eq!(resubmitted.target.platform, "onebot11");
    assert_eq!(resubmitted.target.account_id.as_deref(), Some("bot-b"));
    assert_eq!(resubmitted.target.target_type, PushTargetType::Group);
    assert_eq!(resubmitted.target.target_id, "group-b");
}

#[test]
fn upsert_during_sending_preserves_current_snapshot_and_lease() {
    let store = test_store();
    let mut replacement = multipart_request("ops:sending-upsert:result");
    let original = store.upsert(replacement.clone()).unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    store
        .mark_part_delivered(original.id, "worker-a", 0)
        .unwrap();
    replacement.target = PushTarget::onebot11("bot-b", PushTargetType::Group, "group-b");
    replacement.payload = json!({"parts":[{"message_type":"text", "text":"replacement"}]});

    let unchanged = store.upsert(replacement).unwrap();

    assert_eq!(unchanged.status, NotificationStatus::Sending);
    assert_eq!(unchanged.locked_by.as_deref(), Some("worker-a"));
    assert_eq!(unchanged.delivered_parts, 1);
    assert_eq!(unchanged.target, original.target);
    assert_eq!(unchanged.payload, original.payload);
}

#[test]
fn stale_worker_cannot_advance_or_override_new_worker_retry() {
    let store = test_store();
    let task = store
        .upsert(multipart_request("ops:lease-retry:result"))
        .unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    store
        .claim_due("worker-b", 10, "9999-01-01T00:00:00+08:00")
        .unwrap();

    assert_eq!(
        store.mark_part_delivered(task.id, "worker-a", 0).unwrap(),
        NotificationWriteOutcome::LeaseLost
    );
    assert_eq!(
        store.mark_sent(task.id, "worker-a", 0).unwrap(),
        NotificationWriteOutcome::LeaseLost
    );
    assert_eq!(
        store
            .mark_failed(task.id, "worker-b", "retry by current worker", 60)
            .unwrap(),
        NotificationWriteOutcome::Applied
    );
    assert_eq!(
        store
            .mark_failed(task.id, "worker-a", "stale overwrite", 60)
            .unwrap(),
        NotificationWriteOutcome::LeaseLost
    );
    let retry = store
        .get_by_dedupe_key("ops:lease-retry:result")
        .unwrap()
        .unwrap();
    assert_eq!(retry.status, NotificationStatus::Retry);
    assert_eq!(retry.last_error.as_deref(), Some("retry by current worker"));
}

#[test]
fn stale_worker_cannot_override_new_worker_sent_state() {
    let store = test_store();
    let task = store
        .upsert(multipart_request("ops:lease-sent:result"))
        .unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    store
        .claim_due("worker-b", 10, "9999-01-01T00:00:00+08:00")
        .unwrap();
    assert_eq!(
        store.mark_part_delivered(task.id, "worker-b", 0).unwrap(),
        NotificationWriteOutcome::Applied
    );
    assert_eq!(
        store.mark_part_delivered(task.id, "worker-b", 1).unwrap(),
        NotificationWriteOutcome::Applied
    );
    assert_eq!(
        store.mark_sent(task.id, "worker-b", 2).unwrap(),
        NotificationWriteOutcome::Applied
    );
    assert_eq!(
        store
            .mark_failed(task.id, "worker-a", "stale overwrite", 60)
            .unwrap(),
        NotificationWriteOutcome::LeaseLost
    );
    let sent = store
        .get_by_dedupe_key("ops:lease-sent:result")
        .unwrap()
        .unwrap();
    assert_eq!(sent.status, NotificationStatus::Sent);
    assert!(sent.last_error.is_none());
}

#[test]
fn delivered_part_refreshes_worker_lock_timestamp() {
    let store = test_store();
    let task = store
        .upsert(multipart_request("ops:lock-refresh:result"))
        .unwrap();
    store
        .claim_due("worker-a", 10, "2020-01-01T00:00:00+08:00")
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE notification_outbox SET locked_at = '2000-01-01T00:00:00+08:00' WHERE id = ?1",
            [task.id],
        )
        .unwrap();

    assert_eq!(
        store.mark_part_delivered(task.id, "worker-a", 0).unwrap(),
        NotificationWriteOutcome::Applied
    );
    let refreshed = store
        .get_by_dedupe_key("ops:lock-refresh:result")
        .unwrap()
        .unwrap();
    assert_ne!(
        refreshed.locked_at.as_deref(),
        Some("2000-01-01T00:00:00+08:00")
    );
    assert_eq!(refreshed.locked_by.as_deref(), Some("worker-a"));
}
