use std::fs;

use qq_maid_llm::provider::{ToolCallingProtocol, types::ChatRole};

use crate::runtime::{
    respond::{
        RespondRequest,
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT, empty_respond_request,
        },
    },
    tools::memory::{MemoryScopeType, MemoryTarget, MemoryVisibility},
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
    seed_recall_memory(
        &service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::ContextOnly,
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
    assert!(
        memory_prompt
            .content
            .contains("仅供理解当前发言，不得主动披露、列举或转述")
    );
}

#[tokio::test]
async fn memory_recall_matrix_isolated_by_scene_scope_and_visibility() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());

    seed_recall_memory(
        &service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::Private,
        "u1 私聊敏感记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::ContextOnly,
        "u1 允许群聊理解的个人记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::Public,
        "u1 已公开个人记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::personal("u2"),
        MemoryVisibility::ContextOnly,
        "u2 允许群聊理解的个人记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group("g-a"),
        MemoryVisibility::GroupMembers,
        "群 A 公共记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group("g-b"),
        MemoryVisibility::GroupMembers,
        "群 B 公共记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group_profile("g-a", "u1"),
        MemoryVisibility::ContextOnly,
        "群 A 用户 u1 画像",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group_profile("g-a", "u2"),
        MemoryVisibility::GroupMembers,
        "群 A 用户 u2 画像",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group_profile("g-b", "u1"),
        MemoryVisibility::GroupMembers,
        "群 B 用户 u1 画像",
    );

    service.respond(private_message("私聊矩阵")).await.unwrap();
    let private_prompt = inspector
        .requests()
        .into_iter()
        .rev()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    assert!(private_prompt.contains("u1 私聊敏感记忆"));
    assert!(private_prompt.contains("u1 允许群聊理解的个人记忆"));
    assert!(private_prompt.contains("u1 已公开个人记忆"));
    assert!(!private_prompt.contains("群 A 公共记忆"));
    assert!(!private_prompt.contains("u2 允许群聊理解的个人记忆"));

    service
        .respond(message_in_scope("群 A 用户 u1", "group:g-a", "u1", "g-a"))
        .await
        .unwrap();
    let group_a_u1_prompt = inspector
        .requests()
        .into_iter()
        .rev()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    assert!(group_a_u1_prompt.contains("群 A 公共记忆"));
    assert!(group_a_u1_prompt.contains("群 A 用户 u1 画像"));
    assert!(group_a_u1_prompt.contains("u1 允许群聊理解的个人记忆"));
    assert!(group_a_u1_prompt.contains("u1 已公开个人记忆"));
    assert!(group_a_u1_prompt.contains("当前用户个人记忆（可在当前群聊中正常引用）"));
    assert!(
        group_a_u1_prompt
            .contains("当前用户个人记忆（仅供理解当前发言，不得主动披露、列举或转述）")
    );
    assert!(
        group_a_u1_prompt.contains("当前用户在本群的画像（仅供理解，不得主动披露、列举或转述）")
    );
    assert!(!group_a_u1_prompt.contains("当前用户在本群可正常引用的画像"));
    let public_personal_start = group_a_u1_prompt
        .find("【当前用户个人记忆（可在当前群聊中正常引用）】")
        .unwrap();
    let public_personal_section = &group_a_u1_prompt[public_personal_start..]
        .split_once("\n\n")
        .unwrap()
        .0;
    assert!(!public_personal_section.contains("不得主动披露"));
    assert!(!group_a_u1_prompt.contains("u1 私聊敏感记忆"));
    assert!(!group_a_u1_prompt.contains("群 A 用户 u2 画像"));
    assert!(!group_a_u1_prompt.contains("群 B 公共记忆"));
    assert!(!group_a_u1_prompt.contains("群 B 用户 u1 画像"));

    service
        .respond(message_in_scope("群 A 用户 u2", "group:g-a", "u2", "g-a"))
        .await
        .unwrap();
    let group_a_u2_prompt = inspector
        .requests()
        .into_iter()
        .rev()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    assert!(group_a_u2_prompt.contains("群 A 公共记忆"));
    assert!(group_a_u2_prompt.contains("群 A 用户 u2 画像"));
    assert!(group_a_u2_prompt.contains("u2 允许群聊理解的个人记忆"));
    assert!(group_a_u2_prompt.contains("当前用户在本群可正常引用的画像"));
    let public_profile_start = group_a_u2_prompt
        .find("【当前用户在本群可正常引用的画像】")
        .unwrap();
    let public_profile_section = &group_a_u2_prompt[public_profile_start..]
        .split_once("\n\n")
        .unwrap()
        .0;
    assert!(!public_profile_section.contains("不得主动披露"));
    assert!(!group_a_u2_prompt.contains("群 A 用户 u1 画像"));
    assert!(!group_a_u2_prompt.contains("u1 允许群聊理解的个人记忆"));
    assert!(!group_a_u2_prompt.contains("u1 私聊敏感记忆"));

    service
        .respond(message_in_scope("群 B 用户 u1", "group:g-b", "u1", "g-b"))
        .await
        .unwrap();
    let group_b_u1_prompt = inspector
        .requests()
        .into_iter()
        .rev()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    assert!(group_b_u1_prompt.contains("群 B 公共记忆"));
    assert!(group_b_u1_prompt.contains("群 B 用户 u1 画像"));
    assert!(!group_b_u1_prompt.contains("群 A 公共记忆"));
    assert!(!group_b_u1_prompt.contains("群 A 用户 u1 画像"));
    assert!(!group_b_u1_prompt.contains("群 A 用户 u2 画像"));
}

#[tokio::test]
async fn group_memory_budget_counts_chinese_layer_text_and_truncates_long_records() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    seed_recall_memory(
        &service,
        MemoryTarget::group("g-budget"),
        MemoryVisibility::GroupMembers,
        &format!("第一条长内容：{}尾部标记", "甲".repeat(700)),
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group("g-budget"),
        MemoryVisibility::GroupMembers,
        &format!("第二条中文记录：{}", "乙".repeat(300)),
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group("g-budget"),
        MemoryVisibility::GroupMembers,
        &format!("第三条中文记录：{}", "丙".repeat(300)),
    );

    service
        .respond(message_in_scope(
            "预算边界",
            "group:g-budget",
            "u1",
            "g-budget",
        ))
        .await
        .unwrap();

    let prompt = inspector
        .requests()
        .into_iter()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    let layer_start = prompt.find("【当前群聊可正常引用的群组记忆】").unwrap();
    let layer_end = prompt[layer_start..]
        .find("\n\n群聊使用说明")
        .map(|offset| layer_start + offset)
        .unwrap();
    let layer = &prompt[layer_start..layer_end];

    assert_eq!(layer.chars().count(), 1_100);
    assert!(layer.contains("第三条中文记录"));
    assert!(layer.contains("第二条中文记录"));
    assert!(layer.contains("第一条长内容："));
    assert!(!layer.contains("尾部标记"));
}

#[tokio::test]
async fn group_memory_recall_isolated_between_bot_accounts() {
    let inspector = MockProvider::new();
    let (service, _) = test_service_with_provider_and_base(inspector.clone());
    seed_recall_memory(
        &service,
        MemoryTarget::group("platform:qq_official:account:app-a:group:g1"),
        MemoryVisibility::GroupMembers,
        "app-a 群记忆",
    );
    seed_recall_memory(
        &service,
        MemoryTarget::group("platform:qq_official:account:app-b:group:g1"),
        MemoryVisibility::GroupMembers,
        "app-b 群记忆",
    );

    let mut request = message_in_scope(
        "账号 A 群聊",
        "platform:qq_official:account:app-a:group:g1",
        "u1",
        "g1",
    );
    request.account_id = Some("app-a".to_owned());
    service.respond(request).await.unwrap();

    let prompt = inspector
        .requests()
        .into_iter()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;
    assert!(prompt.contains("app-a 群记忆"));
    assert!(!prompt.contains("app-b 群记忆"));
}

#[tokio::test]
async fn standard_chat_and_tool_loop_share_the_same_memory_context() {
    let direct_inspector = MockProvider::new();
    let (direct_service, _) = test_service_with_provider_and_base(direct_inspector.clone());
    seed_recall_memory(
        &direct_service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::Private,
        "同一份已判定的记忆",
    );
    direct_service
        .respond(private_message("普通 Chat"))
        .await
        .unwrap();
    let direct_context = direct_inspector
        .requests()
        .into_iter()
        .flat_map(|request| request.messages)
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content;

    let tool_inspector = MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_loop_reply_without_tool("工具循环完成");
    let tool_service = test_service_with_provider_and_tool_calling(tool_inspector.clone(), true);
    seed_recall_memory(
        &tool_service,
        MemoryTarget::personal("u1"),
        MemoryVisibility::Private,
        "同一份已判定的记忆",
    );
    tool_service
        .respond(private_message("杭州今天要带伞吗"))
        .await
        .unwrap();
    let tool_context = tool_inspector.tool_requests()[0]
        .chat
        .messages
        .iter()
        .find(|message| message.role == ChatRole::System && message.content.contains("本地记忆"))
        .unwrap()
        .content
        .clone();

    let normalize_context = |context: String| {
        context
            .lines()
            .map(|line| {
                line.strip_prefix("- [")
                    .and_then(|line| line.split_once("] "))
                    .map_or_else(|| line.to_owned(), |(_, content)| format!("- {content}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        normalize_context(direct_context),
        normalize_context(tool_context)
    );
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
async fn chat_memory_layers_keep_group_context_when_personal_layer_is_newer() {
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
        seed_recall_memory(
            &service,
            MemoryTarget::personal("u1"),
            MemoryVisibility::ContextOnly,
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
    assert!(!memory_prompt.content.contains("较新个人记忆 0"));
    assert!(memory_prompt.content.contains("更旧群记忆 3"));
    assert!(memory_prompt.content.contains("更旧群记忆 0"));
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
