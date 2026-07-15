use super::support::*;
use crate::runtime::{
    respond::RespondRequest,
    tools::memory::{
        CreateMemoryRequest, CreateScopedMemoryRequest, MemoryScopeType, UpdateMemoryRequest,
    },
};

fn private_message_for(text: &str, user_id: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: format!("private:{user_id}"),
        conversation_kind: qq_maid_common::identity_context::ConversationKind::Private,
        conversation_id: Some(user_id.to_owned()),
        user_id: Some(user_id.to_owned()),
        platform: "qq_official".to_owned(),
        ..RespondRequest::default()
    }
}

fn create_personal_memory(
    service: &crate::runtime::respond::RustRespondService,
    content: &str,
) -> crate::runtime::tools::memory::MemoryRecord {
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: None,
            content: content.to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap()
}

#[tokio::test]
async fn shared_conversation_does_not_expose_or_manage_personal_memory() {
    let service = test_service();
    for (user_id, content) in [("u1", "A 的个人记忆"), ("u2", "B 的个人记忆")] {
        service
            .memory_store
            .create_scoped(CreateScopedMemoryRequest {
                scope_type: MemoryScopeType::Personal,
                scope_id: user_id.to_owned(),
                created_by_user_id: user_id.to_owned(),
                user_id: Some(user_id.to_owned()),
                group_id: Some("g1".to_owned()),
                content: content.to_owned(),
                source_text: "seed".to_owned(),
                memory_type: "note".to_owned(),
                scope: "general".to_owned(),
            })
            .unwrap();
    }

    let a_list = service
        .respond(message("/记忆"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(a_list.contains("请前往私聊"));
    assert!(!a_list.contains("A 的个人记忆"));
    assert!(!a_list.contains("B 的个人记忆"));

    for command in [
        "/memory personal list",
        "/memory personal show 1",
        "/memory personal edit 1 新内容",
        "/memory personal delete 1",
        "/memory personal clear",
    ] {
        let text = service
            .respond(message(command))
            .await
            .unwrap()
            .text
            .unwrap();
        assert!(text.contains("请前往私聊"), "命令未提示私聊：{command}");
        assert!(!text.contains("A 的个人记忆"));
        assert!(!text.contains("B 的个人记忆"));
    }

    let private_a = service
        .respond(private_message_for("/memory", "u1"))
        .await
        .unwrap()
        .text
        .unwrap();
    let private_b = service
        .respond(private_message_for("/memory", "u2"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(private_a.contains("A 的个人记忆"));
    assert!(!private_a.contains("B 的个人记忆"));
    assert!(private_b.contains("B 的个人记忆"));
    assert!(!private_b.contains("A 的个人记忆"));
}

#[tokio::test]
async fn guild_channel_without_group_scope_blocks_personal_memory_management() {
    let service = test_service();
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: None,
            content: "频道绝不能回显的个人记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let response = service
        .respond(RespondRequest {
            content: "/memory personal list".to_owned(),
            scope_key: "guild:g1:channel:c1".to_owned(),
            conversation_kind: qq_maid_common::identity_context::ConversationKind::Channel,
            conversation_id: Some("c1".to_owned()),
            user_id: Some("u1".to_owned()),
            guild_id: Some("g1".to_owned()),
            channel_id: Some("c1".to_owned()),
            platform: "qq_official".to_owned(),
            ..RespondRequest::default()
        })
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(response.contains("请前往私聊"));
    assert!(!response.contains("频道绝不能回显的个人记忆"));
}

#[tokio::test]
async fn replace_confirmation_rejects_record_drift_without_overwrite() {
    let service = test_service();
    let record = create_personal_memory(&service, "准备修改的旧内容");
    service.respond(private_message("/memory")).await.unwrap();
    service
        .respond(private_message("/memory edit 1 旧确认想写入的内容"))
        .await
        .unwrap();

    service
        .memory_store
        .update(
            &record.id,
            UpdateMemoryRequest {
                content: Some("另一条路径写入的新状态".to_owned()),
                ..UpdateMemoryRequest::default()
            },
        )
        .unwrap();

    let text = service
        .respond(private_message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("执行失败"));
    assert!(!text.contains("已纠正记忆"));
    assert_eq!(
        service.memory_store.get(&record.id).unwrap().content,
        "另一条路径写入的新状态"
    );
}

#[tokio::test]
async fn delete_confirmation_rejects_record_drift_without_deletion() {
    let service = test_service();
    let record = create_personal_memory(&service, "准备删除的旧内容");
    service.respond(private_message("/memory")).await.unwrap();
    service
        .respond(private_message("/memory delete 1"))
        .await
        .unwrap();

    service
        .memory_store
        .update(
            &record.id,
            UpdateMemoryRequest {
                content: Some("删除前被更新的新状态".to_owned()),
                ..UpdateMemoryRequest::default()
            },
        )
        .unwrap();

    let text = service
        .respond(private_message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("执行失败"));
    assert!(!text.contains("已删除这条记忆"));
    assert_eq!(
        service.memory_store.get(&record.id).unwrap().content,
        "删除前被更新的新状态"
    );
}

#[tokio::test]
async fn memory_search_filters_before_limit() {
    let service = test_service();
    create_personal_memory(&service, "第 101 条以前仍应命中的 needle-memory");
    for index in 0..101 {
        create_personal_memory(&service, &format!("较新的无关记忆 {index}"));
    }

    let text = service
        .respond(private_message("/memory search needle-memory"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("第 101 条以前仍应命中的 needle-memory"));
}
