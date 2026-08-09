use super::*;

#[tokio::test]
async fn todo_query_writes_visible_snapshot_for_tool_followup() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let first = service.task_store.create(&owner, draft("第一条")).unwrap();
    let second = service.task_store.create(&owner, draft("第二条")).unwrap();

    let response = service.respond(message("/todo")).await.unwrap();
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
async fn todo_list_command_filters_recurring_type() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let tomorrow = (qq_maid_common::time_context::request_time_context().local_date()
        + Duration::days(1))
    .format("%Y-%m-%d")
    .to_string();
    let mut recurring_draft = draft_due_date("吃药", &tomorrow);
    recurring_draft.recurrence_kind = crate::runtime::tools::todo::TodoRecurrenceKind::Daily;
    recurring_draft.recurrence_interval_days = 1;
    recurring_draft.recurrence_interval = 1;
    recurring_draft.recurrence_unit = crate::runtime::tools::todo::TodoRecurrenceUnit::Day;
    service.task_store.create(&owner, recurring_draft).unwrap();
    service.task_store.create(&owner, draft("买香薰")).unwrap();

    let recurring = service.respond(message("/todo list 周期性")).await.unwrap();
    let recurring_text = recurring.text.unwrap();
    assert_eq!(recurring.command.as_deref(), Some("todo_list"));
    assert!(recurring_text.contains("吃药"));
    assert!(!recurring_text.contains("买香薰"));

    let one_off = service.respond(message("/todo list 一次性")).await.unwrap();
    let one_off_text = one_off.text.unwrap();
    assert_eq!(one_off.command.as_deref(), Some("todo_list"));
    assert!(one_off_text.contains("买香薰"));
    assert!(!one_off_text.contains("吃药"));
}

#[tokio::test]
async fn natural_todo_queries_no_longer_hit_deterministic_shortcuts() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
        .create(&owner, draft("普通事项"))
        .unwrap();

    for input in [
        "看一下待办",
        "查看今天待办",
        "查看已完成待办",
        "查看周期性待办",
        "查看一次性待办",
        "明天有什么待办",
        "帮我看看明天的待办",
        "明天待办",
        "明天有哪些未完成待办",
    ] {
        let response = service.respond(message(input)).await.unwrap();
        assert_ne!(
            response.command.as_deref(),
            Some("todo_list"),
            "{input} should not hit deterministic list shortcut"
        );
        assert_ne!(
            response.command.as_deref(),
            Some("todo_due_date"),
            "{input} should not hit deterministic date shortcut"
        );
        assert_ne!(
            response.command.as_deref(),
            Some("todo_done"),
            "{input} should not hit deterministic completed shortcut"
        );
        assert_ne!(
            response.command.as_deref(),
            Some("todo_all"),
            "{input} should not hit deterministic all shortcut"
        );
    }
}
