use super::support::*;
use crate::runtime::{
    pending::{ClarificationCandidate, PendingOperation, PendingTodoClarification},
    session::{SessionMeta, now_iso_cn},
    todo::{TodoItem, TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision},
};
use serde_json::{Value, json};

fn draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: Some("2026-07-03 09:30:00".to_owned()),
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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

fn private_todo_meta() -> SessionMeta {
    SessionMeta::new(
        "private:u1",
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    )
}

fn private_todo_message(text: &str) -> crate::runtime::respond::RespondRequest {
    message_in_scope(text, "private:u1", "u1", "")
}

fn clarification_candidates(items: &[TodoItem]) -> Vec<ClarificationCandidate> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| ClarificationCandidate {
            id: item.id.clone(),
            display_number: index + 1,
            title: item.title.clone(),
            status: item.status.clone(),
        })
        .collect()
}

fn install_todo_clarification(
    service: &crate::runtime::respond::RustRespondService,
    tool_name: &str,
    arguments: Value,
    allow_many: bool,
    created_at: String,
    candidates: Vec<ClarificationCandidate>,
) {
    let mut session = service
        .session_store
        .get_or_create_active(&private_todo_meta())
        .unwrap();
    session.pending_operation = Some(PendingOperation::TodoClarify {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: "u1".to_owned(),
        request: PendingTodoClarification {
            tool_name: tool_name.to_owned(),
            arguments,
            allow_many,
            error_code: "todo_reference_unavailable".to_owned(),
            question: "请补充要操作哪条待办。".to_owned(),
            candidates,
            created_at: created_at.clone(),
        },
        created_at,
    });
    service.session_store.save(&mut session).unwrap();
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
async fn todo_clarification_llm_tool_call_completes_candidate_scope() {
    let provider = MockProvider::new().with_tool_call_json(
        "complete_todos",
        r#"{"numbers":[1],"reference":null}"#,
        "已完成待办：买票",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let ticket = service.todo_store.create(&owner, draft("买票")).unwrap();
    let hotel = service.todo_store.create(&owner, draft("订酒店")).unwrap();
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
            .todo_store
            .get_by_id(&owner, &ticket.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .todo_store
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
async fn todo_clarification_cancel_todo_reply_executes_without_confirmation() {
    let provider = MockProvider::new().with_tool_call_json(
        "cancel_todo",
        r#"{"number":1,"reference":null}"#,
        "已取消待办。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "cancel_todo",
        json!({"number": null, "reference": "last"}),
        false,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("取消第一条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_resumed"));
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
            .todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Cancelled
    );
}

#[tokio::test]
async fn todo_clarification_original_tool_result_wins_over_same_round_control() {
    let provider = MockProvider::new().with_tool_calls_json(
        vec![
            ("cancel_todo", r#"{"number":1,"reference":null}"#),
            (
                "clarification_control",
                r#"{"action":"abandon","question":null}"#,
            ),
        ],
        "已取消待办。",
    );
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "cancel_todo",
        json!({"number": null, "reference": "last"}),
        false,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );

    let response = service
        .respond(private_todo_message("取消第一条"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_resumed"));
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
            .todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Cancelled
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
    let first = service.todo_store.create(&owner, draft("买票")).unwrap();
    let second = service
        .todo_store
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
                .todo_store
                .get_by_id(&owner, &item.id)
                .unwrap()
                .unwrap()
                .status,
            TodoStatus::Pending
        );
    }
    assert!(matches!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation,
        Some(PendingOperation::TodoClarify { .. })
    ));
}

#[tokio::test]
async fn todo_clarification_cancel_and_expiry_do_not_mutate() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
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
            .todo_store
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
            .todo_store
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
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
    install_todo_clarification(
        &service,
        "complete_todos",
        json!({"numbers": [1], "reference": null}),
        true,
        now_iso_cn(),
        clarification_candidates(std::slice::from_ref(&item)),
    );
    service.todo_store.complete(&owner, &item.id).unwrap();

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
            .todo_store
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
        .todo_store
        .create(&owner, draft("无关列表项"))
        .unwrap();
    let candidate = service
        .todo_store
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
async fn todo_clarification_loop_error_keeps_original_pending_and_blocks_other_tools() {
    let provider = MockProvider::new().with_tool_call_json("weather", r#"{}"#, "不会返回");
    let service = test_service_with_provider(provider);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
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

    assert_eq!(response.command.as_deref(), Some("todo_clarify_loop_error"));
    assert!(matches!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation,
        Some(PendingOperation::TodoClarify { .. })
    ));
    assert_eq!(
        service
            .todo_store
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
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
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
    match session.pending_operation {
        Some(PendingOperation::TodoClarify { request, .. }) => {
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
    let item = service.todo_store.create(&owner, draft("旧任务")).unwrap();
    service.todo_store.complete(&owner, &item.id).unwrap();
    let item = service
        .todo_store
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
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation,
        Some(PendingOperation::TodoDelete { .. })
    ));
}

#[tokio::test]
async fn todo_clarification_out_of_range_number_keeps_pending_without_side_effect() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
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
            .todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert!(matches!(
        service
            .session_store
            .get_or_create_active(&private_todo_meta())
            .unwrap()
            .pending_operation,
        Some(PendingOperation::TodoClarify { .. })
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
    let item = service.todo_store.create(&owner, draft("买票")).unwrap();
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
            .todo_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
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
    service.todo_store.create(&owner, draft("待办一")).unwrap();

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
    assert!(pending_text.contains(" · 时间："));
    assert!(!pending_text.contains("无时间事项 · 时间："));
    assert!(pending_text.contains("（提醒: `"));
    assert!(pending_text.contains("9:30`"));
    assert!(pending_text.contains("   > 详情：需要保留详情"));
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
    assert!(completed_text.contains(" · 完成时间："));
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
    assert!(cancelled_text.contains(" · 取消时间："));
    assert!(cancelled_text.contains("   > 详情：取消原因记录在详情里"));
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
            reminder_at: None,
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
    assert!(text.contains("   > 详情：有详情"));
    assert!(text.contains(" · 完成时间："));
    assert!(text.contains(" · 原定时间："));
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
