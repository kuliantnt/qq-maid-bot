use crate::runtime::tools::memory::{
    MemoryDreamConfig, MemoryOperations, MemoryQuery, MemorySourceType, MemoryTarget,
};

use super::support::*;

#[tokio::test]
async fn ordinary_private_chat_schedules_dream_after_session_write() {
    let provider = MockProvider::with_dream_replies(vec![Ok(
        r#"{"memories":[{"content":"用户长期偏好简洁回答","category":"preference","attribute_key":null,"worth_saving":true}]}"#,
    )]);
    let service = test_service_with_provider_and_memory_dream(
        provider,
        MemoryDreamConfig {
            enabled: true,
            min_interval_seconds: 0,
            min_new_sessions: 1,
            max_sessions: 20,
            max_input_chars: 32_000,
            max_output_memories: 8,
        },
    );

    service
        .respond(private_message("我长期偏好简洁回答"))
        .await
        .unwrap();

    let actor = crate::runtime::tools::memory::MemoryActor {
        user_id: "u1".to_owned(),
        personal_scope_id: "u1".to_owned(),
        group_scope_id: None,
        can_manage_group_memory: false,
    };
    let mut records = Vec::new();
    for _ in 0..50 {
        records = MemoryOperations::new(service.memory_store.clone())
            .list(&actor, MemoryQuery::active(MemoryTarget::personal("u1")))
            .unwrap();
        if !records.is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].content, "用户长期偏好简洁回答");
    assert_eq!(records[0].source_type, MemorySourceType::SystemDerived);
}
