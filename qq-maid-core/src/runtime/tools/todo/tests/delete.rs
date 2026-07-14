use super::support::*;
use super::*;

#[tokio::test]
async fn delete_tool_number_clarification_includes_pending_candidates_without_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let item = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中也能永久删除".to_owned(),
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
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
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
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "delete_todos");
            assert_eq!(request.candidates.len(), 1);
            assert_eq!(request.candidates[0].id, item.id);
            assert_eq!(request.candidates[0].status, TodoStatus::Pending);
        }
        other => panic!("expected delete clarification pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_all_completed_zero_match_does_not_create_pending() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": null, "all_status": "completed"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
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
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn delete_tool_query_unique_creates_single_delete_pending() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "和老公出门".to_owned(),
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
    let delete_tool = DeleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "和老公出门", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert!(output["message"].as_str().unwrap().contains("和老公出门"));
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
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(item_ids.len(), 1);
            assert_eq!(status, TodoStatus::Pending);
        }
        other => panic!("expected delete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_query_multiple_creates_clarification_without_snapshot_pollution() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let visible = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "用户可见第一条".to_owned(),
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
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "飞机票 6号".to_owned(),
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
    let _second = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "买6号飞机票".to_owned(),
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
    todo_store.complete(&owner, &first.id).unwrap();
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
    session.remember_last_todo_query(&owner.key, "all", "全部待办", vec![visible.id.clone()]);
    session_store.save(&mut session).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "飞机票", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
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
    assert_eq!(
        session.last_todo_query.unwrap().result_ids,
        vec![visible.id]
    );
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoClarify { request, .. }) => {
            assert_eq!(request.tool_name, "delete_todos");
            assert_eq!(request.candidates.len(), 2);
        }
        other => panic!("expected clarification pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_query_pending_match_creates_confirmation() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "还没做不能永久删".to_owned(),
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
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": null, "query": "不能永久删", "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
    assert_eq!(output["requires_confirmation"], true);
    assert_eq!(output["pending_action"], "delete");
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
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoBulkDelete {
            item_ids, status, ..
        }) => {
            assert_eq!(item_ids.len(), 1);
            assert_eq!(status, TodoStatus::Pending);
        }
        other => panic!("expected pending bulk delete operation, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_numbers_prefer_current_task_query_over_stale_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let pending = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中".to_owned(),
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
    let cancelled_a = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "内部已完成第一条".to_owned(),
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
    let completed_b = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "和老公出门".to_owned(),
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
    todo_store.complete(&owner, &cancelled_a.id).unwrap();
    todo_store.complete(&owner, &completed_b.id).unwrap();
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
    session.remember_last_todo_query(
        &owner.key,
        "all",
        "全部待办",
        vec![
            pending.id.clone(),
            cancelled_a.id.clone(),
            completed_b.id.clone(),
        ],
    );
    session_store.save(&mut session).unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let delete_tool = DeleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    list_tool
        .execute(test_context(), json!({"status":"completed"}))
        .await
        .unwrap();
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
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
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoDelete { item, .. }) => {
            assert_eq!(item.status, TodoStatus::Completed)
        }
        other => panic!("expected single delete pending, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_numbers_prefer_quoted_snapshot_over_latest_last_todo_query() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut list_a_ids = Vec::new();
    let mut list_b_ids = Vec::new();
    for index in 1..=7 {
        let item = todo_store
            .create(&owner, tool_test_draft(&format!("列表 A 第 {index} 条")))
            .unwrap();
        list_a_ids.push(item.id);
    }
    for index in 1..=7 {
        let item = todo_store
            .create(&owner, tool_test_draft(&format!("列表 B 第 {index} 条")))
            .unwrap();
        list_b_ids.push(item.id);
    }
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
    session.remember_last_todo_query(&owner.key, "list", "列表 B", list_b_ids.clone());
    session_store.save(&mut session).unwrap();

    let delete_tool = DeleteTodoTool::new(todo_store, session_store.clone(), notification_store)
        .with_selection_scope(SelectionScope::Scoped(Arc::from(list_a_ids.clone())));
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [7], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], true);
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
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingOperation::TodoBulkDelete { item_ids, .. }) => {
            assert_eq!(item_ids, vec![list_a_ids[6].clone()]);
            assert_ne!(item_ids, vec![list_b_ids[6].clone()]);
        }
        other => panic!("expected bulk delete pending from quoted snapshot, got {other:?}"),
    }
}

#[tokio::test]
async fn delete_tool_rejects_mixed_status_bulk_selection_without_pending() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let pending = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "进行中目标".to_owned(),
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
    let completed = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "已完成目标".to_owned(),
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
    todo_store.complete(&owner, &completed.id).unwrap();
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
    session.remember_last_todo_query(
        &owner.key,
        "all",
        "全部待办",
        vec![pending.id.clone(), completed.id.clone()],
    );
    session_store.save(&mut session).unwrap();
    let delete_tool = DeleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1, 2], "reference": null, "query": null, "all_status": null}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["error_code"], "todo_delete_mixed_status");
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
    assert!(session.pending_operation.is_none());
    assert_eq!(
        todo_store
            .get_by_id(&owner, &pending.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &completed.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}
