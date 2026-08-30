use qq_maid_llm::provider::{ToolCallingProtocol, types::ChatRole};
use serde_json::json;

use super::support::*;
use crate::runtime::{
    pending::PreparedAction,
    session::{SessionMeta, now_iso_cn},
    tools::{
        memory::MemoryPendingPayload,
        todo::{
            ClarificationCandidate, PendingTodoClarification, TodoItem, TodoPendingPayload,
            TodoStatus, TodoStore,
        },
    },
};

const UNKNOWN_COMMAND_REPLY: &str = "未知命令，发送 `/help` 查看可用命令。";

fn assert_provider_unused(provider: &MockProvider) {
    assert_eq!(provider.tool_call_count(), 0);
    assert!(provider.tool_requests().is_empty());
    assert!(provider.requests().is_empty());
}

fn save_todo_pending(
    service: &crate::runtime::respond::RustRespondService,
    meta: &SessionMeta,
    payload: TodoPendingPayload,
) -> PreparedAction {
    let mut session = service.session_store.get_or_create_active(meta).unwrap();
    let pending = payload.into_prepared_action(&session.scope_key);
    session.pending_operation = Some(pending.clone());
    service.session_store.save(&mut session).unwrap();
    pending
}

fn save_memory_pending(
    service: &crate::runtime::respond::RustRespondService,
    meta: &SessionMeta,
    payload: MemoryPendingPayload,
) -> PreparedAction {
    let mut session = service.session_store.get_or_create_active(meta).unwrap();
    let pending = payload.into_prepared_action(&session.scope_key);
    session.pending_operation = Some(pending.clone());
    service.session_store.save(&mut session).unwrap();
    pending
}

fn completed_todo(
    service: &crate::runtime::respond::RustRespondService,
    owner: &crate::runtime::tools::todo::TodoOwner,
    title: &str,
) -> TodoItem {
    let item = service.task_store.create(owner, todo_draft(title)).unwrap();
    service.task_store.complete(owner, &item.id).unwrap();
    service
        .task_store
        .get_by_id(owner, &item.id)
        .unwrap()
        .unwrap()
}

fn todo_delete_pending(item: TodoItem, owner_key: &str) -> TodoPendingPayload {
    TodoPendingPayload::TodoDelete {
        initiator_user_id: Some("u1".to_owned()),
        owner_key: owner_key.to_owned(),
        item,
        created_at: now_iso_cn(),
    }
}

fn assert_private_pending_unchanged(
    service: &crate::runtime::respond::RustRespondService,
    expected: &PreparedAction,
) {
    let current = service
        .session_store
        .get_active(&private_test_meta())
        .unwrap()
        .unwrap()
        .pending_operation;
    assert_eq!(current.as_ref(), Some(expected));
}

#[tokio::test]
async fn unknown_slash_does_not_consume_todo_delete_confirmation() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "保留的已完成待办");
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        todo_delete_pending(item.clone(), &owner.key),
    );

    let response = service.respond(private_message("/unknown")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some(UNKNOWN_COMMAND_REPLY));
    assert_eq!(response.command.as_deref(), Some("unknown_command"));
    assert_private_pending_unchanged(&service, &pending);
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn roll_dm_bypasses_pending_without_entering_tool_loop() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "保留的已完成待办");
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        todo_delete_pending(item.clone(), &owner.key),
    );

    let response = service
        .respond(private_message("/roll 晚上要不要出门"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .is_some_and(|text| text.starts_with("AI DM 暂时无法判断本次检定难度"))
    );
    assert_eq!(response.command.as_deref(), Some("roll"));
    assert_private_pending_unchanged(&service, &pending);
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(provider.tool_call_count(), 0);
    assert!(provider.tool_requests().is_empty());
}

#[tokio::test]
async fn local_roll_expression_preserves_pending_without_model_or_tool() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "保留的已完成待办");
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        todo_delete_pending(item.clone(), &owner.key),
    );

    let response = service.respond(private_message("/roll 2d6")).await.unwrap();

    assert!(
        response
            .text
            .as_deref()
            .is_some_and(|text| text.starts_with("🎲 2d6："))
    );
    assert_eq!(response.command.as_deref(), Some("roll"));
    assert_private_pending_unchanged(&service, &pending);
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn iching_is_a_deterministic_command_outside_pending_and_tool_loop() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);

    for alias in ["起卦", "算卦", "卜卦"] {
        let response = service
            .respond(private_message(&format!("/{alias}")))
            .await
            .unwrap();

        assert_eq!(response.command.as_deref(), Some("iching"));
        let text = response.text.as_deref().unwrap();
        assert!(text.starts_with("🎴 周易起卦\n\n"));
        assert!(text.contains("本卦："));
        assert!(text.contains("【卦辞】"));
    }
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn iching_receipt_is_available_to_followup_without_recasting() {
    let provider = MockProvider::new();
    let service = test_service_with_provider(provider.clone());

    let cast = service.respond(private_message("/算卦")).await.unwrap();
    let cast_text = cast.text.clone().unwrap();

    assert_eq!(cast.command.as_deref(), Some("iching"));
    assert!(cast.session_id.is_some());
    assert!(provider.requests().is_empty());

    let followup = service
        .respond(private_message("解释一下上一卦"))
        .await
        .unwrap();

    assert_eq!(followup.text.as_deref(), Some("回复：解释一下上一卦"));
    assert_eq!(followup.session_id, cast.session_id);
    assert_eq!(provider.tool_call_count(), 0);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| { message.role == ChatRole::Assistant && message.content == cast_text })
    );

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert_eq!(
        session
            .history
            .iter()
            .filter(|message| message.role == "assistant" && message.content == cast_text)
            .count(),
        1
    );
}

#[tokio::test]
async fn codex_easter_egg_does_not_consume_todo_delete_confirmation() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "保留的已完成待办");
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        todo_delete_pending(item.clone(), &owner.key),
    );

    let response = service.respond(private_message("/status")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("状态：还能继续写。大概。"));
    assert_eq!(response.command.as_deref(), Some("codex_easter_egg"));
    assert_private_pending_unchanged(&service, &pending);
    assert_eq!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn unknown_slash_does_not_resume_todo_clarification_tool_loop() {
    let provider = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(
            "delete_todos",
            r#"{"numbers":[1],"reference":null}"#,
            "不应执行删除",
        );
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "澄清中的已完成待办");
    let created_at = now_iso_cn();
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        TodoPendingPayload::TodoClarify {
            initiator_user_id: Some("u1".to_owned()),
            owner_key: owner.key.clone(),
            request: PendingTodoClarification {
                tool_name: "delete_todos".to_owned(),
                arguments: json!({"numbers": null, "reference": "last"}),
                allow_many: false,
                error_code: "todo_reference_unavailable".to_owned(),
                question: "请补充要删除哪条待办。".to_owned(),
                candidates: vec![ClarificationCandidate {
                    id: item.id.clone(),
                    display_number: 1,
                    title: item.title.clone(),
                    status: item.status.clone(),
                }],
                created_at: created_at.clone(),
            },
            created_at,
        },
    );

    let response = service.respond(private_message("/unknown")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some(UNKNOWN_COMMAND_REPLY));
    assert_eq!(response.command.as_deref(), Some("unknown_command"));
    assert_private_pending_unchanged(&service, &pending);
    assert!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .is_some()
    );
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn compact_unknown_memory_slash_does_not_consume_memory_pending() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let created_at = now_iso_cn();
    let pending = save_memory_pending(
        &service,
        &private_test_meta(),
        MemoryPendingPayload::ClarifyScope {
            initiator_user_id: "u1".to_owned(),
            owner_key: "u1".to_owned(),
            normalized_content: "保留这条待澄清记忆".to_owned(),
            source_text: "记住这条内容".to_owned(),
            source_ref: None,
            created_at,
        },
    );

    let response = service
        .respond(private_message("/记忆查看1"))
        .await
        .unwrap();

    assert_eq!(response.text.as_deref(), Some(UNKNOWN_COMMAND_REPLY));
    assert_eq!(response.command.as_deref(), Some("unknown_command"));
    assert_private_pending_unchanged(&service, &pending);
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn unaddressed_group_unknown_slash_is_silent_without_consuming_pending() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let meta = test_meta();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let item = completed_todo(&service, &owner, "群内保留的已完成待办");
    let pending = save_todo_pending(
        &service,
        &meta,
        todo_delete_pending(item.clone(), &owner.key),
    );

    let response = service.respond(message("/unknown")).await.unwrap();

    assert!(response.text.is_none());
    assert_eq!(response.diagnostics.as_ref().unwrap()["suppressed"], true);
    assert_eq!(
        response.diagnostics.as_ref().unwrap()["reason"],
        "unknown_group_slash_command"
    );
    let current = service
        .session_store
        .get_active(&meta)
        .unwrap()
        .unwrap()
        .pending_operation;
    assert_eq!(current.as_ref(), Some(&pending));
    assert!(
        service
            .task_store
            .get_by_id(&owner, &item.id)
            .unwrap()
            .is_some()
    );
    assert_provider_unused(&provider);
}

#[tokio::test]
async fn registered_todo_command_keeps_existing_pending_priority() {
    let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(provider.clone(), true);
    let owner = private_todo_owner();
    let item = completed_todo(&service, &owner, "仍等待确认的已完成待办");
    let pending = save_todo_pending(
        &service,
        &private_test_meta(),
        todo_delete_pending(item, &owner.key),
    );

    let response = service.respond(private_message("/todo")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_delete"));
    assert_ne!(response.command.as_deref(), Some("todo_list"));
    assert_private_pending_unchanged(&service, &pending);
    assert_provider_unused(&provider);
}
