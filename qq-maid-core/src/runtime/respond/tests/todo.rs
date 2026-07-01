use super::support::*;
use crate::runtime::todo::{TodoItem, TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};

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

fn assert_in_order(text: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let offset = text[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered text: {needle}"));
        cursor += offset + needle.len();
    }
}

fn status_list_items() -> Vec<TodoItem> {
    vec![
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "无时间事项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T12:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T12:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "2".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "后天事项".to_owned(),
            detail: Some("需要保留详情".to_owned()),
            raw_text: None,
            due_date: Some("2026-07-03".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T11:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T11:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "3".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "明天事项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-02".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T10:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T10:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "4".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较早归档".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T09:00:00+08:00".to_owned(),
            completed_at: Some("2026-06-30T18:00:00+08:00".to_owned()),
            cancelled_at: None,
        },
        TodoItem {
            id: "5".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较新归档".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T08:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T08:00:00+08:00".to_owned(),
            completed_at: Some("2026-07-01T18:00:00+08:00".to_owned()),
            cancelled_at: None,
        },
        TodoItem {
            id: "6".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "最近放弃".to_owned(),
            detail: Some("取消原因记录在详情里".to_owned()),
            raw_text: None,
            due_date: Some("2026-07-04".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T13:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T13:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T13:10:00+08:00".to_owned()),
        },
        TodoItem {
            id: "7".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较早放弃".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-05".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T07:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T07:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T07:10:00+08:00".to_owned()),
        },
    ]
}

fn last_todo_result_ids(service: &crate::runtime::respond::RustRespondService) -> Vec<String> {
    service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .last_todo_query
        .expect("missing todo query snapshot")
        .result_ids
}

#[tokio::test]
async fn todo_root_aliases_list_pending_items() {
    let service = test_service();

    for command in ["/todo", "/待办", "/代办", "/任务"] {
        let response = service.respond(message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"));
        let text = response.text.unwrap();
        assert!(text.contains("暂无未完成待办"));
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
async fn todo_deprecated_slash_write_prompts_preserve_visible_snapshot() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.todo_store.create(&owner, draft("第一条")).unwrap();
    let second = service.todo_store.create(&owner, draft("第二条")).unwrap();

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
async fn todo_single_status_lists_render_board_style_and_remember_visible_order() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .set_items_for_test(&owner, &status_list_items())
        .unwrap();

    let pending = service.respond(message("/todo")).await.unwrap();
    assert_eq!(pending.command.as_deref(), Some("todo_list"));
    let pending_text = pending.text.unwrap();
    assert!(pending_text.starts_with("🚧 进行中 · 共 3 项"));
    assert!(pending_text.contains("   时间："));
    assert!(pending_text.contains("   详情：需要保留详情"));
    assert!(!pending_text.contains("（未完成）"));
    assert_in_order(
        &pending_text,
        &["1. 明天事项", "2. 后天事项", "3. 无时间事项"],
    );
    assert_eq!(last_todo_result_ids(&service), vec!["3", "2", "1"]);
    assert!(
        pending
            .markdown
            .unwrap()
            .starts_with("# 🚧 进行中 · 共 3 项")
    );

    let completed = service.respond(message("/todo done")).await.unwrap();
    assert_eq!(completed.command.as_deref(), Some("todo_done"));
    let completed_text = completed.text.unwrap();
    assert!(completed_text.starts_with("✅ 已完成 · 共 2 项"));
    assert!(completed_text.contains("   完成时间："));
    assert!(!completed_text.contains("（已完成）"));
    assert_in_order(&completed_text, &["1. 较新归档", "2. 较早归档"]);
    assert_eq!(last_todo_result_ids(&service), vec!["5", "4"]);
    assert!(
        completed
            .markdown
            .unwrap()
            .starts_with("# ✅ 已完成 · 共 2 项")
    );

    let cancelled = service.respond(message("/todo 已取消")).await.unwrap();
    assert_eq!(cancelled.command.as_deref(), Some("todo_cancelled_list"));
    let cancelled_text = cancelled.text.unwrap();
    assert!(cancelled_text.starts_with("⛔ 已取消 · 共 2 项"));
    assert!(cancelled_text.contains("   取消时间："));
    assert!(cancelled_text.contains("   详情：取消原因记录在详情里"));
    assert!(!cancelled_text.contains("（已取消）"));
    assert_in_order(&cancelled_text, &["1. 最近放弃", "2. 较早放弃"]);
    assert_eq!(last_todo_result_ids(&service), vec!["6", "7"]);
    assert!(
        cancelled
            .markdown
            .unwrap()
            .starts_with("# ⛔ 已取消 · 共 2 项")
    );
}

#[tokio::test]
async fn todo_single_status_lists_render_empty_notices() {
    let service = test_service();

    for (command, expected) in [
        ("/todo", "暂无未完成待办"),
        ("/todo done", "暂无已完成待办"),
        ("/todo 已取消", "暂无已取消待办"),
    ] {
        let response = service.respond(message(command)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(text, expected, "{command}");
        assert!(!text.starts_with("null"), "{command}");
    }
}

#[tokio::test]
async fn natural_and_slash_status_queries_use_same_visible_order() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .set_items_for_test(&owner, &status_list_items())
        .unwrap();

    for (slash, natural, expected_ids) in [
        ("/todo", "查看待办", vec!["3", "2", "1"]),
        ("/todo done", "查看已完成待办", vec!["5", "4"]),
        ("/todo 已取消", "查看已取消待办", vec!["6", "7"]),
    ] {
        let expected_ids = expected_ids
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        service.respond(message(slash)).await.unwrap();
        assert_eq!(last_todo_result_ids(&service), expected_ids, "{slash}");

        service.respond(message(natural)).await.unwrap();
        assert_eq!(last_todo_result_ids(&service), expected_ids, "{natural}");
    }
}

#[tokio::test]
async fn todo_all_renders_grouped_board_and_remembers_visible_order() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let items = vec![
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "无时间进行中".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T12:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T12:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "2".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "稍后进行中".to_owned(),
            detail: Some("有详情".to_owned()),
            raw_text: None,
            due_date: Some("2026-07-03".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T11:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T11:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "3".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "更早进行中".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-02".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T10:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T10:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        },
        TodoItem {
            id: "4".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较早完成".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T09:00:00+08:00".to_owned(),
            completed_at: Some("2026-06-30T18:00:00+08:00".to_owned()),
            cancelled_at: None,
        },
        TodoItem {
            id: "5".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "较新完成".to_owned(),
            detail: None,
            raw_text: None,
            due_date: None,
            due_at: None,
            time_precision: TodoTimePrecision::None,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T08:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T08:00:00+08:00".to_owned(),
            completed_at: Some("2026-07-01T18:00:00+08:00".to_owned()),
            cancelled_at: None,
        },
        TodoItem {
            id: "6".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "已取消新项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-04".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T13:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T13:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T13:10:00+08:00".to_owned()),
        },
        TodoItem {
            id: "7".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "已取消旧项".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2026-07-05".to_owned()),
            due_at: None,
            time_precision: TodoTimePrecision::Date,
            status: TodoStatus::Cancelled,
            created_at: "2026-07-01T07:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T07:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: Some("2026-07-01T07:10:00+08:00".to_owned()),
        },
    ];
    service
        .todo_store
        .set_items_for_test(&owner, &items)
        .unwrap();

    let response = service.respond(message("/todo all")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_all"));
    let text = response.text.unwrap();
    assert!(text.starts_with("📋 全部待办 · 共 7 项"));
    assert!(text.contains("🚧 进行中（3 项）"));
    assert!(text.contains("✅ 已完成（2 项）"));
    assert!(text.contains("⛔ 已取消（2 项）"));
    assert!(text.contains("   详情：有详情"));
    assert!(text.contains("   完成时间："));
    assert!(text.contains("   原定时间："));
    assert!(!text.contains("（未完成）"));
    assert!(!text.contains("（已完成）"));
    assert!(!text.contains("（已取消）"));
    assert_in_order(
        &text,
        &[
            "🚧 进行中（3 项）",
            "1. 更早进行中",
            "2. 稍后进行中",
            "3. 无时间进行中",
            "✅ 已完成（2 项）",
            "4. 较新完成",
            "5. 较早完成",
            "⛔ 已取消（2 项）",
            "6. 已取消新项",
            "7. 已取消旧项",
        ],
    );

    let markdown = response.markdown.unwrap();
    assert!(markdown.starts_with("# 📋 全部待办 · 共 7 项"));
    assert_in_order(
        &markdown,
        &[
            "## 🚧 进行中（3 项）",
            "1. **更早进行中**",
            "2. **稍后进行中**",
            "3. **无时间进行中**",
            "## ✅ 已完成（2 项）",
            "4. **较新完成**",
            "5. **较早完成**",
            "## ⛔ 已取消（2 项）",
            "6. **已取消新项**",
            "7. **已取消旧项**",
        ],
    );

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing all todo snapshot");
    assert_eq!(snapshot.query_type, "all");
    assert_eq!(snapshot.result_ids, vec!["3", "2", "1", "5", "4", "6", "7"]);
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
