use super::support::*;
use crate::runtime::{
    pending::PendingOperation,
    session::now_iso_cn,
    todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision},
};
use crate::service::CoreInboundKind;

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

fn save_pending(service: &crate::runtime::respond::RustRespondService, pending: PendingOperation) {
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.pending_operation = Some(pending);
    service.session_store.save(&mut session).unwrap();
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
        "查看已取消待办",
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
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());

    let confirmed = service.respond(message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("已新增待办：买牛奶"));
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);
}

#[tokio::test]
async fn todo_delete_pending_cancel_and_confirm_are_supported_for_tool_path() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.todo_store.create(&owner, draft("买牛奶")).unwrap();
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
            .todo_store
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
            item,
            created_at: now_iso_cn(),
        },
    );
    let confirmed = service.respond(message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("已取消待办"));
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 0);
}

#[tokio::test]
async fn deprecated_slash_pending_is_cleared_without_execution() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.todo_store.create(&owner, draft("旧待办")).unwrap();
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
            .todo_store
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
    let stale_item = service.todo_store.create(&owner, draft("旧待办")).unwrap();
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
    assert!(confirmed.text.unwrap().contains("已新增待办：新待办"));

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
async fn todo_delete_confirm_clears_stale_last_todo_query_snapshot() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = service.todo_store.create(&owner, draft("待取消")).unwrap();

    // 数据库 session 里已有旧的待办列表快照；软取消成功后 ops 门面会清空
    // last_todo_query，避免后续“第一条”指向已不存在的条目。
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.remember_last_todo_query(&owner.key, "list", "", vec![item.id.clone()]);
    session.pending_operation = Some(PendingOperation::TodoDelete {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner.key.clone(),
        item: item.clone(),
        created_at: now_iso_cn(),
    });
    service.session_store.save(&mut session).unwrap();

    let confirmed = service.respond(message("确认")).await.unwrap();
    assert!(confirmed.text.unwrap().contains("已取消待办"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    // 软取消成功后 last_todo_query 必须被清空，不能被数据库旧快照恢复。
    assert!(
        session.last_todo_query.is_none(),
        "expected last_todo_query cleared after soft cancel, got {:?}",
        session.last_todo_query
    );
    // 同时 last_todo_action 应指向被取消的待办，而非旧值。
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.title, "待取消");
    assert_eq!(last_action.action, "cancelled");
}
