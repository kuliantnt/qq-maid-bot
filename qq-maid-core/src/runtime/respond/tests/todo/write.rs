use super::*;

#[tokio::test]
async fn todo_deprecated_slash_write_prompts_preserve_visible_snapshot() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.task_store.create(&owner, draft("第一条")).unwrap();
    let second = service.task_store.create(&owner, draft("第二条")).unwrap();

    service.respond(message("看一下待办")).await.unwrap();
    let original_snapshot = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .last_todo_query
        .expect("missing initial todo snapshot");
    assert_eq!(original_snapshot.result_ids, vec![first.id, second.id]);

    for command in [
        "/todo add 第三条",
        "/todo done 1",
        "/todo undo 1",
        "/todo edit 1 改时间",
        "/todo delete 1",
    ] {
        let response = service.respond(message(command)).await.unwrap();
        assert!(response.text.unwrap().contains("群聊当前只开放待办查询"));
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        let snapshot = session
            .last_todo_query
            .expect("deprecated slash write prompt cleared todo snapshot");
        assert_eq!(snapshot.owner_key, original_snapshot.owner_key, "{command}");
        assert_eq!(
            snapshot.query_type, original_snapshot.query_type,
            "{command}"
        );
        assert_eq!(
            snapshot.result_ids, original_snapshot.result_ids,
            "{command}"
        );
    }
}

#[tokio::test]
async fn todo_slash_write_commands_are_tool_only_and_do_not_mutate() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service.task_store.create(&owner, draft("待办一")).unwrap();

    for command in [
        "/todo add 买牛奶",
        "/todo done 1",
        "/todo undo 1",
        "/todo edit 1 改时间",
        "/todo delete 1",
    ] {
        let response = service.respond(message(command)).await.unwrap();
        assert!(response.text.unwrap().contains("群聊当前只开放待办查询"));
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        assert!(session.pending_operation.is_none(), "command={command}");
    }

    let items = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, TodoStatus::Pending);
}

#[tokio::test]
async fn todo_done_and_undo_without_arguments_still_query_completed_list() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service
        .task_store
        .create(&owner, draft("已完成项"))
        .unwrap();
    service.task_store.complete(&owner, &item.id).unwrap();

    for command in ["/todo done", "/todo undo"] {
        let response = service.respond(message(command)).await.unwrap();
        assert!(response.text.unwrap().contains("已完成项"));
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        assert_eq!(
            session
                .last_todo_query
                .as_ref()
                .map(|query| query.query_type.as_str()),
            Some("completed-list")
        );
    }
}

#[tokio::test]
async fn todo_all_and_search_remain_deterministic_queries() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
        .create(&owner, draft("查找目标"))
        .unwrap();

    let all = service.respond(message("/todo all")).await.unwrap();
    assert!(all.text.unwrap().contains("查找目标"));

    let search = service.respond(message("/todo search 查找")).await.unwrap();
    assert!(search.text.unwrap().contains("查找目标"));
}
