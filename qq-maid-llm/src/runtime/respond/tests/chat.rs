use std::fs;

use serde_json::Value;

use crate::provider::types::ChatRole;

use super::{
    super::{
        chat_flow::recent_session_messages,
        common::{
            COMPACT_KEEP_MESSAGE_LIMIT, SESSION_HISTORY_MESSAGE_LIMIT,
            SESSION_STATE_SHORT_TEXT_LIMIT,
        },
    },
    support::*,
};

#[tokio::test]
async fn chat_writes_history_and_uses_prompt_files() {
    let service = test_service();

    let response = service.respond(message("我是407，继续")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：我是407"));
    assert_eq!(response.diagnostics.unwrap()["backend"], "rust");
}

#[tokio::test]
async fn chat_injects_world_file_as_system_prompt() {
    let inspector = MockProvider::new();
    let (mut service, base) = test_service_with_provider_and_base(inspector.clone());
    let world_file = base.join("world.md");
    fs::write(&world_file, "聊天链路世界观").unwrap();
    service.prompt_config = service
        .prompt_config
        .clone()
        .with_world_file(Some(world_file));

    service.respond(message("你好")).await.unwrap();

    let requests = inspector.requests();
    assert!(requests.iter().any(|request| {
        request
            .messages
            .iter()
            .any(|message| message.role == ChatRole::System && message.content == "聊天链路世界观")
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
        .respond(message(
            "整理一下今天的部署方案，顺便确认启动脚本和环境变量说明",
        ))
        .await
        .unwrap();

    service.respond(message("我是407，前台不对")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
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
