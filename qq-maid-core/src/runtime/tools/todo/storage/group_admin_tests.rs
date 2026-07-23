use super::*;
use crate::{
    runtime::tools::todo::sync_reminder_task,
    storage::notification::{NotificationOutboxStore, NotificationStatus},
};

fn store() -> TodoStore {
    stores().0
}

fn stores() -> (TodoStore, NotificationOutboxStore) {
    let path = std::env::temp_dir().join(format!(
        "qq-maid-group-todo-storage-{}.db",
        uuid::Uuid::new_v4()
    ));
    let database = SqliteDatabase::open(path, crate::storage::APP_MIGRATIONS).unwrap();
    (
        TodoStore::new(database.clone()),
        NotificationOutboxStore::new(database),
    )
}

fn draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        reminder_at: None,
        time_precision: TodoTimePrecision::None,
        recurrence_kind: TodoRecurrenceKind::None,
        recurrence_interval_days: 0,
        recurrence_interval: 0,
        recurrence_unit: TodoRecurrenceUnit::Day,
    }
}

#[test]
fn group_admin_query_crosses_creators_but_not_group_or_private_scope() {
    let store = store();
    let group_a_creator_1 = TodoStore::owner(Some("u1"), "group:g1");
    let group_a_creator_2 = TodoStore::owner(Some("u2"), "group:g1");
    let group_b_creator = TodoStore::owner(Some("u1"), "group:g2");
    let private_creator = TodoStore::owner(Some("u1"), "private:u1");
    store
        .create(&group_a_creator_1, draft("A 群甲创建"))
        .unwrap();
    store
        .create(&group_a_creator_2, draft("A 群乙创建"))
        .unwrap();
    store.create(&group_b_creator, draft("B 群记录")).unwrap();
    store.create(&private_creator, draft("私聊记录")).unwrap();

    let items = store.list_pending_for_group_scope("group:g1").unwrap();
    let titles = items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(titles, vec!["A 群甲创建", "A 群乙创建"]);
}

#[test]
fn group_admin_delete_rechecks_exact_group_scope_and_pending_status() {
    let store = store();
    let owner = TodoStore::owner(Some("u2"), "group:g1");
    let pending = store.create(&owner, draft("不能跨群误删")).unwrap();

    assert!(
        store
            .delete_pending_for_group_scope_and_cancel_notification("group:g2", &pending.id)
            .unwrap()
            .is_none()
    );
    assert!(store.get_by_id(&owner, &pending.id).unwrap().is_some());

    store
        .complete_by_ids(&owner, std::slice::from_ref(&pending.id))
        .unwrap();
    assert!(
        store
            .delete_pending_for_group_scope_and_cancel_notification("group:g1", &pending.id)
            .unwrap()
            .is_none()
    );
    assert!(store.get_by_id(&owner, &pending.id).unwrap().is_some());

    let deletable = store.create(&owner, draft("当前群可删")).unwrap();
    let deleted = store
        .delete_pending_for_group_scope_and_cancel_notification("group:g1", &deletable.id)
        .unwrap()
        .unwrap();
    assert_eq!(deleted.id, deletable.id);
    assert!(store.get_by_id(&owner, &deletable.id).unwrap().is_none());
}

#[test]
fn notification_cancel_failure_rolls_back_group_todo_delete() {
    let (store, notification_store) = stores();
    let owner = TodoStore::owner(Some("u2"), "group:g1");
    let item = store
        .create(
            &owner,
            TodoItemDraft {
                reminder_at: Some("2099-07-24 09:00:00".to_owned()),
                ..draft("必须原子删除的提醒")
            },
        )
        .unwrap();
    sync_reminder_task(&notification_store, &owner, &item).unwrap();

    assert!(
        store
            .delete_group_todo_with_cancel_failure_for_test("group:g1", &item.id)
            .is_err()
    );
    assert!(store.get_by_id(&owner, &item.id).unwrap().is_some());
    let tasks = notification_store.list_all_for_test().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, NotificationStatus::Pending);
}
