use crate::provider::ToolCallingProtocol;
use crate::runtime::todo::TodoStatus;

use super::support::*;

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
async fn todo_tools_create_cancel_restore_and_delete_use_existing_pending_boundaries() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();

    service
        .respond(private_message("帮我记待办"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let created = execute_tool_json(
        &tool_request,
        "create_todo",
        r#"{"content":"今晚检查机器人日志","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
    )
    .await;
    assert_eq!(created["ok"], true);
    assert_eq!(created["created"]["title"], "今晚检查机器人日志");
    assert!(created.get("requires_confirmation").is_none());
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);

    execute_tool_json(&tool_request, "list_todos", r#"{"status":"pending"}"#).await;
    let cancel = execute_tool_json(&tool_request, "cancel_todo", r#"{"number":1}"#).await;
    assert_eq!(cancel["ok"], true);
    assert_eq!(cancel["cancelled"][0]["visible_number"], 1);
    assert!(cancel["missing_numbers"].as_array().unwrap().is_empty());
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Cancelled
    );

    execute_tool_json(&tool_request, "list_todos", r#"{"status":"cancelled"}"#).await;
    let restore = execute_tool_json(&tool_request, "restore_todos", r#"{"numbers":[1]}"#).await;
    assert_eq!(restore["restored"][0]["visible_number"], 1);
    assert!(restore["missing_numbers"].as_array().unwrap().is_empty());
    let restored = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].cancelled_at.is_none());

    service
        .todo_store
        .complete(&owner, &restored[0].id)
        .unwrap();
    execute_tool_json(&tool_request, "list_todos", r#"{"status":"completed"}"#).await;
    let delete = execute_tool_json(&tool_request, "delete_todos", r#"{"numbers":[1]}"#).await;
    assert_eq!(delete["requires_confirmation"], true);
    assert_eq!(delete["pending_action"], "delete");
    service.respond(private_message("确认")).await.unwrap();
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
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
    let today = crate::util::time_context::request_time_context()
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
async fn deterministic_cancelled_query_then_tool_loop_restore_first_uses_latest_snapshot() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let first = create_private_todo(&service, "已取消 A");
    let second = create_private_todo(&service, "已取消 B");
    service.todo_store.cancel(&owner, &first.id).unwrap();
    service.todo_store.cancel(&owner, &second.id).unwrap();

    let listed = service
        .respond(private_message("看看已取消"))
        .await
        .unwrap();
    assert_eq!(listed.command.as_deref(), Some("todo_cancelled_list"));
    let snapshot = last_todo_snapshot(&service, "cancelled");
    let (expected_first_id, expected_first_title) =
        first_snapshot_item(&service, &owner, &snapshot, "cancelled");

    let _ = service
        .respond(private_message("恢复第一条"))
        .await
        .unwrap();
    let tool_request = newest_tool_request(&inspector, "after cancelled restore");
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
