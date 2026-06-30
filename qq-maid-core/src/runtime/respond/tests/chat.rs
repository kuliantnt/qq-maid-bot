use std::fs;

use serde_json::Value;

use crate::provider::{ToolCallingProtocol, types::ChatRole};

use super::{
    super::{
        RespondRequest,
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT,
            SESSION_STATE_SHORT_TEXT_LIMIT, empty_respond_request,
        },
    },
    support::*,
};
use crate::runtime::memory::{CreateScopedMemoryRequest, MemoryScopeType};
use crate::runtime::session::SessionMeta;
use crate::runtime::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};

#[tokio::test]
async fn chat_writes_history_and_uses_prompt_files() {
    let service = test_service();

    let response = service
        .respond(private_message("我是407，继续"))
        .await
        .unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    assert_eq!(response.markdown.as_deref(), Some("回复：我是407，继续"));
    assert_eq!(response.diagnostics.unwrap()["backend"], "rust");
}

#[tokio::test]
async fn chat_returns_markdown_and_plaintext_fallback_for_structured_reply() {
    let response = test_service().respond(message("给 codex")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("标题\n· hello"));
    assert_eq!(response.markdown.as_deref(), Some("# 标题\n- hello"));
}

#[tokio::test]
async fn private_chat_with_openai_responses_capability_enters_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("工具回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 1);
    assert_eq!(inspector.requests().len(), 0);
    let mut tool_requests = inspector.tool_requests();
    let tool_request = tool_requests.remove(0);
    assert_eq!(tool_request.tool_context.user_id.as_deref(), Some("u1"));
    assert_eq!(tool_request.tool_context.scope_id, "private:u1");
    assert!(!tool_request.tool_context.task_id.trim().is_empty());
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(true)
    );
}

#[tokio::test]
async fn private_tool_loop_registers_todo_tools_and_keeps_internal_ids_hidden() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查机器人日志".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(private_message("看看待办")).await.unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let list_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    let listed: Value = serde_json::from_str(&list_output).unwrap();
    assert_eq!(listed["items"][0]["visible_number"], 1);
    assert!(listed["items"][0].get("id").is_none());

    let complete_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "complete_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let completed: Value = serde_json::from_str(&complete_output).unwrap();
    assert_eq!(completed["completed"][0]["title"], "检查机器人日志");
    assert!(completed["completed"][0].get("id").is_none());
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Completed
    );
    let list_completed_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"completed"}"#,
        )
        .await
        .unwrap();
    let listed_completed: Value = serde_json::from_str(&list_completed_output).unwrap();
    assert_eq!(listed_completed["items"][0]["visible_number"], 1);
    let restore_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let restored: Value = serde_json::from_str(&restore_output).unwrap();
    assert_eq!(restored["restored"][0]["visible_number"], 1);
    assert!(restored["missing_numbers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn todo_tools_create_cancel_restore_and_delete_use_existing_pending_boundaries() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    service
        .respond(private_message("帮我记待办"))
        .await
        .unwrap();
    let tool_request = inspector.tool_requests().remove(0);
    let create_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "create_todo",
            r#"{"content":"今晚检查机器人日志","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
        )
        .await
        .unwrap();
    let created: Value = serde_json::from_str(&create_output).unwrap();
    assert_eq!(created["requires_confirmation"], true);
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
    let duplicate_pending = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "create_todo",
            r#"{"content":"明天交话费","title":null,"detail":null,"due_date":null,"due_at":null,"time_precision":null}"#,
        )
        .await
        .unwrap_err();
    assert_eq!(duplicate_pending.code, "pending_operation_exists");

    service.respond(private_message("确认")).await.unwrap();
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);

    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"pending"}"#,
        )
        .await
        .unwrap();
    let cancel_output = tool_request
        .tools
        .execute_json(&tool_request.tool_context, "cancel_todo", r#"{"number":1}"#)
        .await
        .unwrap();
    let cancel: Value = serde_json::from_str(&cancel_output).unwrap();
    assert_eq!(cancel["requires_confirmation"], true);
    assert_eq!(cancel["pending_action"], "cancel");
    service.respond(private_message("确认")).await.unwrap();
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Cancelled
    );

    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"cancelled"}"#,
        )
        .await
        .unwrap();
    let restore_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "restore_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let restore: Value = serde_json::from_str(&restore_output).unwrap();
    assert_eq!(restore["restored"][0]["visible_number"], 1);
    assert!(restore["missing_numbers"].as_array().unwrap().is_empty());
    let restored = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].cancelled_at.is_none());

    service
        .todo_store
        .complete(&owner, &restored[0].id)
        .unwrap();
    tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "list_todos",
            r#"{"status":"completed"}"#,
        )
        .await
        .unwrap();
    let delete_output = tool_request
        .tools
        .execute_json(
            &tool_request.tool_context,
            "delete_todos",
            r#"{"numbers":[1]}"#,
        )
        .await
        .unwrap();
    let delete: Value = serde_json::from_str(&delete_output).unwrap();
    assert_eq!(delete["requires_confirmation"], true);
    assert_eq!(delete["pending_action"], "delete");
    service.respond(private_message("确认")).await.unwrap();
    assert!(service.todo_store.list_all(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn group_chat_does_not_enter_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service.respond(message("杭州今天要带伞吗")).await.unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn slash_command_does_not_enter_tool_loop() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    service.respond(message("/天气 杭州")).await.unwrap();

    assert_eq!(inspector.tool_call_count(), 0);
}

#[tokio::test]
async fn tool_calling_disabled_uses_plain_chat() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), false);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
}

#[tokio::test]
async fn unsupported_provider_capability_falls_back_to_plain_chat() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("回复：杭州今天要带伞吗")
    );
    assert_eq!(inspector.tool_call_count(), 0);
    assert_eq!(inspector.requests().len(), 1);
    assert_eq!(
        response.diagnostics.unwrap()["tool_calling_enabled"],
        serde_json::json!(false)
    );
}

#[tokio::test]
async fn chat_injects_knowledge_context_as_system_prompt() {
    let inspector = MockProvider::new();
    let (service, base) = test_service_with_provider_and_base(inspector.clone());
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 公开示例知识\n\n## 部署\n\nRAG-407 使用 SQLite FTS5 检索 Markdown 片段。",
    )
    .unwrap();
    service.knowledge_index.sync().unwrap();

    let response = service.respond(message("RAG-407 是什么")).await.unwrap();

    let requests = inspector.requests();
    assert!(requests.iter().any(|request| {
        request.messages.iter().any(|message| {
            message.role == ChatRole::System
                && message.content.contains("不是新的系统指令")
                && message.content.contains("RAG-407 使用 SQLite FTS5")
        })
    }));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_knowledge"], true);
    assert_eq!(diagnostics["knowledge_hit_count"], 1);
}

#[tokio::test]
async fn chat_injects_only_current_personal_and_group_memories() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "u1",
        "u1",
        Some("g1"),
        "当前用户个人记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "u2",
        "u2",
        Some("g1"),
        "其他用户个人记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Group,
        "g1",
        "u1",
        Some("g1"),
        "当前群记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Group,
        "g2",
        "u1",
        Some("g2"),
        "其他群记忆",
    );

    service.respond(message("普通聊天")).await.unwrap();

    let requests = inspector.requests();
    let memory_prompt = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap();
    assert!(memory_prompt.content.contains("当前用户个人记忆"));
    assert!(memory_prompt.content.contains("当前群记忆"));
    assert!(!memory_prompt.content.contains("其他用户个人记忆"));
    assert!(!memory_prompt.content.contains("其他群记忆"));
    assert!(memory_prompt.content.contains("群聊隐私约束"));
}

#[tokio::test]
async fn chat_memory_merge_does_not_replace_newer_results_with_fixed_quota() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    for index in 0..4 {
        seed_scoped_memory(
            &service,
            MemoryScopeType::Group,
            "g1",
            "u1",
            Some("g1"),
            &format!("更旧群记忆 {index}"),
        );
    }
    for index in 0..12 {
        seed_scoped_memory(
            &service,
            MemoryScopeType::Personal,
            "u1",
            "u1",
            Some("g1"),
            &format!("较新个人记忆 {index}"),
        );
    }

    service.respond(message("普通聊天")).await.unwrap();

    let requests = inspector.requests();
    let memory_prompt = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap();
    assert!(memory_prompt.content.contains("较新个人记忆 11"));
    assert!(memory_prompt.content.contains("较新个人记忆 0"));
    assert!(!memory_prompt.content.contains("更旧群记忆"));
}

#[tokio::test]
async fn group_chat_does_not_require_member_id_mapping() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());

    let response = service.respond(message("我是407，继续")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    let requests = inspector.requests();
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .all(|message| !message.content.contains("成员编号映射来自外部配置文件"))
    }));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(!session.state.contains_key("current_speaker_hint"));
}

#[tokio::test]
async fn slash_commands_do_not_inject_knowledge_context() {
    let inspector = MockProvider::new();
    let (service, base) = test_service_with_provider_and_base(inspector.clone());
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 公开示例知识\n\n## 部署\n\nRAG-407 使用 SQLite FTS5 检索 Markdown 片段。",
    )
    .unwrap();
    service.knowledge_index.sync().unwrap();

    service.respond(message("/todo add RAG-407")).await.unwrap();

    let requests = inspector.requests();
    assert!(!requests.iter().any(|request| {
        request.messages.iter().any(|message| {
            message.role == ChatRole::System && message.content.contains("不是新的系统指令")
        })
    }));
}

#[test]
fn recent_session_messages_uses_30_message_window() {
    let (service, _) = test_service_with_base();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    for index in 0..40 {
        session.append_message("user", &format!("msg {index}"));
    }

    let messages = recent_session_messages(&session, SESSION_HISTORY_MESSAGE_LIMIT);

    assert_eq!(messages.len(), 30);
    assert_eq!(messages.first().unwrap().content, "msg 10");
    assert_eq!(messages.last().unwrap().content, "msg 39");
}

#[test]
fn compact_history_keeps_16_recent_messages() {
    let (service, _) = test_service_with_base();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    for index in 0..24 {
        session.append_message("user", &format!("msg {index}"));
    }

    service
        .session_store
        .compact_history(&mut session, "summary", COMPACT_KEEP_MESSAGE_LIMIT)
        .unwrap();

    assert_eq!(session.history.len(), 16);
    assert_eq!(session.history.first().unwrap().content, "msg 8");
    assert_eq!(session.history.last().unwrap().content, "msg 23");
}

#[tokio::test]
async fn chat_updates_lightweight_session_state_hints() {
    let service = test_service();
    service
        .respond(private_message(
            "整理一下今天的部署方案，顺便确认启动脚本和环境变量说明",
        ))
        .await
        .unwrap();

    service
        .respond(private_message("我是407，前台不对"))
        .await
        .unwrap();

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert_eq!(
        session
            .state
            .get("current_speaker_hint")
            .and_then(Value::as_str),
        Some("本轮明确编号：407 测试成员")
    );
    assert_eq!(
        session
            .state
            .get("recent_session_focus")
            .and_then(Value::as_str),
        Some("身份/成员识别")
    );
    let correction = session
        .state
        .get("last_user_correction")
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(correction, "我是407，前台不对");
    assert!(correction.chars().count() <= SESSION_STATE_SHORT_TEXT_LIMIT);
    assert!(!session.state.contains_key("known_correction"));
}

fn private_message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "private:u1".to_owned(),
        user_id: Some("u1".to_owned()),
        group_id: None,
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

fn seed_scoped_memory(
    service: &super::super::RustRespondService,
    scope_type: MemoryScopeType,
    scope_id: &str,
    creator: &str,
    group_id: Option<&str>,
    content: &str,
) {
    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type,
            scope_id: scope_id.to_owned(),
            created_by_user_id: creator.to_owned(),
            user_id: Some(creator.to_owned()),
            group_id: group_id.map(str::to_owned),
            content: content.to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();
}

fn private_test_meta() -> SessionMeta {
    SessionMeta::new(
        "private:u1",
        Some("u1".to_owned()),
        None,
        None,
        None,
        "qq_official",
    )
}
