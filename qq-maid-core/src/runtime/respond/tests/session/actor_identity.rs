use super::super::support::*;
use super::support::{
    guild_message_with_actor_context, history_actor_ref, message_with_actor_context, request_text,
};
use crate::runtime::session::SessionMeta;

#[tokio::test]
async fn group_members_with_same_display_name_still_use_distinct_actor_refs() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    for (user_id, text) in [("u1", "A 的消息"), ("u2", "B 的消息")] {
        service
            .respond(message_with_actor_context(
                text,
                "group:g1",
                "g1",
                user_id,
                "同名成员",
            ))
            .await
            .unwrap();
    }

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing B chat request");
    let actor_a = request
        .messages
        .iter()
        .find(|message| message.content.contains("A 的消息"))
        .and_then(|message| history_actor_ref(&message.content))
        .expect("A actor_ref should be present");
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let actor_b = session.history[2]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("B actor_ref should be present");

    assert_ne!(actor_a, actor_b);
    let current_context = request
        .messages
        .last()
        .and_then(|message| message.content_parts.first())
        .expect("current MessageContext should be present")
        .fallback_text();
    assert!(current_context.contains("昵称=同名成员"));
    assert!(current_context.contains(&format!("current_actor_ref={actor_b}")));
}

#[tokio::test]
async fn compacted_group_summary_keeps_actor_ownership_for_next_member() {
    let provider = MockProvider::new();
    let inspector = provider.clone();
    let service = test_service_with_provider(provider.clone());

    service
        .respond(message_with_actor_context(
            "/set 昵称 初墨",
            "group:g1",
            "g1",
            "u1",
            "平台A",
        ))
        .await
        .unwrap();
    let actor_a = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .history[0]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.clone())
        .expect("A actor_ref should be persisted");
    let summary = format!(
        "当前话题：身份确认\n公共内容：无\n成员事实：\n- actor_ref={actor_a}：展示名为初墨\n待处理事项：无\n回复偏好：无"
    );
    provider.push_compact_reply(summary.clone());

    service
        .respond(message_with_actor_context(
            "/compact", "group:g1", "g1", "u1", "平台A",
        ))
        .await
        .unwrap();
    let compacted = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(compacted.summary, summary);
    assert!(
        compacted
            .summary
            .contains(&format!("actor_ref={actor_a}：展示名为初墨"))
    );

    service
        .respond(message_with_actor_context(
            "我是谁？",
            "group:g1",
            "g1",
            "u2",
            "平台B",
        ))
        .await
        .unwrap();
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let actor_b = session
        .history
        .iter()
        .rev()
        .find(|message| message.role == "user" && message.content == "我是谁？")
        .and_then(|message| message.turn_actor.as_ref())
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("B actor_ref should be persisted");
    assert_ne!(actor_a, actor_b);

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing B chat request");
    let request_text = request_text(&request);
    assert!(request_text.contains(&format!("actor_ref={actor_a}：展示名为初墨")));
    assert!(request_text.contains(&format!("current_actor_ref={actor_b}")));
    assert!(request_text.contains("actor_ref 不同或 unknown 时，不得把对应事实归给当前发言人"));
}

#[tokio::test]
async fn guild_channel_members_use_distinct_actor_refs_and_current_mapping() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(guild_message_with_actor_context(
            "A 的频道消息",
            "u1",
            "同名成员",
        ))
        .await
        .unwrap();
    service
        .respond(guild_message_with_actor_context(
            "B 的频道消息",
            "u2",
            "同名成员",
        ))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing guild B chat request");
    let actor_a = request
        .messages
        .iter()
        .find(|message| message.content.contains("A 的频道消息"))
        .and_then(|message| history_actor_ref(&message.content))
        .expect("guild A actor_ref should be present");
    let meta = SessionMeta::new(
        "guild:guild-1:channel-1",
        Some("u2".to_owned()),
        None,
        Some("guild-1".to_owned()),
        Some("channel-1".to_owned()),
        "qq_official",
    );
    let session = service.session_store.get_or_create_active(&meta).unwrap();
    let actor_b = session.history[2]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("guild B actor_ref should be present");

    assert_ne!(actor_a, actor_b);
    let request_text = request_text(&request);
    assert!(request_text.contains("[历史发言人：actor_ref="));
    assert!(request_text.contains(&format!("current_actor_ref={actor_b}")));
}

#[tokio::test]
async fn private_chat_does_not_inject_actor_refs_or_history_labels() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(private_message("第一条私聊"))
        .await
        .unwrap();
    service
        .respond(private_message("第二条私聊"))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing private chat request");
    let request_text = request_text(&request);
    assert!(request_text.contains("第一条私聊"));
    assert!(!request_text.contains("current_actor_ref="));
    assert!(!request_text.contains("[历史发言人："));
    assert!(!request_text.contains("[机器人当时回复给："));
}

#[tokio::test]
async fn group_history_keeps_set_command_owned_by_a_when_b_asks_identity() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(message_with_actor_context(
            "/set 昵称 初墨",
            "group:g1",
            "g1",
            "u1",
            "平台A",
        ))
        .await
        .unwrap();
    service
        .respond(message_with_actor_context(
            "我是谁？",
            "group:g1",
            "g1",
            "u2",
            "平台B",
        ))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing B chat request");
    let set_user = request
        .messages
        .iter()
        .find(|message| message.content.contains("/set 昵称 初墨"))
        .expect("set command should be present in shared group history");
    let set_reply = request
        .messages
        .iter()
        .find(|message| message.content.contains("当前展示名：初墨"))
        .expect("set reply should be present in shared group history");
    let actor_a = history_actor_ref(&set_user.content).expect("A actor_ref should be present");

    assert!(set_user.content.starts_with("[历史发言人：actor_ref="));
    assert!(set_user.content.contains("展示名=初墨"));
    assert!(set_user.content.contains("展示名来源=manual"));
    assert!(
        set_reply
            .content
            .starts_with("[机器人当时回复给：actor_ref=")
    );
    assert_eq!(history_actor_ref(&set_reply.content), Some(actor_a));
    assert!(!actor_a.contains("u1"));

    let current = request.messages.last().expect("missing current B message");
    let current_context = current
        .content_parts
        .first()
        .expect("current B message should contain MessageContext")
        .fallback_text();
    assert!(current_context.contains("稳定ID=u2"));
    assert!(current_context.contains("昵称=平台B"));
    assert!(current_context.contains("不得根据历史里最近出现的 /set 命令"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let actor_b = session.history[2]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("B actor_ref should be persisted");
    assert_ne!(actor_a, actor_b);
    assert!(current_context.contains(&format!("current_actor_ref={actor_b}")));
    assert!(current_context.contains(
        "只有历史 actor_ref（包括压缩摘要中的成员事实）与 current_actor_ref 相同，才能把历史昵称、偏好、身份声明和操作归给当前发言人"
    ));
    assert!(current_context.contains("不得通过昵称相同推断为同一人"));
    assert!(current_context.contains("不得在最终回复中主动向用户展示"));
    assert_eq!(
        session.history[2].turn_actor, session.history[3].turn_actor,
        "B 的 user / assistant 消息必须共享同一个 turn actor"
    );
}

#[tokio::test]
async fn consecutive_group_chat_turns_keep_distinct_actor_refs() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(message_with_actor_context(
            "A 的普通消息",
            "group:g1",
            "g1",
            "u1",
            "成员A",
        ))
        .await
        .unwrap();
    service
        .respond(message_with_actor_context(
            "B 的普通消息",
            "group:g1",
            "g1",
            "u2",
            "成员B",
        ))
        .await
        .unwrap();
    service
        .respond(message_with_actor_context(
            "继续", "group:g1", "g1", "u1", "成员A",
        ))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing latest chat request");
    let a_user = request
        .messages
        .iter()
        .find(|message| message.content.contains("A 的普通消息"))
        .unwrap();
    let b_user = request
        .messages
        .iter()
        .find(|message| message.content.contains("B 的普通消息"))
        .unwrap();
    let actor_a = history_actor_ref(&a_user.content).unwrap();
    let actor_b = history_actor_ref(&b_user.content).unwrap();

    assert_ne!(actor_a, actor_b);
    assert!(a_user.content.contains("展示名=成员A"));
    assert!(b_user.content.contains("展示名=成员B"));

    let a_reply = request
        .messages
        .iter()
        .find(|message| message.content.contains("回复：A 的普通消息"))
        .unwrap();
    let b_reply = request
        .messages
        .iter()
        .find(|message| message.content.contains("回复：B 的普通消息"))
        .unwrap();
    assert_eq!(history_actor_ref(&a_reply.content), Some(actor_a));
    assert_eq!(history_actor_ref(&b_reply.content), Some(actor_b));
}
