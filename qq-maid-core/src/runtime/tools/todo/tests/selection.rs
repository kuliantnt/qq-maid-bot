use super::support::*;
use super::*;

#[test]
fn todo_tool_scope_uses_explicit_private_and_group_context() {
    let (_todo_store, session_store, _notification_store, _owner) = test_stores();
    for (kind, target_id, scope_id, interaction_scope_id, expected_group_id) in [
        (
            ConversationKind::Private,
            "u1",
            "platform:onebot11:account:bot-1:private:u1",
            "platform:onebot11:account:bot-1:private:u1",
            None,
        ),
        (
            ConversationKind::Group,
            "g1",
            "opaque-conversation-scope",
            "opaque-interaction-scope",
            Some("g1"),
        ),
    ] {
        let mut context = test_context();
        context.conversation.platform = "onebot11".to_owned();
        context.conversation.account_id = Some("bot-1".to_owned());
        context.conversation.kind = kind;
        context.conversation.target_id = Some(target_id.to_owned());
        context.conversation.scope_id = scope_id.to_owned();
        context.conversation.interaction_scope_id = interaction_scope_id.to_owned();

        let scope = TodoToolScope::load(&session_store, &context, None)
            .unwrap_or_else(|err| panic!("{scope_id} should load, got {err}"));

        assert_eq!(scope.session.group_id.as_deref(), expected_group_id);
        assert_eq!(scope.session.scope_key, interaction_scope_id);
        assert_eq!(scope.session.platform, "onebot11");
        assert_eq!(scope.owner.scope_key, scope_id);
    }
}

#[test]
fn todo_tool_scope_keeps_stable_private_and_group_distinct() {
    let (_todo_store, session_store, _notification_store, _owner) = test_stores();
    let mut private_context = test_context();
    private_context.conversation.scope_id =
        "platform:qq_official:account:app-1:private:u1".to_owned();
    private_context.conversation.interaction_scope_id =
        private_context.conversation.scope_id.clone();
    let mut group_context = test_context();
    group_context.conversation.kind = ConversationKind::Group;
    group_context.conversation.target_id = Some("g1".to_owned());
    group_context.conversation.scope_id = "platform:qq_official:account:app-1:group:g1".to_owned();
    group_context.conversation.interaction_scope_id =
        format!("{}:actor:u1", group_context.conversation.scope_id);

    let private_scope = TodoToolScope::load(&session_store, &private_context, None).unwrap();
    let group_scope = TodoToolScope::load(&session_store, &group_context, None).unwrap();

    assert_eq!(private_scope.session.group_id, None);
    assert_eq!(group_scope.session.group_id.as_deref(), Some("g1"));
}

#[tokio::test]
async fn prepared_number_binding_survives_previous_completion() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let first = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "搬家".to_owned(),
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
    let second = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "宽带迁移".to_owned(),
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

    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let complete_tool = CompleteTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let edit_tool = EditTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store.clone(),
    );
    let context = test_context();

    list_tool
        .execute(context.clone(), json!({"status":"pending"}))
        .await
        .unwrap();

    let complete_prepared = complete_tool
        .prepare(&context, json!({"numbers":[1], "reference": null}))
        .unwrap();
    let mut edit_context = context.clone();
    edit_context.tool_call_id = Some("call-2".to_owned());
    let edit_prepared = edit_tool
        .prepare(
            &edit_context,
            json!({
                "number": 2,
                "reference": null,
                "raw_text": "改为除了搬家还有宽带要迁移",
                "title": null,
                "detail": "除了搬家还有宽带要迁移",
                "due_date": null,
                "due_at": null,
                "reminder_at": null,
                "time_precision": null
            }),
        )
        .unwrap();

    complete_tool
        .execute(context.clone(), complete_prepared.arguments)
        .await
        .unwrap();
    let edited = edit_tool
        .execute(edit_context.clone(), edit_prepared.arguments)
        .await
        .unwrap();

    let edited_value = edited.value;
    assert_eq!(edited_value["ok"], true);
    assert_eq!(
        todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    let second_item = todo_store.get_by_id(&owner, &second.id).unwrap().unwrap();
    assert_eq!(
        second_item.detail.as_deref(),
        Some("除了搬家还有宽带要迁移")
    );
}

#[tokio::test]
async fn same_task_query_numbers_prefer_current_list_over_stale_visible_snapshot() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let stale_visible = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "旧可见列表第一条".to_owned(),
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
    let current_completed = todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "当前已完成第一条".to_owned(),
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
    todo_store.complete(&owner, &current_completed.id).unwrap();
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
        "pending",
        "旧列表",
        vec![stale_visible.id.clone()],
    );
    session_store.save(&mut session).unwrap();
    let list_tool = ListTodoTool::new(todo_store.clone(), session_store.clone());
    let restore_tool = super::RestoreTodoTool::new(
        todo_store.clone(),
        session_store,
        notification_store.clone(),
    );
    let context = test_context();

    let listed = list_tool
        .execute(context.clone(), json!({"status":"completed"}))
        .await
        .unwrap()
        .value;
    assert_eq!(listed["items"][0]["visible_number"], 1);
    assert_eq!(listed["items"][0]["title"], "当前已完成第一条");

    let restored = restore_tool
        .execute(context, json!({"numbers":[1], "reference": null}))
        .await
        .unwrap()
        .value;

    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["title"], "当前已完成第一条");
    assert_eq!(
        todo_store
            .get_by_id(&owner, &current_completed.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        todo_store
            .get_by_id(&owner, &stale_visible.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn blocked_quoted_snapshot_does_not_fallback_to_last_todo_query() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let fallback = todo_store
        .create(&owner, tool_test_draft("不应被 fallback 删除"))
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
    session.remember_last_todo_query(&owner.key, "list", "列表 B", vec![fallback.id]);
    session_store.save(&mut session).unwrap();

    let delete_tool = DeleteTodoTool::new(todo_store, session_store.clone(), notification_store)
        .with_selection_scope(SelectionScope::Blocked);
    let output = delete_tool
        .execute(
            test_context(),
            json!({"numbers": [1], "reference": null, "query": null, "all_status": null}),
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
