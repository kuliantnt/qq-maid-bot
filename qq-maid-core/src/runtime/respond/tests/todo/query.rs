use super::*;

#[tokio::test]
async fn todo_query_writes_visible_snapshot_for_tool_followup() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.task_store.create(&owner, draft("第一条")).unwrap();
    let second = service.task_store.create(&owner, draft("第二条")).unwrap();

    let response = service.respond(message("看一下待办")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_list"));
    let text = response.text.as_deref().unwrap();
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
async fn todo_pending_list_shows_ten_and_reports_truncation_total() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let mut created_ids = Vec::new();
    for index in 1..=11 {
        let item = service
            .task_store
            .create(&owner, draft(&format!("第{index}条待办")))
            .unwrap();
        created_ids.push(item.id);
    }

    let response = service.respond(message("/todo")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_list"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("🚧 进行中 · 共 11 项"));
    assert!(text.contains("1. 第1条待办"));
    assert!(text.contains("10. 第10条待办"));
    assert!(!text.contains("第11条待办"));
    assert!(text.contains("共找到 11 条待办，当前展示前 10 条"));
    assert!(
        response
            .markdown
            .as_deref()
            .unwrap()
            .contains("共找到 11 条待办，当前展示前 10 条")
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing todo snapshot");
    assert_eq!(snapshot.result_ids, created_ids[..10].to_vec());
}

#[tokio::test]
async fn todo_list_command_rejects_conflicting_time_filters_with_help() {
    let service = test_service();

    let response = service
        .respond(message("/todo list 今天 明天"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_list_invalid"));
    let text = response.text.unwrap();
    assert!(text.contains("筛选条件无效"));
    assert!(text.contains("一次查询只能指定一个时间条件"));
    assert!(text.contains("/todo list"));
}

#[tokio::test]
async fn todo_list_overdue_pending_is_order_independent() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let yesterday = (today - Duration::days(1)).format("%Y-%m-%d").to_string();
    let tomorrow = (today + Duration::days(1)).format("%Y-%m-%d").to_string();
    let overdue = service
        .task_store
        .create(&owner, draft_due_date("逾期未完成", &yesterday))
        .unwrap();
    service
        .task_store
        .create(&owner, draft_due_date("未来未完成", &tomorrow))
        .unwrap();
    let completed = service
        .task_store
        .create(&owner, draft_due_date("逾期已完成", &yesterday))
        .unwrap();
    service.task_store.complete(&owner, &completed.id).unwrap();

    let mut replies = Vec::new();
    for arguments in ["未完成 逾期", "逾期 未完成"] {
        let response = service
            .respond(message(&format!("/todo list {arguments}")))
            .await
            .unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"));
        let text = response.text.unwrap();
        assert!(text.contains("🚧 进行中 · 共 1 项"));
        assert!(text.contains("逾期未完成"));
        assert!(!text.contains("未来未完成"));
        assert!(!text.contains("逾期已完成"));

        let snapshot = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .last_todo_query
            .expect("missing overdue snapshot");
        assert_eq!(snapshot.query_type, "search");
        assert_eq!(snapshot.condition, "未完成、逾期");
        assert_eq!(snapshot.result_ids, vec![overdue.id.clone()]);
        replies.push(text);
    }
    assert_eq!(replies[0], replies[1]);
}

#[tokio::test]
async fn todo_list_status_conflicts_are_order_independent() {
    let service = test_service();
    let cases = [
        ("已完成 逾期", "逾期 已完成", "逾期筛选只适用于未完成待办"),
        ("全部 逾期", "逾期 全部", "逾期筛选只适用于未完成待办"),
        (
            "未完成 已完成",
            "已完成 未完成",
            "一次查询只能指定一个状态条件",
        ),
        ("全部 已完成", "已完成 全部", "一次查询只能指定一个状态条件"),
        (
            "pending completed",
            "completed pending",
            "一次查询只能指定一个状态条件",
        ),
    ];

    for (forward, reverse, expected_error) in cases {
        let forward_response = service
            .respond(message(&format!("/todo list {forward}")))
            .await
            .unwrap();
        let reverse_response = service
            .respond(message(&format!("/todo list {reverse}")))
            .await
            .unwrap();
        assert_eq!(
            forward_response.command.as_deref(),
            Some("todo_list_invalid")
        );
        assert_eq!(
            reverse_response.command.as_deref(),
            Some("todo_list_invalid")
        );
        let forward_text = forward_response.text.unwrap();
        let reverse_text = reverse_response.text.unwrap();
        assert!(forward_text.contains(expected_error));
        assert_eq!(forward_text, reverse_text);
    }
}

#[tokio::test]
async fn todo_list_duplicate_status_is_deduplicated_in_condition() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let tomorrow = (qq_maid_common::time_context::request_time_context().local_date()
        + Duration::days(1))
    .format("%Y-%m-%d")
    .to_string();
    let item = service
        .task_store
        .create(&owner, draft_due_date("明天待办", &tomorrow))
        .unwrap();

    let response = service
        .respond(message("/todo list 未完成 未完成 明天"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_list"));
    assert!(response.text.unwrap().contains("明天待办"));
    let snapshot = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .last_todo_query
        .expect("missing duplicate status snapshot");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, "未完成、明天");
    assert_eq!(snapshot.condition.matches("未完成").count(), 1);
    assert_eq!(snapshot.result_ids, vec![item.id]);
}

#[tokio::test]
async fn todo_list_command_combines_time_status_and_fuzzy_keyword() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let today = qq_maid_common::time_context::request_time_context().local_date();
    let tomorrow = (today + Duration::days(1)).format("%Y-%m-%d").to_string();
    let day_after = (today + Duration::days(2)).format("%Y-%m-%d").to_string();
    service
        .task_store
        .create(&owner, draft_due_date("项目 A 报告", &tomorrow))
        .unwrap();
    service
        .task_store
        .create(&owner, draft_due_date("项目 B 报告", &tomorrow))
        .unwrap();
    service
        .task_store
        .create(&owner, draft_due_date("项目 A 后续", &day_after))
        .unwrap();

    let response = service
        .respond(message("/todo list 明天 未完成 项目 A"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("项目 A 报告"));
    assert!(!text.contains("项目 B 报告"));
    assert!(!text.contains("项目 A 后续"));
}

#[tokio::test]
async fn todo_list_command_supports_completed_no_due_and_keyword_filters() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let pending = service
        .task_store
        .create(&owner, draft("报销材料"))
        .unwrap();
    let completed = service
        .task_store
        .create(&owner, draft("报销报告"))
        .unwrap();
    service.task_store.complete(&owner, &completed.id).unwrap();

    let completed_response = service
        .respond(message("/todo list 已完成 关键词 报销"))
        .await
        .unwrap();
    let completed_text = completed_response.text.unwrap();
    assert!(completed_text.contains("报销报告"));
    assert!(!completed_text.contains("报销材料"));

    let no_due_response = service
        .respond(message("/todo list 无截止时间 报销"))
        .await
        .unwrap();
    let no_due_text = no_due_response.text.unwrap();
    assert!(no_due_text.contains("报销材料"));
    assert!(!no_due_text.contains("报销报告"));
    assert!(
        service
            .task_store
            .get_by_id(&owner, &pending.id)
            .unwrap()
            .is_some()
    );
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

    let natural_tomorrow = service
        .respond(message("帮我看看明天的待办"))
        .await
        .unwrap();
    assert_eq!(natural_tomorrow.command.as_deref(), Some("todo_due_date"));
    let natural_tomorrow_text = natural_tomorrow.text.unwrap();
    assert!(natural_tomorrow_text.contains("明天事项"));
    assert!(!natural_tomorrow_text.contains("今天日期型"));
    assert!(!natural_tomorrow_text.contains("无时间事项"));

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
