use super::support::*;
use super::*;

#[tokio::test]
async fn edit_tool_reuses_user_visible_snapshot_across_same_task_rounds() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut visible_ids = Vec::new();
    for title in ["第一条", "第二条", "第三条旧内容", "第四条旧内容"] {
        let item = todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: Some(format!("{title}详情")),
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
        visible_ids.push(item.id);
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
    session.remember_last_todo_query(&owner.key, "list", "进行中列表", visible_ids.clone());
    session_store.save(&mut session).unwrap();
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );

    let mut first_context = test_context();
    first_context.tool_call_id = Some("edit-third".to_owned());
    let first_prepared = edit_tool
        .prepare(
            &first_context,
            json!({
                "number": 3,
                "reference": null,
                "raw_text": "第三条不要详情了",
                "title": null,
                "detail": "",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap()
        .arguments;
    let first_output = edit_tool
        .execute(first_context, first_prepared)
        .await
        .unwrap()
        .value;
    assert_eq!(first_output["ok"], true);
    assert!(
        session_store
            .get_or_create_active(&SessionMeta::new(
                "private:u1",
                Some("u1".to_owned()),
                None,
                None,
                None,
                "qq_official",
            ))
            .unwrap()
            .last_todo_query
            .is_none()
    );

    let mut second_context = test_context();
    second_context.tool_call_id = Some("edit-fourth".to_owned());
    let second_prepared = edit_tool
        .prepare(
            &second_context,
            json!({
                "number": 4,
                "reference": null,
                "raw_text": "第四条详情也不需要",
                "title": null,
                "detail": "",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap()
        .arguments;
    let second_output = edit_tool
        .execute(second_context, second_prepared)
        .await
        .unwrap()
        .value;

    assert_eq!(second_output["ok"], true);
    let third = todo_store
        .get_by_id(&owner, &visible_ids[2])
        .unwrap()
        .expect("missing third todo");
    let fourth = todo_store
        .get_by_id(&owner, &visible_ids[3])
        .unwrap()
        .expect("missing fourth todo");
    assert_eq!(third.title, "第三条旧内容");
    assert_eq!(third.detail, None);
    assert_eq!(fourth.title, "第四条旧内容");
    assert_eq!(fourth.detail, None);
}

#[tokio::test]
async fn edit_tool_detail_patch_sets_preserves_and_clears_without_touching_other_fields() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store,
    );
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store);

    let create_item = |title: &str, detail: &str| {
        todo_store
            .create(
                &owner,
                TodoItemDraft {
                    title: title.to_owned(),
                    detail: Some(detail.to_owned()),
                    raw_text: Some("原始输入".to_owned()),
                    due_date: Some("2099-01-02".to_owned()),
                    due_at: Some("2099-01-02 10:30:00".to_owned()),
                    reminder_at: Some("2099-01-02 09:30:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                    recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::EveryNWeeks,
                    recurrence_interval_days: 0,
                    recurrence_interval: 2,
                    recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Week,
                },
            )
            .unwrap()
    };

    let set_item = create_item("设置详情", "旧详情");
    let preserve_item = create_item("保留详情", "需要保留");
    let clear_item = create_item("清空详情", "需要清除");
    let whitespace_item = create_item("空白清除", "也要清除");
    list_tool
        .execute(test_context(), json!({"status": "pending"}))
        .await
        .unwrap();

    for (index, detail, raw_text) in [
        (1, json!("  新的详情  "), "把第一条详情改成新的详情"),
        (2, Value::Null, "第二条只刷新原始输入"),
        (3, json!(""), "清除第三条详情"),
        (4, json!("   \t  "), "第四条不要备注了"),
    ] {
        let mut context = test_context();
        context.tool_call_id = Some(format!("edit-detail-{index}"));
        let prepared = edit_tool
            .prepare(
                &context,
                json!({
                    "number": index,
                    "reference": null,
                    "raw_text": raw_text,
                    "title": null,
                    "detail": detail,
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
            .unwrap()
            .arguments;
        let output = edit_tool.execute(context, prepared).await.unwrap().value;
        assert_eq!(output["ok"], true);
    }

    let set_item = todo_store.get_by_id(&owner, &set_item.id).unwrap().unwrap();
    let preserve_item = todo_store
        .get_by_id(&owner, &preserve_item.id)
        .unwrap()
        .unwrap();
    let clear_item = todo_store
        .get_by_id(&owner, &clear_item.id)
        .unwrap()
        .unwrap();
    let whitespace_item = todo_store
        .get_by_id(&owner, &whitespace_item.id)
        .unwrap()
        .unwrap();

    assert_eq!(set_item.detail.as_deref(), Some("新的详情"));
    assert_eq!(preserve_item.detail.as_deref(), Some("需要保留"));
    assert_eq!(clear_item.detail, None);
    assert_eq!(whitespace_item.detail, None);
    assert_eq!(clear_item.title, "清空详情");
    assert_eq!(clear_item.due_date.as_deref(), Some("2099-01-02"));
    assert_eq!(clear_item.due_at.as_deref(), Some("2099-01-02 10:30:00"));
    assert_eq!(
        clear_item.reminder_at.as_deref(),
        Some("2099-01-02 09:30:00")
    );
    assert_eq!(clear_item.time_precision, TodoTimePrecision::DateTime);
    assert_eq!(
        clear_item.recurrence_kind,
        crate::runtime::tools::todo::TodoRecurrenceKind::EveryNWeeks
    );
    assert_eq!(clear_item.recurrence_interval, 2);
    assert_eq!(
        clear_item.recurrence_unit,
        crate::runtime::tools::todo::TodoRecurrenceUnit::Week
    );
}

#[tokio::test]
async fn edit_tool_clears_visible_third_detail_and_list_no_longer_formats_it() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut ids = Vec::new();
    for index in 1..=4 {
        ids.push(
            todo_store
                .create(
                    &owner,
                    TodoItemDraft {
                        title: format!("第{index}条"),
                        detail: Some(format!("第{index}条原详情")),
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
                .unwrap()
                .id,
        );
    }
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let edit_tool = EditTodoTool::new(todo_store.clone(), session_store, notification_store);
    list_tool
        .execute(test_context(), json!({"status": "pending"}))
        .await
        .unwrap();

    let mut context = test_context();
    context.tool_call_id = Some("clear-visible-third".to_owned());
    let output = edit_tool
        .execute(
            context,
            json!({
                "number": 3,
                "reference": null,
                "raw_text": "清除第三条详情",
                "title": null,
                "detail": "",
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

    assert_eq!(output["updated"]["visible_number"], 3);
    let third = todo_store.get_by_id(&owner, &ids[2]).unwrap().unwrap();
    assert_eq!(third.detail, None);
    let list =
        super::format::format_todo_list_reply(&todo_store.list_pending(&owner).unwrap(), true);
    assert!(!list.text.contains("第3条原详情"));
    assert!(!list.markdown.unwrap().contains("第3条原详情"));
}

#[tokio::test]
async fn create_then_edit_reference_last_updates_same_todo_without_pending() {
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
    create_context.tool_call_id = Some("create-call".to_owned());

    create_tool
        .execute(
            create_context,
            json!({
                "content":"明天搬家",
                "title":null,
                "detail":null,
                "due_date":null,
                "due_at":null,
                "reminder_at": null,
                "time_precision":null
            }),
        )
        .await
        .unwrap();

    let mut edit_context = test_context();
    edit_context.tool_call_id = Some("edit-call".to_owned());
    let updated = edit_tool
        .execute(
            edit_context,
            json!({
                "number": null,
                "reference": "last",
                "raw_text": "修改一下时间，中午搬家",
                "title": null,
                "detail": null,
                "due_date": "2026-07-03",
                "due_at": "2026-07-03 12:00:00",
                "time_precision": "date_time"
            }),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(updated["ok"], true);
    let todos = todo_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].due_at.as_deref(), Some("2026-07-03 12:00:00"));
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
    let last_action = session.last_todo_action.expect("missing last action");
    assert_eq!(last_action.item_id, todos[0].id);
    assert_eq!(last_action.action, "edited");
}

#[tokio::test]
async fn unresolved_last_reference_creates_todo_clarification_pending() {
    let (todo_store, session_store, notification_store, _owner) = test_stores();
    let complete_tool = CompleteTodoTool::new(
        todo_store,
        session_store.clone(),
        notification_store.clone(),
    );

    let output = complete_tool
        .execute(
            test_context(),
            json!({"numbers": null, "reference": "last"}),
        )
        .await
        .unwrap()
        .value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["requires_clarification"], true);
    assert_eq!(output["pending_action"], "clarify");
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
            assert_eq!(request.tool_name, "complete_todos");
            assert_eq!(
                request.arguments,
                json!({"numbers": null, "reference": "last"})
            );
        }
        other => panic!("unexpected pending operation: {other:?}"),
    }
}
