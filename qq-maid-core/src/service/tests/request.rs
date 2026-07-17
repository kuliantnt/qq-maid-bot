use super::*;

#[test]
fn private_conversation_derives_private_scope() {
    let req = CoreRequest {
        text: "hello".to_owned(),
        message_id: Some("request-1".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();

    assert_eq!(
        respond.scope_key,
        "platform:qq_official:account:app-1:private:u1"
    );
    assert_eq!(respond.platform, "qq_official");
    assert_eq!(respond.user_id.as_deref(), Some("u1"));
    assert_eq!(respond.group_id, None);
}

#[test]
fn group_conversation_derives_group_scope_without_member_split() {
    let req = CoreRequest {
        text: "/todo".to_owned(),
        message_id: Some("request-2".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: None,
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();

    assert_eq!(
        respond.scope_key,
        "platform:qq_official:account:app-1:group:g1"
    );
    assert_eq!(respond.platform, "qq_official");
    assert_eq!(respond.user_id, None);
    assert_eq!(respond.group_id.as_deref(), Some("g1"));
}

#[test]
fn message_context_is_derived_from_core_request_authoritative_fields() {
    // #319 收敛：message_context 由 CoreRequest 权威字段派生，Gateway 不再单独构造。
    use qq_maid_common::identity_context::{
        IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext,
    };

    let req = CoreRequest {
        text: "hi".to_owned(),
        message_id: Some("request-3".to_owned()),
        input_parts: Vec::new(),
        quoted: None,
        mentions: vec![MentionIdentity {
            raw_text: None,
            target: MessageActorContext {
                user_id: Some("member-2".to_owned()),
                source: IdentitySource::MemberApi,
                ..Default::default()
            },
            is_self: false,
            confidence: MentionConfidence::Event,
        }],
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("sender-1".to_owned()),
            union_id: Some("union-1".to_owned()),
            display_name: Some("昵称".to_owned()),
            group_member_role: Some(CoreGroupMemberRole::Admin),
            is_bot: false,
            identity_source: IdentitySource::MemberApi,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    };

    let respond: RespondRequest = req.into();
    let context = respond
        .message_context
        .as_ref()
        .expect("message_context should be derived");
    assert_eq!(respond.conversation_kind, ConversationKind::Group);
    assert_eq!(respond.conversation_id.as_deref(), Some("g1"));

    // actor 字段从 CoreActor 派生。
    let actor = context.actor.as_ref().expect("actor present");
    assert_eq!(actor.user_id.as_deref(), Some("sender-1"));
    assert_eq!(actor.union_id.as_deref(), Some("union-1"));
    assert_eq!(actor.display_name.as_deref(), Some("昵称"));
    assert_eq!(actor.group_member_role.as_deref(), Some("admin"));
    assert_eq!(actor.is_bot, Some(false));
    assert_eq!(actor.source, IdentitySource::MemberApi);

    // mentions 透传。
    assert_eq!(context.mentions.len(), 1);
    assert_eq!(
        context.mentions[0].target.user_id.as_deref(),
        Some("member-2")
    );

    // conversation 从 CoreConversation 派生。
    assert_eq!(context.conversation.kind, "group");
    assert_eq!(context.conversation.id.as_deref(), Some("g1"));
    assert_eq!(
        context.conversation.platform.as_deref(),
        Some("qq_official")
    );
    assert_eq!(context.conversation.account_id.as_deref(), Some("app-1"));
}

#[test]
fn safe_error_message_redacts_secret_like_detail() {
    let err = LlmError::http(
        "OpenAI chat returned HTTP 400: key sk-test-secret and bearer abc.def.ghi rejected",
    );

    let message = safe_error_message(&err);

    assert!(message.contains("HTTP 400"));
    assert!(!message.contains("sk-test-secret"));
    assert!(!message.contains("abc.def.ghi"));
}
