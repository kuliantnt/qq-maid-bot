use super::support::*;
use crate::runtime::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};

fn draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        time_precision: TodoTimePrecision::None,
    }
}

#[tokio::test]
async fn todo_root_aliases_list_pending_items() {
    let service = test_service();

    for command in ["/todo", "/待办", "/代办", "/任务"] {
        let response = service.respond(message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"));
        let text = response.text.unwrap();
        assert!(text.contains("当前没有未完成待办"));
        assert!(!text.starts_with("null"));
    }
}

#[tokio::test]
async fn todo_query_writes_visible_snapshot_for_tool_followup() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.todo_store.create(&owner, draft("第一条")).unwrap();
    let second = service.todo_store.create(&owner, draft("第二条")).unwrap();

    let response = service.respond(message("看一下待办")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_list"));
    let text = response.text.unwrap();
    assert!(text.contains("1. 第一条"));
    assert!(text.contains("2. 第二条"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing todo snapshot");
    assert_eq!(snapshot.result_ids, vec![first.id, second.id]);
}

#[tokio::test]
async fn todo_slash_write_commands_are_tool_only_and_do_not_mutate() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service.todo_store.create(&owner, draft("待办一")).unwrap();

    for command in [
        "/todo add 买牛奶",
        "/todo done 1",
        "/todo undo 1",
        "/todo edit 1 改时间",
        "/todo delete 1",
    ] {
        let response = service.respond(message(command)).await.unwrap();
        assert!(
            response
                .text
                .unwrap()
                .contains("待办写操作已统一改为自然语言工具调用")
        );
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        assert!(session.pending_operation.is_none(), "command={command}");
    }

    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, TodoStatus::Pending);
}

#[tokio::test]
async fn todo_done_and_undo_without_arguments_still_query_completed_list() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service
        .todo_store
        .create(&owner, draft("已完成项"))
        .unwrap();
    service.todo_store.complete(&owner, &item.id).unwrap();

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
        .todo_store
        .create(&owner, draft("查找目标"))
        .unwrap();

    let all = service.respond(message("/todo all")).await.unwrap();
    assert!(all.text.unwrap().contains("查找目标"));

    let search = service.respond(message("/todo search 查找")).await.unwrap();
    assert!(search.text.unwrap().contains("查找目标"));
}

#[tokio::test]
async fn completed_time_query_still_updates_visible_snapshot() {
    let service = test_service();
    let seeded = seed_completed_time_todos(&service.todo_store);

    let response = service
        .respond(message("/todo 昨天之前完成"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_completed_search"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let query = session.last_todo_query.expect("missing completed snapshot");
    assert_eq!(query.query_type, "completed-time");
    assert!(query.result_ids.contains(&seeded.old_id));
    assert!(!query.result_ids.contains(&seeded.yesterday_id));
}
