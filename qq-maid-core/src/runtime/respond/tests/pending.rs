use super::support::*;
use crate::runtime::{
    pending::{ClarificationCandidate, PendingOperation, PendingTodoClarification},
    session::{SessionMeta, now_iso_cn},
    tools::{
        CompleteTodoTool,
        todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision},
    },
};
use crate::service::CoreInboundKind;
use qq_maid_llm::tool::{Tool, ToolContext};
use serde_json::json;

fn draft(title: &str) -> TodoItemDraft {
    TodoItemDraft {
        title: title.to_owned(),
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
    }
}

fn save_pending(service: &crate::runtime::respond::RustRespondService, pending: PendingOperation) {
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.pending_operation = Some(pending);
    service.session_store.save(&mut session).unwrap();
}

fn stable_group_scope() -> &'static str {
    "platform:qq_official:account:app-1:group:g1"
}

fn stable_group_interaction_meta(user_id: &str) -> SessionMeta {
    SessionMeta::new_with_account(
        format!("{}:actor:{user_id}", stable_group_scope()),
        Some(user_id.to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
        Some("app-1".to_owned()),
    )
}

fn stable_group_conversation_meta(user_id: &str) -> SessionMeta {
    SessionMeta::new_with_account(
        stable_group_scope(),
        Some(user_id.to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
        Some("app-1".to_owned()),
    )
}

fn stable_group_message(text: &str, user_id: &str) -> crate::runtime::respond::RespondRequest {
    crate::runtime::respond::RespondRequest {
        content: text.to_owned(),
        scope_key: stable_group_scope().to_owned(),
        user_id: Some(user_id.to_owned()),
        group_member_role: Some("owner".to_owned()),
        group_id: Some("g1".to_owned()),
        platform: "qq_official".to_owned(),
        account_id: Some("app-1".to_owned()),
        event_type: "FakeEvent".to_owned(),
        ..crate::runtime::respond::RespondRequest::default()
    }
}

#[test]
fn inbound_classification_keeps_plain_cancel_aggregatable_without_pending() {
    let service = test_service();

    let classification = service.classify_inbound(message("取消")).unwrap();

    assert_eq!(classification.kind, CoreInboundKind::NormalChat);
}

#[tokio::test]
async fn inbound_classification_marks_pending_input_immediate() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    save_pending(
        &service,
        PendingOperation::TodoAdd {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key,
            draft: draft("买牛奶"),
            allow_revision: false,
            created_at: now_iso_cn(),
        },
    );

    let classification = service.classify_inbound(message("取消")).unwrap();

    assert_eq!(classification.kind, CoreInboundKind::Immediate);
}

#[test]
fn inbound_classification_marks_business_commands_immediate() {
    let service = test_service();

    for input in [
        "/todo",
        "/代办",
        "/memory",
        "/查 Rust",
        "/天气杭州",
        "/翻译 hello",
    ] {
        let classification = service.classify_inbound(message(input)).unwrap();
        assert_eq!(classification.kind, CoreInboundKind::Immediate, "{input}");
    }
}

#[test]
fn inbound_classification_marks_natural_todo_queries_immediate() {
    let service = test_service();

    for input in [
        "看一下待办",
        "看一下代办",
        "查询待办",
        "查询代办",
        "查看所有待办",
        "查看已完成待办",
    ] {
        let classification = service.classify_inbound(message(input)).unwrap();
        assert_eq!(classification.kind, CoreInboundKind::Immediate, "{input}");
    }
}

#[tokio::test]
async fn todo_add_pending_confirm_and_cancel_are_supported_for_tool_path() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    save_pending(
        &service,
        PendingOperation::TodoAdd {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            draft: draft("买牛奶"),
            allow_revision: false,
            created_at: now_iso_cn(),
        },
    );

    let waiting = service.respond(message("改成买酸奶")).await.unwrap();
    assert!(waiting.text.unwrap().contains("还在等待确认"));
    assert!(service.task_store.list_pending(&owner).unwrap().is_empty());

    let confirmed = service.respond(message("确认")).await.unwrap();
    let text = confirmed.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("买牛奶"));
    assert!(!text.contains("🚧 当前进行中 · 共 1 项"));
    let todos = service.task_store.list_pending(&owner).unwrap();
    assert_eq!(todos.len(), 1);
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(
        session
            .last_todo_query
            .expect("missing refreshed todo query")
            .result_ids,
        vec![todos[0].id.clone()]
    );
}

#[tokio::test]
async fn legacy_todo_delete_pending_item_confirm_asks_to_restart_without_cancel() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.task_store.create(&owner, draft("买牛奶")).unwrap();
    save_pending(
        &service,
        PendingOperation::TodoDelete {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        },
    );

    let cancel = service.respond(message("取消")).await.unwrap();
    assert!(cancel.text.unwrap().contains("已取消，不删除待办"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );

    save_pending(
        &service,
        PendingOperation::TodoDelete {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        },
    );
    let confirmed = service.respond(message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("旧版待确认操作已失效"));
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
async fn deprecated_slash_pending_is_cleared_without_execution() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.task_store.create(&owner, draft("旧待办")).unwrap();
    save_pending(
        &service,
        PendingOperation::TodoDone {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        },
    );

    let response = service.respond(message("确认")).await.unwrap();
    assert!(response.text.unwrap().contains("旧版待办确认流程已清理"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn todo_add_confirm_keeps_fresh_last_todo_action_over_stale_db_snapshot() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    // 数据库 session 里先写入旧快照：模拟用户之前查询过待办、并新增过一条待办。
    // 确认流程会重新从数据库读取 latest，当前轮次的新值必须覆盖这些旧值，
    // 不能反过来被旧值覆盖，否则“刚才那个”会指向已被取代的旧待办。
    let stale_item = service.task_store.create(&owner, draft("旧待办")).unwrap();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.remember_last_todo_action(&owner.key, &stale_item, "created");
    session.remember_last_todo_query(&owner.key, "list", "", vec![stale_item.id.clone()]);
    session.pending_operation = Some(PendingOperation::TodoAdd {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner.key.clone(),
        draft: draft("新待办"),
        allow_revision: false,
        created_at: now_iso_cn(),
    });
    service.session_store.save(&mut session).unwrap();

    let confirmed = service.respond(message("确认")).await.unwrap();
    let text = confirmed.text.unwrap();
    assert!(text.contains("✅ 已新增待办"));
    assert!(text.contains("新待办"));
    assert!(!text.contains("🚧 当前进行中"));

    // 确认后 last_todo_action 必须指向刚新增的“新待办”；
    // 若 append_pending_response 未合并该字段，latest 里的旧值会反向覆盖。
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.title, "新待办");
    assert_eq!(last_action.action, "created");
}

#[tokio::test]
async fn todo_delete_confirm_pending_item_refreshes_snapshot_after_delete() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.task_store.create(&owner, draft("待取消")).unwrap();

    let other = service.task_store.create(&owner, draft("保留")).unwrap();

    // 新版进行中待办永久删除使用 TodoBulkDelete 保存明确 status，避免复用旧
    // TodoDelete + Pending 的软取消语义。
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.remember_last_todo_query(
        &owner.key,
        "list",
        "",
        vec![item.id.clone(), other.id.clone()],
    );
    session.pending_operation = Some(PendingOperation::TodoBulkDelete {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner.key.clone(),
        item_ids: vec![item.id.clone()],
        matched_count: 1,
        status: TodoStatus::Pending,
        summary: "待取消".to_owned(),
        source_condition: "进行中待办".to_owned(),
        created_at: now_iso_cn(),
    });
    service.session_store.save(&mut session).unwrap();

    let confirmed = service.respond(message("确认")).await.unwrap();
    let text = confirmed.text.unwrap();
    assert!(text.contains("已永久删除 1 条进行中待办"));
    assert!(!text.contains("🚧 当前进行中"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let snapshot = session
        .last_todo_query
        .as_ref()
        .expect("missing refreshed snapshot");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(snapshot.result_ids, vec![other.id.clone()]);
    assert!(session.last_todo_action.is_none());
    assert!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &other.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn todo_delete_confirm_skips_item_when_status_changed_after_pending_created() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service
        .task_store
        .create(&owner, draft("临时删除"))
        .unwrap();

    save_pending(
        &service,
        PendingOperation::TodoDelete {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        },
    );

    service.task_store.complete(&owner, &item.id).unwrap();
    let confirmed = service.respond(message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("旧版待确认操作已失效"));

    let current = service
        .task_store
        .get_by_id(&owner, &item.id)
        .unwrap()
        .unwrap();
    assert_eq!(current.status, TodoStatus::Completed);
}

#[tokio::test]
async fn todo_bulk_delete_confirm_keeps_items_whose_status_changed_after_pending_created() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let keep = service
        .task_store
        .create(&owner, draft("恢复保留"))
        .unwrap();
    let delete = service
        .task_store
        .create(&owner, draft("保持完成"))
        .unwrap();
    let ids = vec![keep.id.clone(), delete.id.clone()];
    service.task_store.complete_by_ids(&owner, &ids).unwrap();

    save_pending(
        &service,
        PendingOperation::TodoBulkDelete {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            item_ids: ids.clone(),
            matched_count: ids.len(),
            status: TodoStatus::Completed,
            summary: "恢复保留、保持完成".to_owned(),
            source_condition: "已完成待办".to_owned(),
            created_at: now_iso_cn(),
        },
    );

    service
        .task_store
        .restore_completed_by_ids(&owner, std::slice::from_ref(&keep.id))
        .unwrap();
    let confirmed = service.respond(message("确认")).await.unwrap();
    let text = confirmed.text.unwrap();
    assert!(text.contains("已永久删除 1 条已完成待办"));
    assert!(text.contains("跳过 1 条已不存在或状态已变化的待办"));

    let kept = service
        .task_store
        .get_by_id(&owner, &keep.id)
        .unwrap()
        .unwrap();
    assert_eq!(kept.status, TodoStatus::Pending);
    assert!(
        service
            .task_store
            .get_by_id(&owner, &delete.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn stable_group_todo_clarify_is_isolated_by_actor_interaction_session() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), stable_group_scope());
    let item = service.task_store.create(&owner, draft("买票")).unwrap();
    let created_at = now_iso_cn();
    let mut session = service
        .session_store
        .get_or_create_active(&stable_group_interaction_meta("u1"))
        .unwrap();
    session.pending_operation = Some(PendingOperation::TodoClarify {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner.key.clone(),
        request: PendingTodoClarification {
            tool_name: "complete_todos".to_owned(),
            arguments: json!({"numbers": null, "reference": "last"}),
            allow_many: true,
            error_code: "todo_reference_unavailable".to_owned(),
            question: "请补充要操作哪条待办。".to_owned(),
            candidates: vec![ClarificationCandidate {
                id: item.id.clone(),
                display_number: 1,
                title: item.title.clone(),
                status: item.status.clone(),
            }],
            created_at: created_at.clone(),
        },
        created_at,
    });
    service.session_store.save(&mut session).unwrap();

    let other_number = service
        .respond(stable_group_message("1", "u2"))
        .await
        .unwrap();
    assert_ne!(
        other_number.command.as_deref(),
        Some("todo_clarify_resumed")
    );
    let other_cancel = service
        .respond(stable_group_message("取消", "u2"))
        .await
        .unwrap();
    assert_ne!(other_cancel.command.as_deref(), Some("todo_clarify_cancel"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert!(
        service
            .session_store
            .get_or_create_active(&stable_group_interaction_meta("u1"))
            .unwrap()
            .pending_operation
            .is_some()
    );

    let owner_number = service
        .respond(stable_group_message("1", "u1"))
        .await
        .unwrap();
    assert_eq!(
        owner_number.command.as_deref(),
        Some("todo_clarify_resumed")
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
    assert!(
        service
            .session_store
            .get_or_create_active(&stable_group_interaction_meta("u1"))
            .unwrap()
            .pending_operation
            .is_none()
    );
}

#[tokio::test]
async fn todo_clarify_manage_recurring_reminder_number_resume_skips_next() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service
        .task_store
        .create(
            &owner,
            TodoItemDraft {
                title: "喝水".to_owned(),
                reminder_at: Some("2099-01-01 09:30".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: crate::runtime::tools::todo::TodoRecurrenceKind::Daily,
                recurrence_interval_days: 1,
                recurrence_interval: 1,
                recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
                ..draft("喝水")
            },
        )
        .unwrap();
    let created_at = now_iso_cn();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.pending_operation = Some(PendingOperation::TodoClarify {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner.key.clone(),
        request: PendingTodoClarification {
            tool_name: "manage_recurring_reminder".to_owned(),
            arguments: json!({
                "numbers": null,
                "selection_text": null,
                "reference": null,
                "action": "skip_next"
            }),
            allow_many: true,
            error_code: "todo_visible_numbers_unavailable".to_owned(),
            question: "你说的是哪一条？".to_owned(),
            candidates: vec![ClarificationCandidate {
                id: item.id.clone(),
                display_number: 1,
                title: item.title.clone(),
                status: TodoStatus::Pending,
            }],
            created_at: created_at.clone(),
        },
        created_at,
    });
    service.session_store.save(&mut session).unwrap();

    let response = service.respond(message("第一条")).await.unwrap();
    let updated = service
        .task_store
        .get_by_id(&owner, &item.id)
        .unwrap()
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_clarify_resumed"));
    assert!(response.text.unwrap().contains("已跳过重复提醒的当前周期"));
    assert_eq!(updated.status, TodoStatus::Pending);
    assert_eq!(updated.reminder_at.as_deref(), Some("2099-01-02 09:30"));
    assert!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .pending_operation
            .is_none()
    );
}

#[tokio::test]
async fn stable_group_visible_todo_snapshots_are_isolated_by_actor() {
    let service = test_service();
    let owner_u1 = TodoStore::owner(Some("u1"), stable_group_scope());
    let owner_u2 = TodoStore::owner(Some("u2"), stable_group_scope());
    let u1_item = service
        .task_store
        .create(&owner_u1, draft("u1 的待办"))
        .unwrap();
    let u2_item = service
        .task_store
        .create(&owner_u2, draft("u2 的待办"))
        .unwrap();

    service
        .respond(stable_group_message("看一下待办", "u1"))
        .await
        .unwrap();
    service
        .respond(stable_group_message("看一下待办", "u2"))
        .await
        .unwrap();

    let u1_session = service
        .session_store
        .get_or_create_active(&stable_group_interaction_meta("u1"))
        .unwrap();
    let u2_session = service
        .session_store
        .get_or_create_active(&stable_group_interaction_meta("u2"))
        .unwrap();
    assert_eq!(
        u1_session
            .last_todo_query
            .as_ref()
            .expect("missing u1 snapshot")
            .result_ids,
        vec![u1_item.id.clone()]
    );
    assert_eq!(
        u2_session
            .last_todo_query
            .as_ref()
            .expect("missing u2 snapshot")
            .result_ids,
        vec![u2_item.id.clone()]
    );

    let complete_tool = CompleteTodoTool::new(
        service.task_store.clone(),
        service.session_store.clone(),
        service.notification_store.clone(),
    );
    let output = complete_tool
        .execute(
            ToolContext {
                task_id: "stable-group-u2-complete".to_owned(),
                user_id: Some("u2".to_owned()),
                scope_id: stable_group_scope().to_owned(),
                group_member_role: None,
                tool_call_id: Some("call-u2-complete".to_owned()),
            },
            json!({"numbers": [1], "reference": null}),
        )
        .await
        .unwrap();
    assert_eq!(output.value["ok"], true);

    assert_eq!(
        service
            .task_store
            .get_by_id(&owner_u1, &u1_item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner_u2, &u2_item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}

#[tokio::test]
async fn stable_group_plain_chat_keeps_conversation_session_without_actor_split() {
    let service = test_service();

    let first = service
        .respond(stable_group_message("聊聊 Rust", "u1"))
        .await
        .unwrap();
    let second = service
        .respond(stable_group_message("继续聊所有权", "u2"))
        .await
        .unwrap();

    let conversation = service
        .session_store
        .get_active(&stable_group_conversation_meta("u1"))
        .unwrap()
        .expect("missing group conversation session");
    assert_eq!(
        first.session_id.as_deref(),
        Some(conversation.session_id.as_str())
    );
    assert_eq!(
        second.session_id.as_deref(),
        Some(conversation.session_id.as_str())
    );
    let user_messages = conversation
        .history
        .iter()
        .filter(|message| message.role == "user")
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(user_messages, vec!["聊聊 Rust", "继续聊所有权"]);
    assert!(
        service
            .session_store
            .get_active(&stable_group_interaction_meta("u1"))
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .session_store
            .get_active(&stable_group_interaction_meta("u2"))
            .unwrap()
            .is_none()
    );
}
