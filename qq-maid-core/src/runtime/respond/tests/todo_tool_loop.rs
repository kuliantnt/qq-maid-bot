use serde_json::json;

use crate::runtime::respond::{RespondRequest, common::empty_respond_request};
use crate::runtime::session::SessionMeta;
use crate::runtime::tools::todo::{TodoItemDraft, TodoStatus, TodoTimePrecision};
use qq_maid_llm::provider::{ToolCallingProtocol, ToolExecutionResult};

use super::support::*;

fn stable_group_scope() -> &'static str {
    "platform:qq_official:account:app-1:group:g1"
}

fn stable_group_message(text: &str, user_id: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: stable_group_scope().to_owned(),
        user_id: Some(user_id.to_owned()),
        group_id: Some("g1".to_owned()),
        platform: "qq_official".to_owned(),
        account_id: Some("app-1".to_owned()),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
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

fn group_todo_draft(title: &str) -> TodoItemDraft {
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

#[tokio::test]
async fn private_tool_loop_registers_todo_tools_and_keeps_internal_ids_hidden() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    create_private_todo(&service, "检查机器人日志");

    service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let listed = execute_tool_json(&tool_request, "list_todos", r#"{"status":"pending"}"#).await;
    assert_eq!(listed["items"][0]["visible_number"], 1);
    assert!(listed["items"][0].get("id").is_none());

    let completed = execute_tool_json(&tool_request, "complete_todos", r#"{"numbers":[1]}"#).await;
    assert_eq!(completed["completed"][0]["title"], "检查机器人日志");
    assert!(completed["completed"][0].get("id").is_none());
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Completed
    );
    let listed_completed =
        execute_tool_json(&tool_request, "list_todos", r#"{"status":"completed"}"#).await;
    assert_eq!(listed_completed["items"][0]["visible_number"], 1);
    let restored = execute_tool_json(&tool_request, "restore_todos", r#"{"numbers":[1]}"#).await;
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["visible_number"], 1);
    assert!(restored["missing_numbers"].as_array().unwrap().is_empty());

    let session = active_private_session(&service);
    assert!(session.last_todo_query.is_none());
    let last_action = session.last_todo_action.expect("missing last_todo_action");
    assert_eq!(last_action.owner_key, owner.key);
    assert_eq!(last_action.title, "检查机器人日志");
    assert_eq!(last_action.action, "restored");
    assert_eq!(last_action.resulting_status, TodoStatus::Pending);
}

#[tokio::test]
async fn group_tool_loop_todo_visible_snapshot_uses_actor_interaction_session() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_raw_tool_results(
            vec![ToolExecutionResult {
                name: "list_todos".to_owned(),
                output: json!({"ok": true, "status": "pending"}),
                succeeded: true,
            }],
            "已列出待办",
        )
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "尝试完成第一条",
        )
        .with_tool_call_json(
            "complete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "完成第一条",
        );
    let service = test_service_with_provider_and_group_tool_calling_tools(
        inspector,
        true,
        true,
        Some(vec!["list_todos".to_owned(), "complete_todos".to_owned()]),
    );
    let owner_a = crate::runtime::tools::todo::TodoStore::owner(Some("u1"), stable_group_scope());
    let owner_b = crate::runtime::tools::todo::TodoStore::owner(Some("u2"), stable_group_scope());
    let todo_a = service
        .todo_store
        .create(&owner_a, group_todo_draft("A 的待办"))
        .unwrap();
    let todo_b = service
        .todo_store
        .create(&owner_b, group_todo_draft("B 的待办"))
        .unwrap();

    let list_response = service
        .respond(stable_group_message("检查待办", "u1"))
        .await
        .unwrap();

    let conversation = service
        .session_store
        .get_or_create_active(&stable_group_conversation_meta("u1"))
        .unwrap();
    assert_eq!(
        list_response.session_id.as_deref(),
        Some(conversation.session_id.as_str())
    );
    assert!(
        conversation
            .history
            .iter()
            .any(|message| message.role == "user" && message.content == "检查待办"),
        "群聊 Tool Loop 后 conversation session 仍应保留公开聊天历史"
    );
    assert!(
        conversation.last_todo_query.is_none(),
        "群聊 Tool Loop 的 Todo 可见快照不能写入 conversation session"
    );
    let interaction_a = service
        .session_store
        .get_or_create_active(&stable_group_interaction_meta("u1"))
        .unwrap();
    assert_eq!(
        interaction_a
            .last_todo_query
            .as_ref()
            .expect("missing user A visible snapshot")
            .result_ids,
        vec![todo_a.id.clone()]
    );
    assert!(
        service
            .session_store
            .get_active(&stable_group_interaction_meta("u2"))
            .unwrap()
            .is_none(),
        "user A 的工具快照不能提前创建 user B 的 interaction session"
    );

    service
        .respond(stable_group_message("第一条完成", "u2"))
        .await
        .unwrap();
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner_a, &todo_a.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending,
        "user B 不能沿用 user A 的可见编号完成 A 的待办"
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner_b, &todo_b.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending,
        "user B 没有自己的可见快照时也不能误完成自己的待办"
    );
    assert!(
        service
            .session_store
            .get_or_create_active(&stable_group_interaction_meta("u2"))
            .unwrap()
            .pending_operation
            .is_some(),
        "user B 缺少可见编号时应进入自己的澄清 pending"
    );

    service
        .respond(stable_group_message("第一条完成", "u1"))
        .await
        .unwrap();
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner_a, &todo_a.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed,
        "user A 仍能使用自己的 interaction session 快照继续操作"
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner_b, &todo_b.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_pending_query_then_tool_loop_complete_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let first = create_private_todo(&service, "测试代办");
    let second = create_private_todo(&service, "明天晚上搬到16栋");

    let listed = service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_list"));
    let listed_text = listed.text.unwrap();
    assert!(listed_text.contains("1. 测试代办"));
    assert!(listed_text.contains("2. 明天晚上搬到16栋"));
    assert_eq!(inspector.tool_call_count(), 0);

    let snapshot = last_todo_snapshot(&service, "todo");
    assert_eq!(snapshot.query_type, "list");
    assert_eq!(
        snapshot.result_ids,
        vec![first.id.clone(), second.id.clone()]
    );

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    assert!(inspector.tool_call_count() >= 1);
    let tool_request = newest_tool_request(&inspector, "after completing first visible todo");
    let completed = complete_first_visible_todo(&tool_request).await;
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"][0]["visible_number"], 1);
    assert_eq!(completed["completed"][0]["title"], "测试代办");

    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &second.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_date_query_then_tool_loop_complete_first_uses_date_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let today = qq_maid_common::time_context::request_time_context()
        .local_date()
        .format("%Y-%m-%d")
        .to_string();
    let today_item = create_private_todo_due_date(&service, "今天要完成", today.clone());
    let no_time = create_private_todo(&service, "无时间待办");

    let listed = service
        .respond(private_message("查看今天待办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_due_date"));
    let listed_text = listed.text.unwrap();
    assert!(listed_text.contains("1. 今天要完成"));
    assert!(!listed_text.contains("无时间待办"));

    let snapshot = last_todo_snapshot(&service, "date");
    assert_eq!(snapshot.query_type, "due-date");
    assert_eq!(snapshot.condition, today);
    assert_eq!(snapshot.result_ids, vec![today_item.id.clone()]);

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after completing first dated todo");
    let completed = complete_first_visible_todo(&tool_request).await;
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"][0]["title"], "今天要完成");

    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &today_item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &no_time.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_todo_query_alias_then_tool_loop_complete_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let first = create_private_todo(&service, "代办 A");
    create_private_todo(&service, "代办 B");

    let listed = service
        .respond(private_message("看一下代办"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_list"));
    assert!(listed.text.as_deref().unwrap().contains("1. 代办 A"));

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after alias query");
    let completed = complete_first_visible_todo(&tool_request).await;
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"][0]["title"], "代办 A");
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &first.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
}

#[tokio::test]
async fn deterministic_completed_query_then_tool_loop_restore_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let first = create_private_todo(&service, "已完成 A");
    let second = create_private_todo(&service, "已完成 B");
    service.todo_store.complete(&owner, &first.id).unwrap();
    service.todo_store.complete(&owner, &second.id).unwrap();

    let listed = service
        .respond(private_message("看看已完成"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_done"));
    let snapshot = last_todo_snapshot(&service, "completed");
    assert_eq!(snapshot.query_type, "completed-list");
    let (expected_first_id, expected_first_title) =
        first_snapshot_item(&service, &owner, &snapshot, "completed");

    let _ = service
        .respond(private_message("恢复第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after completed restore");
    let restored = execute_tool_json(
        &tool_request,
        "restore_todos",
        r#"{"numbers":[1],"reference":null}"#,
    )
    .await;
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["restored"][0]["title"], expected_first_title);
    assert_eq!(
        service
            .todo_store
            .get_by_id(&owner, &expected_first_id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn deterministic_empty_query_clears_old_snapshot_before_number_mutation() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let todo = create_private_todo(&service, "旧快照条目");

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    service.todo_store.complete(&owner, &todo.id).unwrap();

    let empty_list = service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    assert!(
        empty_list
            .text
            .as_deref()
            .unwrap()
            .contains("暂无未完成待办")
    );
    let snapshot = last_todo_snapshot(&service, "empty");
    assert!(snapshot.result_ids.is_empty());

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after empty query");
    let completed = complete_first_visible_todo(&tool_request).await;
    assert_eq!(completed["ok"], false);
    assert_eq!(completed["requires_clarification"], true);
    assert_eq!(completed["pending_action"], "clarify");
}

#[tokio::test]
async fn deterministic_query_then_status_changes_returns_precise_missing_error() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let todo = create_private_todo(&service, "状态先被改掉");

    service
        .respond(private_message("看一下待办"))
        .await
        .unwrap();
    // 模拟用户看到列表后，条目已被其他操作提前完成。
    service.todo_store.complete(&owner, &todo.id).unwrap();

    let _ = service
        .respond(private_message("完成第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after state change");
    let completed = complete_first_visible_todo(&tool_request).await;
    assert_eq!(completed["ok"], true);
    assert_eq!(completed["completed"], serde_json::json!([]));
    assert_eq!(completed["missing_numbers"], serde_json::json!([1]));
}
