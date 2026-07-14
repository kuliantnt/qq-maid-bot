use super::support::*;
use super::*;

#[tokio::test]
async fn merge_numbers_use_quoted_snapshot_and_physically_delete_source() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut list_a_ids = Vec::new();
    let mut list_b_ids = Vec::new();
    for index in 1..=7 {
        let mut draft = tool_test_draft(&format!("合并列表 A 第 {index} 条"));
        draft.detail = Some(format!("A detail {index}"));
        list_a_ids.push(todo_store.create(&owner, draft).unwrap().id);
        list_b_ids.push(
            todo_store
                .create(
                    &owner,
                    tool_test_draft(&format!("合并列表 B 第 {index} 条")),
                )
                .unwrap()
                .id,
        );
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

    let merge_tool = MergeTodoTool::new(todo_store.clone(), session_store, notification_store)
        .with_selection_scope(SelectionScope::Scoped(Arc::from(list_a_ids.clone())));
    let context = test_context();
    let arguments = json!({"source_number": 7, "target_number": 6});
    let output = merge_tool
        .execute(context.clone(), arguments.clone())
        .await
        .unwrap()
        .value;
    let replayed = merge_tool.execute(context, arguments).await.unwrap().value;

    assert_eq!(output["ok"], true);
    assert_eq!(replayed, output);
    let target = todo_store
        .get_by_id(&owner, &list_a_ids[5])
        .unwrap()
        .unwrap();
    let target_detail = target.detail.unwrap_or_default();
    assert_eq!(
        target_detail
            .matches("合并来源：合并列表 A 第 7 条")
            .count(),
        1
    );
    assert_eq!(target_detail.matches("A detail 7").count(), 1);
    assert!(
        todo_store
            .get_by_id(&owner, &list_a_ids[6])
            .unwrap()
            .is_none()
    );
    assert!(
        todo_store
            .get_by_id(&owner, &list_b_ids[6])
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn merge_reminder_sync_failure_returns_structured_partial_failure() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut target_draft = tool_test_draft("目标待办");
    target_draft.reminder_at = Some("not-a-valid-reminder".to_owned());
    let target = todo_store.create(&owner, target_draft).unwrap();
    let source = todo_store
        .create(&owner, tool_test_draft("来源待办"))
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
    session.remember_last_todo_query(
        &owner.key,
        "list",
        "待办列表",
        vec![target.id.clone(), source.id.clone()],
    );
    session_store.save(&mut session).unwrap();

    let merge_tool = MergeTodoTool::new(
        todo_store.clone(),
        session_store.clone(),
        notification_store,
    );
    let context = test_context();
    let arguments = json!({"source_number": 2, "target_number": 1});
    let output = merge_tool
        .execute(context.clone(), arguments.clone())
        .await
        .unwrap()
        .value;
    let replayed = merge_tool.execute(context, arguments).await.unwrap().value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["partial_failure"], true);
    assert_eq!(output["error_code"], "todo_merge_reminder_sync_failed");
    assert_eq!(replayed, output);
    let updated_target = todo_store.get_by_id(&owner, &target.id).unwrap().unwrap();
    let target_detail = updated_target.detail.unwrap_or_default();
    assert_eq!(target_detail.matches("合并来源：来源待办").count(), 1);
    assert!(
        todo_store.get_by_id(&owner, &source.id).unwrap().is_some(),
        "source should not be deleted after reminder sync partial failure"
    );
    let saved = session_store
        .get_or_create_active(&SessionMeta::new(
            "private:u1",
            Some("u1".to_owned()),
            None,
            None,
            None,
            "qq_official",
        ))
        .unwrap();
    assert!(saved.last_todo_query.is_none());
    assert_eq!(
        saved
            .last_todo_action
            .as_ref()
            .map(|action| action.action.as_str()),
        Some("merged_partial")
    );
}

#[tokio::test]
async fn merge_source_delete_failure_replays_without_duplicate_target_update() {
    let (todo_store, session_store, notification_store, owner) = test_stores();
    let mut target_draft = tool_test_draft("目标待办");
    target_draft.detail = Some("目标详情".to_owned());
    let target = todo_store.create(&owner, target_draft).unwrap();
    let mut source_draft = tool_test_draft("来源待办");
    source_draft.detail = Some("来源详情".to_owned());
    let source = todo_store.create(&owner, source_draft).unwrap();
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
        "list",
        "待办列表",
        vec![target.id.clone(), source.id.clone()],
    );
    session_store.save(&mut session).unwrap();

    let merge_tool = MergeTodoTool::new(todo_store.clone(), session_store, notification_store)
        .with_source_delete_failure_for_test();
    let context = test_context();
    let arguments = json!({"source_number": 2, "target_number": 1});
    let output = merge_tool
        .execute(context.clone(), arguments.clone())
        .await
        .unwrap()
        .value;
    let replayed = merge_tool.execute(context, arguments).await.unwrap().value;

    assert_eq!(output["ok"], false);
    assert_eq!(output["partial_failure"], true);
    assert_eq!(output["error_code"], "todo_merge_source_delete_failed");
    assert_eq!(replayed, output);
    let updated_target = todo_store.get_by_id(&owner, &target.id).unwrap().unwrap();
    let target_detail = updated_target.detail.unwrap_or_default();
    assert_eq!(target_detail.matches("合并来源：来源待办").count(), 1);
    assert_eq!(target_detail.matches("来源详情").count(), 1);
    assert!(todo_store.get_by_id(&owner, &source.id).unwrap().is_some());
}
