//! 确定性 Todo 短路（Issue #361）的 Respond 集成测试。
//!
//! 覆盖：完整/恢复操作在“可见快照存在 + 编号唯一”时直接按快照真实 ID 执行，
//! 不经过 LLM；歧义、缺快照、删除（需二次确认）保持现有 Tool Loop 流程；
//! 编号到真实 todo_id 的映射不被破坏。

use qq_maid_common::{identity_context::ConversationKind, time_context::now_iso_cn};
use qq_maid_llm::provider::ToolCallingProtocol;

use crate::{
    runtime::{
        respond::RespondRequest,
        tools::todo::{
            TodoOwner, TodoStatus, TodoStore, flow::deterministic::plan_deterministic_todo,
        },
    },
    service::{VisibleEntityItem, VisibleEntitySnapshot},
};

use super::support::*;

fn private_todo_owner() -> TodoOwner {
    TodoStore::owner(Some("u1"), "private:u1")
}

#[tokio::test]
async fn deterministic_complete_uses_snapshot_real_id_without_llm_request() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "待办", 1..=3);

    // `/todo list` 写入用户刚刚看到的可见编号快照。
    service
        .respond(private_message("/todo list"))
        .await
        .unwrap();
    let snapshot = last_todo_snapshot(&service, "list");
    assert_eq!(
        snapshot.result_ids,
        items.iter().map(|item| item.id.clone()).collect::<Vec<_>>()
    );

    let response = service.respond(private_message("完成第1条")).await.unwrap();

    // 短路路径不发起任何 LLM 请求。
    assert!(
        inspector.requests().is_empty(),
        "short circuit must not call the LLM"
    );
    let text = response.text.unwrap();
    assert!(text.contains("待办 1"));
    let completed = service
        .task_store
        .get_by_id(&owner, &items[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, TodoStatus::Completed);
    // 编号 2 / 3 不受影响。
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &items[1].id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
    // 出站消息携带刷新后的可见快照，编号 -> 真实 ID 映射仍指向同一批条目。
    let snapshot = response.visible_entity_snapshot.unwrap();
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].entity_id, items[0].id);
    assert_eq!(snapshot.items[0].visible_number, 1);
}

#[tokio::test]
async fn deterministic_restore_uses_snapshot_real_id_without_llm_request() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "收尾", 1..=2);
    // 直接完成第 2 条，随后用 `/todo done` 生成“已完成”可见快照。
    service.task_store.complete(&owner, &items[1].id).unwrap();
    service
        .respond(private_message("/todo done"))
        .await
        .unwrap();

    let response = service.respond(private_message("恢复第1条")).await.unwrap();

    assert!(inspector.requests().is_empty());
    let restored = service
        .task_store
        .get_by_id(&owner, &items[1].id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, TodoStatus::Pending);
    assert!(response.text.unwrap().contains("收尾 2"));
}

#[tokio::test]
async fn ambiguous_or_missing_snapshot_keeps_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("请先查看待办列表再指定编号。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "待办", 1..=2);

    // 没有可见快照：不能确定性解析编号，必须进入现有 Tool Loop 流程。
    let response = service.respond(private_message("完成第1条")).await.unwrap();

    assert_eq!(inspector.tool_requests().len(), 1);
    assert!(response.text.unwrap().contains("请先查看待办列表"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &items[0].id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn out_of_range_number_keeps_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("没有这个编号。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "待办", 1..=2);
    service
        .respond(private_message("/todo list"))
        .await
        .unwrap();

    let response = service.respond(private_message("完成第9条")).await.unwrap();

    // 编号超出快照范围 -> 不短路，走现有流程。
    assert_eq!(inspector.tool_requests().len(), 1);
    assert!(response.text.unwrap().contains("没有这个编号"));
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &items[0].id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Pending
    );
}

#[tokio::test]
async fn delete_never_short_circuits_because_it_needs_confirmation() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("删除需要确认。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "待办", 1..=2);
    service
        .respond(private_message("/todo list"))
        .await
        .unwrap();

    let response = service.respond(private_message("删除第1条")).await.unwrap();

    // 删除必须二次确认，不能短路直接删除。
    assert_eq!(inspector.tool_requests().len(), 1);
    assert!(response.text.unwrap().contains("删除需要确认"));
    assert!(
        service
            .task_store
            .get_by_id(&owner, &items[0].id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn chinese_ordinal_and_mixed_actions_keep_tool_loop() {
    let inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("收到。")
        .with_tool_loop_reply_without_tool("收到。");
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = private_todo_owner();
    let items = create_numbered_private_todos(&service, "待办", 1..=3);
    service
        .respond(private_message("/todo list"))
        .await
        .unwrap();

    // 中文数字（第一条）与混合动作（完成1恢复2）都不具备唯一确定性，不短路。
    for (index, text) in ["完成第一条", "完成第1条并恢复第2条"].iter().enumerate() {
        let response = service.respond(private_message(text)).await.unwrap();
        assert_eq!(inspector.tool_requests().len(), index + 1, "text: {text}");
        assert!(response.text.unwrap().contains("收到"));
    }
    for item in &items {
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
}

/// 构造一个“快照有效、编号唯一”的 Channel / Unknown 请求，验证确定性短路
/// 只用统一 conversation kind 判定会话类型：Channel、Unknown 一律不短路。
fn non_private_request_with_valid_snapshot(
    kind: ConversationKind,
    channel_id: Option<&str>,
) -> RespondRequest {
    let scope_key = "guild:g1:channel:c1".to_owned();
    let owner = TodoStore::owner(Some("u1"), &scope_key);
    let mut req = RespondRequest {
        content: "完成第1条".to_owned(),
        scope_key,
        conversation_kind: kind,
        conversation_id: Some("c1".to_owned()),
        user_id: Some("u1".to_owned()),
        guild_id: Some("g1".to_owned()),
        channel_id: channel_id.map(str::to_owned),
        platform: "qq_official".to_owned(),
        ..RespondRequest::default()
    };
    req.visible_entity_snapshot = Some(VisibleEntitySnapshot {
        platform: "qq_official".to_owned(),
        account_id: None,
        scope_key: req.scope_key.clone(),
        owner_key: Some(owner.key),
        created_at: now_iso_cn(),
        items: vec![VisibleEntityItem {
            domain: "todo".to_owned(),
            entity_kind: "todo".to_owned(),
            entity_id: "todo-1".to_owned(),
            visible_number: 1,
            label: Some("待办".to_owned()),
            status: Some("pending".to_owned()),
        }],
    });
    req
}

#[test]
fn channel_and_unknown_conversation_kinds_keep_tool_loop_even_with_valid_snapshot() {
    let cases = [
        // 显式 Channel：即使快照有效也必须保持现有 Tool Loop 流程。
        (
            "explicit channel",
            non_private_request_with_valid_snapshot(ConversationKind::Channel, Some("c1")),
        ),
        // 显式 Unknown（无 group/channel/事件类型可归一）：同样不短路。
        (
            "explicit unknown",
            non_private_request_with_valid_snapshot(ConversationKind::Unknown, None),
        ),
        // Unknown + channel 元数据：统一 conversation kind 解析应归一为 Channel，
        // 不能因 group_id 为空而误判为私聊后短路。
        (
            "unknown with channel metadata",
            non_private_request_with_valid_snapshot(ConversationKind::Unknown, Some("c1")),
        ),
    ];

    for (label, req) in cases {
        let plan = plan_deterministic_todo(&req, None, &["complete_todos"]).unwrap();
        assert!(plan.is_none(), "{label} must keep the Tool Loop flow");
    }
}
