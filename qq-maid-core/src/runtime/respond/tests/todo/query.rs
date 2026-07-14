use super::*;

#[tokio::test]
async fn todo_query_writes_visible_snapshot_for_tool_followup() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.task_store.create(&owner, draft("第一条")).unwrap();
    let second = service.task_store.create(&owner, draft("第二条")).unwrap();

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
async fn todo_pending_list_collapses_after_five_items_and_full_query_restores_all() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let mut created_ids = Vec::new();
    for index in 1..=6 {
        let item = service
            .task_store
            .create(&owner, draft(&format!("第{index}条待办")))
            .unwrap();
        created_ids.push(item.id);
    }

    let response = service.respond(message("/todo")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_list"));
    let text = response.text.unwrap();
    assert!(text.contains("🚧 进行中 · 共 6 项"));
    assert!(text.contains("1. 第1条待办"));
    assert!(text.contains("5. 第5条待办"));
    assert!(!text.contains("第6条待办"));
    assert!(text.contains("还有 1 项进行中待办，可说“查看全部进行中待办”。"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing todo snapshot");
    assert_eq!(snapshot.result_ids, created_ids[..5].to_vec());

    let full = service
        .respond(message("查看全部进行中待办"))
        .await
        .unwrap();
    assert_eq!(full.command.as_deref(), Some("todo_list"));
    let full_text = full.text.unwrap();
    assert!(full_text.contains("🚧 进行中 · 共 6 项"));
    assert!(full_text.contains("6. 第6条待办"));
    assert!(!full_text.contains("还有 1 项进行中待办"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing full todo snapshot");
    assert_eq!(snapshot.result_ids, created_ids);
}

#[tokio::test]
async fn natural_todo_date_query_filters_pending_by_local_due_date() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let tomorrow = today + Duration::days(1);
    let explicit = today + Duration::days(2);
    let today_text = today.format("%Y-%m-%d").to_string();
    let tomorrow_text = tomorrow.format("%Y-%m-%d").to_string();
    let explicit_text = explicit.format("%Y-%m-%d").to_string();

    let today_date = service
        .task_store
        .create(&owner, draft_due_date("今天日期型", &today_text))
        .unwrap();
    let today_datetime = service
        .task_store
        .create(
            &owner,
            draft_due_at("今天带时间", &format!("{today_text} 09:30:00")),
        )
        .unwrap();
    service
        .task_store
        .create(&owner, draft_due_date("明天事项", &tomorrow_text))
        .unwrap();
    service
        .task_store
        .create(&owner, draft_due_date("明确日期事项", &explicit_text))
        .unwrap();
    service
        .task_store
        .create(&owner, draft("无时间事项"))
        .unwrap();

    let response = service.respond(message("查看今天待办")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.unwrap();
    assert!(text.contains("今天日期型"));
    assert!(text.contains("今天带时间"));
    assert!(!text.contains("明天事项"));
    assert!(!text.contains("无时间事项"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing due date snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, today_text);
    assert_eq!(snapshot.result_ids, vec![today_date.id, today_datetime.id]);

    let tomorrow_response = service.respond(message("明天有什么待办")).await.unwrap();
    assert_eq!(tomorrow_response.command.as_deref(), Some("todo_due_date"));
    let tomorrow_text_reply = tomorrow_response.text.unwrap();
    assert!(tomorrow_text_reply.contains("明天事项"));
    assert!(!tomorrow_text_reply.contains("今天日期型"));

    let standard_chat_response = service.respond(message("明天要做什么")).await.unwrap();
    assert_ne!(
        standard_chat_response.command.as_deref(),
        Some("todo_due_date")
    );

    let short_response = service.respond(message("明天待办")).await.unwrap();
    assert_eq!(short_response.command.as_deref(), Some("todo_due_date"));
    let short_text_reply = short_response.text.unwrap();
    assert!(short_text_reply.contains("明天事项"));
    assert!(!short_text_reply.contains("今天日期型"));

    let explicit_response = service
        .respond(message(&format!(
            "查看 {} 的待办",
            explicit.format("%-m月%-d日")
        )))
        .await
        .unwrap();
    assert_eq!(explicit_response.command.as_deref(), Some("todo_due_date"));
    let explicit_reply = explicit_response.text.unwrap();
    assert!(explicit_reply.contains("明确日期事项"));
    assert!(!explicit_reply.contains("无时间事项"));
}

#[tokio::test]
async fn todo_date_query_empty_result_does_not_fallback_to_pending_list() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
        .create(&owner, draft("无时间事项"))
        .unwrap();

    let response = service.respond(message("查看明天待办")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.unwrap();
    assert!(text.contains("这一天暂无未完成待办"));
    assert!(!text.contains("无时间事项"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing empty snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert!(snapshot.result_ids.is_empty());
}

#[tokio::test]
async fn natural_todo_date_query_allows_negated_completed_marker() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let tomorrow = today + Duration::days(1);
    let today_text = today.format("%Y-%m-%d").to_string();
    let tomorrow_text = tomorrow.format("%Y-%m-%d").to_string();

    service
        .task_store
        .create(&owner, draft_due_date("今天事项", &today_text))
        .unwrap();
    let tomorrow_item = service
        .task_store
        .create(&owner, draft_due_date("明天事项", &tomorrow_text))
        .unwrap();
    service
        .task_store
        .create(&owner, draft("无时间事项"))
        .unwrap();

    let response = service
        .respond(message("明天有哪些未完成待办"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_due_date"));
    let text = response.text.unwrap();
    assert!(text.contains("明天事项"));
    assert!(!text.contains("今天事项"));
    assert!(!text.contains("无时间事项"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing due date snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, tomorrow_text);
    assert_eq!(snapshot.result_ids, vec![tomorrow_item.id]);
}
