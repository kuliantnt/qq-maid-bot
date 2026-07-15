use super::*;

#[tokio::test]
async fn todo_clarification_llm_tool_call_completes_candidate_scope() {
    let provider = MockProvider::new().with_tool_call_json(
        "complete_todos",
        r#"{"numbers":[1],"reference":null}"#,
        "已完成待办：买票",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let ticket = service.task_store.create(&owner, draft("买票")).unwrap();
    let hotel = service.task_store.create(&owner, draft("订酒店")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(&[ticket.clone(), hotel.clone()]),
    );

    let response = service
        .respond(private_todo_message("买票那条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_resumed"));
    assert!(response.text.unwrap().contains("已完成待办"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &ticket.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &hotel.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation
            .is_none()
    );
}

#[tokio::test]
async fn todo_clarification_control_ask_again_keeps_pending_without_mutation() {
    let provider = MockProvider::new().with_tool_call_json(
        "clarification_control",
        r#"{"action":"ask_again","question":"找到多条匹配待办，请回复候选编号。"}"#,
        "找到多条匹配待办，请回复候选编号。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let first = service.task_store.create(&owner, draft("买票")).unwrap();
    let second = service
        .task_store
        .create(&owner, draft("买票确认"))
        .unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(&[first.clone(), second.clone()]),
    );

    let response = service.respond(private_todo_message("买票")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    assert!(response.text.unwrap().contains("回复候选编号"));
    for item in [first, second] {
        assert_eq!(
            service
                .task_store
                .get_by_id(&owner, &item.id)
                .unwrap()
                .unwrap()
                .status,
            TodoStatus::Pending
        );
    }
    assert!(matches!(
        todo_pending(
            service
                .session_store
                .get_or_create_active(&private_todo_meta())
                .unwrap()
                .pending_operation
                .as_ref()
        ),
        Some(TodoPendingPayload::TodoClarify { .. })
    ));
}

#[tokio::test]
async fn todo_clarification_cancel_and_expiry_do_not_mutate() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let cancelled = service.respond(private_todo_message("取消")).await.unwrap();
    assert_eq!(cancelled.command.as_deref(), Some("todo_clarify_cancel"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );

    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        "2020-01-01T00:00:00+08:00".to_owned(),
        clarification_candidates(std::slice::from_ref(&item)),
    );
    let expired = service.respond(private_todo_message("买票")).await.unwrap();
    assert_eq!(expired.command.as_deref(), Some("todo_clarify_expired"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn todo_clarification_number_target_changed_keeps_pending_without_side_effect() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": [1], "reference": null}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );
    service.task_store.complete(&owner, &item.id).unwrap();

    let response = service.respond(private_todo_message("1")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    assert!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation
            .is_some()
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}

#[tokio::test]
async fn todo_clarification_candidate_scope_does_not_persist_as_last_query() {
    let provider = MockProvider::new().with_tool_call_json(
        "complete_todos",
        r#"{"numbers":[1],"reference":null}"#,
        "已完成候选。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let unrelated = service
        .task_store
        .create(&owner, draft("无关列表项"))
        .unwrap();
    let candidate = service
        .task_store
        .create(&owner, draft("澄清候选"))
        .unwrap();
    let mut session = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "原列表", vec![unrelated.id.clone()]);
    service.session_store.save(&mut session).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&candidate)),
    );

    service
        .respond(private_todo_message("候选那条"))
        .await
        .unwrap();

    let latest = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap();
    assert_ne!(
        latest.last_todo_query.map(|query| query.result_ids),
        Some(vec![candidate.id.clone()])
    );
}

#[tokio::test]
async fn todo_clarification_loop_error_marks_failed_and_blocks_repeat_execution() {
    let provider = MockProvider::new().with_tool_call_json("weather", r#"{}"#, "不会返回");
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("查天气吧"))
        .await
        .unwrap();

    assert_eq!(
        response.command.as_deref(),
        Some("pending_execution_failed")
    );
    assert!(response.text.unwrap().contains("执行失败"));
    let pending = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap()
        .pending_operation
        .expect("failed prepared action should be retained");
    assert_eq!(
        pending.state(),
        crate::runtime::pending::PreparedActionState::Failed
    );
    assert!(matches!(
        todo_pending(Some(&pending)),
        Some(TodoPendingPayload::TodoClarify { .. })
    ));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn todo_clarification_no_tool_reply_updates_question_and_keeps_pending() {
    let provider = MockProvider::new().with_tool_loop_reply_without_tool("请再说明要选哪个候选。");
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("不太确定"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    let session = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap();
    match todo_pending(session.pending_operation.as_ref()) {
        Some(TodoPendingPayload::TodoClarify { request, .. }) => {
            assert!(request.question.contains("请再说明"));
        }
        other => panic!("expected TodoClarify pending, got {other:?}"),
    }
}

#[tokio::test]
async fn todo_clarification_delete_tool_replaces_with_confirmation_pending() {
    let provider = MockProvider::new().with_tool_call_json(
        "delete_todos",
        r#"{"numbers":[1],"reference":null}"#,
        "已发起删除确认。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("旧任务")).unwrap();
    service.task_store.complete(&owner, &item.id).unwrap();
    let item = service
        .task_store
        .get_by_id(&owner, &item.id)
        .unwrap()
        .unwrap();
    install_todo_clarification(
        &service,
        "delete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("旧任务那条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_resumed"));
    assert!(matches!(
        todo_pending(
            service
                .session_store
                .get_or_create_active(&private_todo_meta())
                .unwrap()
                .pending_operation
                .as_ref()
        ),
        Some(TodoPendingPayload::TodoDelete { .. })
    ));
}

#[tokio::test]
async fn todo_clarification_out_of_range_number_keeps_pending_without_side_effect() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service.respond(private_todo_message("2")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_wait"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert!(matches!(
        todo_pending(
            service
                .session_store
                .get_or_create_active(&private_todo_meta())
                .unwrap()
                .pending_operation
                .as_ref()
        ),
        Some(TodoPendingPayload::TodoClarify { .. })
    ));
}

#[tokio::test]
async fn todo_clarification_control_abandon_clears_pending_without_mutation() {
    let provider = MockProvider::new().with_tool_call_json(
        "clarification_control",
        r#"{"action":"abandon","question":null}"#,
        "已放弃这次澄清。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": null, "reference": "last"}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("我不处理这个了"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_abandon"));
    assert!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation
            .is_none()
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}
