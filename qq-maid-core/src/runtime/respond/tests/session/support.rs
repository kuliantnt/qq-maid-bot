use qq_maid_llm::provider::types::ChatRequest;

use super::super::support::{MockProvider, message_in_scope};

pub(super) fn message_with_actor_context(
    text: &str,
    scope_key: &str,
    group_id: &str,
    user_id: &str,
    platform_name: &str,
) -> crate::runtime::respond::RespondRequest {
    let mut req = message_in_scope(text, scope_key, user_id, group_id);
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        current_actor_ref: None,
        actor: Some(qq_maid_common::identity_context::MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(platform_name.to_owned()),
            display_name_source: Some("event".to_owned()),
            source: qq_maid_common::identity_context::IdentitySource::Event,
            ..Default::default()
        }),
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "group".to_owned(),
            id: Some(group_id.to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    req
}

pub(super) fn guild_message_with_actor_context(
    text: &str,
    user_id: &str,
    platform_name: &str,
) -> crate::runtime::respond::RespondRequest {
    let mut req = crate::runtime::respond::RespondRequest {
        content: text.to_owned(),
        scope_key: "guild:guild-1:channel-1".to_owned(),
        conversation_kind: qq_maid_common::identity_context::ConversationKind::Channel,
        conversation_id: Some("channel-1".to_owned()),
        user_id: Some(user_id.to_owned()),
        guild_id: Some("guild-1".to_owned()),
        channel_id: Some("channel-1".to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..crate::runtime::respond::common::empty_respond_request()
    };
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        current_actor_ref: None,
        actor: Some(qq_maid_common::identity_context::MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(platform_name.to_owned()),
            display_name_source: Some("event".to_owned()),
            source: qq_maid_common::identity_context::IdentitySource::Event,
            ..Default::default()
        }),
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "channel".to_owned(),
            id: Some("channel-1".to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    req
}

pub(super) fn last_chat_request_text(inspector: &MockProvider) -> String {
    request_text(
        inspector
            .requests()
            .iter()
            .rev()
            .find(|req| req.metadata.get("purpose").map(String::as_str) != Some("session_title"))
            .expect("missing chat request"),
    )
}

pub(super) fn history_actor_ref(content: &str) -> Option<&str> {
    let (_, tail) = content.lines().next()?.split_once("actor_ref=")?;
    tail.split(['，', ']']).next()
}

pub(super) fn request_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| {
            let mut texts = vec![message.content.as_str()];
            for part in &message.content_parts {
                if let qq_maid_common::input_part::MessageInputPart::Text { text, .. } = part {
                    texts.push(text.as_str());
                }
            }
            texts
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn assert_unimplemented_rss_commands_absent(text: &str) {
    for command in ["/rss refresh", "/rss enable", "/rss disable", "/rss edit"] {
        assert!(
            !text.contains(command),
            "unimplemented RSS command leaked into help: {command}"
        );
    }
}
