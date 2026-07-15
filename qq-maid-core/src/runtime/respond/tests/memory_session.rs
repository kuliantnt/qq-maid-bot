use std::fs;

use qq_maid_llm::provider::types::ChatRole;

use crate::runtime::{
    respond::{
        RespondRequest,
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT, empty_respond_request,
        },
    },
    tools::memory::MemoryScopeType,
};

use super::support::*;

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
async fn streaming_chat_uses_request_account_for_personal_memory_scope() {
    let inspector = MockProvider::new().with_stream_enabled(true);
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "platform:qq_official:account:app-1:private:u1",
        "u1",
        None,
        "app 账号下的个人记忆",
    );
    seed_scoped_memory(
        &service,
        MemoryScopeType::Personal,
        "platform:qq_official:account:-:private:u1",
        "u1",
        None,
        "缺失账号时的旧错误命名空间记忆",
    );

    let response = service
        .respond_stream(
            RespondRequest {
                content: "普通聊天".to_owned(),
                scope_key: "platform:qq_official:account:app-1:private:u1".to_owned(),
                user_id: Some("u1".to_owned()),
                platform: "qq_official".to_owned(),
                account_id: Some("app-1".to_owned()),
                event_type: "FakeEvent".to_owned(),
                ..empty_respond_request()
            },
            |_| Box::pin(async { Ok(()) }),
        )
        .await
        .unwrap();

    assert!(response.metrics.stream);
    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    let memory_prompt = requests[0]
        .messages
        .iter()
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap();
    assert!(memory_prompt.content.contains("app 账号下的个人记忆"));
    assert!(
        !memory_prompt
            .content
            .contains("缺失账号时的旧错误命名空间记忆")
    );
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
async fn chat_does_not_inject_member_id_mapping_or_speaker_hint() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());

    let response = service.respond(message("我是407，继续")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    let requests = inspector.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.messages.iter().all(|message| {
                !message.content.contains("成员编号映射来自外部配置文件")
                    && !message.content.contains("本轮用户消息命中了已知成员编号")
            }))
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(
        session.history.last().map(|item| item.content.as_str()),
        Some("回复：我是407，继续")
    );
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
