use super::*;

#[tokio::test]
async fn todo_single_status_lists_render_board_style_and_remember_visible_order() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
        .set_items_for_test(&owner, &status_list_items())
        .unwrap();

    let pending = service.respond(message("/todo")).await.unwrap();
    assert_eq!(pending.command.as_deref(), Some("todo_list"));
    let pending_text = pending.text.unwrap();
    assert!(pending_text.starts_with("🚧 进行中 · 共 3 项"));
    assert!(pending_text.contains("1. 明天事项\n   07-02（四）"));
    assert!(pending_text.contains("2. 后天事项\n   07-03（五） · 提醒 9:30"));
    assert!(pending_text.contains("   需要保留详情"));
    assert!(!pending_text.contains("时间："));
    assert!(!pending_text.contains("提醒时间："));
    assert!(!pending_text.contains("（提醒:"));
    assert!(!pending_text.contains("> 详情"));
    assert!(!pending_text.contains("无时间事项\n   "));
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
    assert!(completed_text.contains("1. 较新归档\n   07-01 18:00（三）"));
    assert!(!completed_text.contains("（已完成）"));
    assert_in_order(&completed_text, &["1. 较新归档", "2. 较早归档"]);
    assert_eq!(last_todo_result_ids(&service), vec!["5", "4"]);
    assert!(
        completed
            .markdown
            .unwrap()
            .starts_with("# ✅ 已完成 · 共 2 项")
    );
}

#[tokio::test]
async fn todo_single_status_lists_render_empty_notices() {
    let service = test_service();

    for (command, expected) in [
        ("/todo", "暂无未完成待办"),
        ("/todo done", "暂无已完成待办"),
    ] {
        let response = service.respond(message(command)).await.unwrap();
        let text = response.text.unwrap();
        assert_eq!(text, expected, "{command}");
        assert!(!text.starts_with("null"), "{command}");
    }
}

#[tokio::test]
async fn todo_pending_list_shows_reminder_without_due_time() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let mut item = draft("买香蕉");
    item.detail = Some("老公要买香蕉，要薰衣草味的".to_owned());
    item.reminder_at = Some("2026-07-31 18:00:00".to_owned());
    service.task_store.create(&owner, item).unwrap();

    let response = service.respond(message("/todo")).await.unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("1. 买香蕉\n   提醒 07-31 18:00（五）"));
    assert!(text.contains("   老公要买香蕉，要薰衣草味的"));
    assert!(!text.contains("时间："));
    assert!(!text.contains("提醒时间："));
    assert!(!text.contains("（提醒:"));
}

#[tokio::test]
async fn natural_and_slash_status_queries_use_same_visible_order() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .task_store
        .set_items_for_test(&owner, &status_list_items())
        .unwrap();

    for (slash, natural, expected_ids) in [
        ("/todo", "查看待办", vec!["3", "2", "1"]),
        ("/todo done", "查看已完成待办", vec!["5", "4"]),
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
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T12:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T12:00:00+08:00".to_owned(),
            completed_at: None,
        },
        TodoItem {
            id: "2".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "group:g1".to_owned(),
            title: "稍后进行中".to_owned(),
            detail: Some(
                "有详情，这是一段很长的补充说明，用来确认默认列表会截断详情，避免 QQ 卡片被长文本撑开，额外补充几句用于超过限制，尾部不应完整显示"
                    .to_owned(),
            ),
            raw_text: None,
            due_date: Some("2026-07-03".to_owned()),
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T11:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T11:00:00+08:00".to_owned(),
            completed_at: None,
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
            reminder_at: None,
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-01T10:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T10:00:00+08:00".to_owned(),
            completed_at: None,
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
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T09:00:00+08:00".to_owned(),
            completed_at: Some("2026-06-30T18:00:00+08:00".to_owned()),
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
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
                recurrence_interval: 0,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Completed,
            created_at: "2026-07-01T08:00:00+08:00".to_owned(),
            updated_at: "2026-07-01T08:00:00+08:00".to_owned(),
            completed_at: Some("2026-07-01T18:00:00+08:00".to_owned()),
        },
    ];
    service
        .task_store
        .set_items_for_test(&owner, &items)
        .unwrap();

    let response = service.respond(message("/todo all")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_all"));
    let text = response.text.unwrap();
    assert!(text.starts_with("📋 全部待办 · 共 5 项"));
    assert!(text.contains("🚧 进行中（3 项）"));
    assert!(text.contains("✅ 已完成（2 项）"));
    assert!(!text.contains("⛔ 已取消"));
    assert!(text.contains("2. 稍后进行中\n   07-03（五）\n   有详情，这是一段很长的补充说明"));
    assert!(text.contains("…"));
    assert!(!text.contains("尾部不应完整显示"));
    assert!(text.contains("4. 较新完成\n   07-01 18:00（三）"));
    assert!(!text.contains("已取消新项"));
    assert!(!text.contains("> 详情"));
    assert!(!text.contains("完成时间："));
    assert!(!text.contains("原定时间："));
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
        ],
    );

    let markdown = response.markdown.unwrap();
    assert!(markdown.starts_with("# 📋 全部待办 · 共 5 项"));
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
        ],
    );

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session.last_todo_query.expect("missing all todo snapshot");
    assert_eq!(snapshot.query_type, "all");
    assert_eq!(snapshot.result_ids, vec!["3", "2", "1", "5", "4"]);
}

#[tokio::test]
async fn completed_time_query_still_updates_visible_snapshot() {
    let service = test_service();
    let seeded = seed_completed_time_todos(&service.task_store);

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
