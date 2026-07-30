use super::*;
use qq_maid_common::identity_context::IdentitySource;
use qq_maid_common::input_part::{MessageMedia, QuotedMessageContext};
use qq_maid_core::service::{VisibleEntityItem, VisibleEntitySnapshot};

mod capacity;
mod quote_payload;

fn test_snapshot(entity_id: &str) -> VisibleEntitySnapshot {
    VisibleEntitySnapshot {
        platform: "qq_official".to_owned(),
        account_id: Some("app".to_owned()),
        scope_key: "private:u1".to_owned(),
        owner_key: Some("private:u1".to_owned()),
        created_at: "2026-07-06T10:00:00+08:00".to_owned(),
        items: vec![VisibleEntityItem {
            domain: "todo".to_owned(),
            entity_kind: "todo".to_owned(),
            entity_id: entity_id.to_owned(),
            visible_number: 1,
            label: None,
            status: Some("list".to_owned()),
        }],
    }
}

fn inbound(message_id: &str, msg_idx: Option<&str>, text: &str) -> InboundMessage {
    InboundMessage {
        platform: super::super::platform::Platform::QqOfficial,
        account_id: Some("app".to_owned()),
        conversation: ConversationTarget::Private {
            target_id: "user-1".to_owned(),
        },
        actor: super::super::platform::Actor {
            sender_id: Some("user-1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: qq_maid_common::identity_context::IdentitySource::Event,
        },
        message_id: message_id.to_owned(),
        current_msg_idx: msg_idx.map(str::to_owned),
        timestamp: None,
        text: text.to_owned(),
        input_parts: vec![MessageInputPart::text(text.to_owned())],
        attachments: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        mentioned_bot: false,
        visible_entity_snapshot: None,
    }
}

fn group_inbound(message_id: &str, msg_idx: Option<&str>, text: &str) -> InboundMessage {
    InboundMessage {
        conversation: ConversationTarget::Group {
            target_id: "group-1".to_owned(),
        },
        actor: super::super::platform::Actor {
            sender_id: Some("member-1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: qq_maid_common::identity_context::IdentitySource::Event,
        },
        ..inbound(message_id, msg_idx, text)
    }
}

fn group_inbound_from(
    message_id: &str,
    msg_idx: Option<&str>,
    text: &str,
    sender_id: &str,
    is_bot: bool,
) -> InboundMessage {
    let mut message = group_inbound(message_id, msg_idx, text);
    message.actor.sender_id = Some(sender_id.to_owned());
    message.actor.is_bot = is_bot;
    message
}

fn onebot_inbound(
    account_id: &str,
    conversation: ConversationTarget,
    message_id: &str,
    text: &str,
) -> InboundMessage {
    InboundMessage {
        platform: super::super::platform::Platform::OneBot11,
        account_id: Some(account_id.to_owned()),
        conversation,
        message_id: message_id.to_owned(),
        current_msg_idx: None,
        ..inbound(message_id, None, text)
    }
}

fn onebot_quote(
    account_id: &str,
    conversation: ConversationTarget,
    current_message_id: &str,
    reference_id: &str,
) -> InboundMessage {
    let mut current = onebot_inbound(account_id, conversation, current_message_id, "继续");
    current.quoted = Some(QuotedMessageContext {
        current_message_id: Some(current_message_id.to_owned()),
        reference_id: Some(reference_id.to_owned()),
        ..Default::default()
    });
    current
}

fn quoted_group_lookup(store: &mut RefIndex, ref_id: &str) -> QuotedMessageContext {
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "查看引用");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some(ref_id.to_owned()),
        ..Default::default()
    });
    store.enrich_inbound(&mut current);
    current.quoted.unwrap()
}

#[test]
fn index_isolated_by_peer_and_fills_quote_context() {
    let mut store = RefIndex::default();
    store.insert_inbound(&inbound("m1", Some("REFIDX_1"), "上一条"));
    let mut current = inbound("m2", Some("REFIDX_2"), "继续");
    current.quoted = Some(QuotedMessageContext {
        current_message_id: Some("m2".to_owned()),
        current_msg_idx: Some("REFIDX_2".to_owned()),
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("上一条"));
}

#[test]
fn inbound_message_id_does_not_become_ref_index_key() {
    let mut store = RefIndex::default();
    store.insert_inbound(&inbound("m1", None, "上一条"));
    let mut current = inbound("m2", None, "继续");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("m1".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(!quoted.lookup_found);
    assert_eq!(quoted.fallback_reason.as_deref(), Some("ref_index_miss"));
}

#[test]
fn onebot_ref_index_uses_message_id_and_restores_sender_and_media() {
    let conversation = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    let mut original = onebot_inbound("bot-1", conversation.clone(), "12345", "看这张图");
    original.actor.display_name = Some("测试用户".to_owned());
    original
        .input_parts
        .push(MessageInputPart::image(MessageMedia {
            filename: Some("photo.png".to_owned()),
            url: Some("https://example.test/photo.png".to_owned()),
            platform: Some("onebot11".to_owned()),
            ..Default::default()
        }));
    let mut store = RefIndex::default();
    store.insert_inbound(&original);
    let mut current = onebot_quote("bot-1", conversation, "12346", "12345");

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.reference_id.as_deref(), Some("12345"));
    assert_eq!(quoted.ref_msg_idx, None);
    assert_eq!(quoted.text_summary.as_deref(), Some("看这张图"));
    assert_eq!(quoted.from_bot, Some(false));
    assert_eq!(
        quoted
            .sender
            .as_ref()
            .and_then(|sender| sender.user_id.as_deref()),
        Some("user-1")
    );
    assert_eq!(
        quoted
            .sender
            .as_ref()
            .and_then(|sender| sender.display_name.as_deref()),
        Some("测试用户")
    );
    assert!(matches!(
        quoted.input_parts[1],
        MessageInputPart::Image { .. }
    ));
    assert_eq!(quoted.media_summaries.len(), 1);
}

#[test]
fn onebot_ref_index_outbound_restores_snapshot_and_isolates_scope() {
    let private = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    let snapshot = test_snapshot("todo-1");
    let mut store = RefIndex::default();
    store.insert_bot_outbound(
        super::super::platform::Platform::OneBot11,
        Some("bot-1"),
        &private,
        Some("90001".to_owned()),
        "待办列表",
        Some(snapshot.clone()),
    );

    let mut hit = onebot_quote("bot-1", private.clone(), "next-1", "90001");
    store.enrich_inbound(&mut hit);
    let quoted = hit.quoted.unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.from_bot, Some(true));
    assert_eq!(
        quoted.sender.as_ref().and_then(|sender| sender.is_bot),
        Some(true)
    );
    assert_eq!(hit.visible_entity_snapshot, Some(snapshot));

    let isolated = [
        onebot_quote("bot-2", private.clone(), "next-2", "90001"),
        onebot_quote(
            "bot-1",
            ConversationTarget::Private {
                target_id: "user-2".to_owned(),
            },
            "next-3",
            "90001",
        ),
        onebot_quote(
            "bot-1",
            ConversationTarget::Group {
                target_id: "user-1".to_owned(),
            },
            "next-4",
            "90001",
        ),
    ];
    for mut current in isolated {
        store.enrich_inbound(&mut current);
        assert!(!current.quoted.unwrap().lookup_found);
        assert_eq!(current.visible_entity_snapshot, None);
    }
}

#[test]
fn onebot_ref_index_miss_uses_payload_or_safe_restart_fallback() {
    let conversation = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    let mut payload = onebot_quote("bot-1", conversation.clone(), "next-1", "old-1");
    payload.quoted.as_mut().unwrap().text_summary = Some("事件正文".to_owned());
    payload.quoted.as_mut().unwrap().input_parts = vec![MessageInputPart::text("事件正文")];
    let mut restarted_store = RefIndex::default();

    restarted_store.enrich_inbound(&mut payload);

    let quoted = payload.quoted.unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("事件正文"));
    assert_eq!(quoted.fallback_reason.as_deref(), Some("quoted_payload"));

    let mut missing = onebot_quote("bot-1", conversation, "next-2", "old-2");
    restarted_store.enrich_inbound(&mut missing);
    let quoted = missing.quoted.unwrap();
    assert!(!quoted.lookup_found);
    assert_eq!(quoted.fallback_reason.as_deref(), Some("ref_index_miss"));
}

#[test]
fn quoted_reference_id_without_ref_msg_idx_does_not_lookup() {
    let mut store = RefIndex::default();
    store.insert_inbound(&inbound("m1", Some("REFIDX_1"), "上一条"));
    let mut current = inbound("m2", None, "继续");
    current.quoted = Some(QuotedMessageContext {
        reference_id: Some("REFIDX_1".to_owned()),
        ref_msg_idx: None,
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(!quoted.lookup_found);
    assert_eq!(
        quoted.fallback_reason.as_deref(),
        Some("missing_reference_id")
    );
}

#[test]
fn image_quote_keeps_media_summary_and_part() {
    let mut message = inbound("m1", Some("REFIDX_1"), "看图");
    message
        .input_parts
        .push(MessageInputPart::image(MessageMedia {
            mime_type: Some("image/png".to_owned()),
            filename: Some("a.png".to_owned()),
            url: Some("https://example.test/a.png".to_owned()),
            ..Default::default()
        }));
    let mut store = RefIndex::default();
    store.insert_inbound(&message);
    let mut current = inbound("m2", None, "这张怎么处理");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.media_summaries.len(), 1);
    assert!(matches!(
        quoted.input_parts[1],
        MessageInputPart::Image { .. }
    ));
}

#[test]
fn index_drops_data_url_and_keeps_only_lightweight_media_reference() {
    let mut message = inbound("m1", Some("REFIDX_1"), "看图");
    message
        .input_parts
        .push(MessageInputPart::image(MessageMedia {
            mime_type: Some("image/png".to_owned()),
            filename: Some("a.png".to_owned()),
            url: Some("data:image/png;base64,AAAA".to_owned()),
            local_path: Some("/tmp/qq-maid/a.png".to_owned()),
            ..Default::default()
        }));
    let mut store = RefIndex::default();
    store.insert_inbound(&message);
    let mut current = inbound("m2", None, "继续");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    let MessageInputPart::Image { media } = &quoted.input_parts[1] else {
        panic!("expected image part");
    };
    assert_eq!(media.url, None);
    assert_eq!(media.local_path.as_deref(), Some("/tmp/qq-maid/a.png"));
    assert_eq!(quoted.media_summaries[0].media.as_ref().unwrap().url, None);
}

#[test]
fn missing_quote_records_fallback_reason() {
    let mut store = RefIndex::default();
    let mut current = inbound("m2", None, "继续");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_missing".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.unwrap();
    assert!(!quoted.lookup_found);
    assert_eq!(quoted.fallback_reason.as_deref(), Some("ref_index_miss"));
}

#[test]
fn missing_quote_uses_current_payload_fallback_when_available() {
    let mut store = RefIndex::default();
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "查看这条");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_missing".to_owned()),
        text_summary: Some("payload 原文".to_owned()),
        input_parts: vec![MessageInputPart::text("payload 原文")],
        lookup_found: true,
        fallback_reason: Some("pending_ref_index_lookup".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("payload 原文"));
    assert_eq!(quoted.from_bot, None);
    assert_eq!(quoted.fallback_reason.as_deref(), Some("quoted_payload"));
}

#[test]
fn outbound_and_inbound_lookup_share_app_id_for_private_and_group() {
    let mut store = RefIndex::default();
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &ConversationTarget::Private {
            target_id: "user-1".to_owned(),
        },
        Some("bot-private-1".to_owned()),
        "私聊回复",
        None,
    );
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &ConversationTarget::Group {
            target_id: "group-1".to_owned(),
        },
        Some("bot-group-1".to_owned()),
        "群聊回复",
        None,
    );

    let mut private_current = inbound("m2", None, "继续");
    private_current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("bot-private-1".to_owned()),
        ..Default::default()
    });
    let mut group_current = group_inbound("gm2", None, "继续");
    group_current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("bot-group-1".to_owned()),
        ..Default::default()
    });
    let mut missing_account = inbound("m3", None, "继续");
    missing_account.account_id = None;
    missing_account.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("bot-private-1".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut private_current);
    store.enrich_inbound(&mut group_current);
    store.enrich_inbound(&mut missing_account);

    assert!(private_current.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(
        private_current
            .quoted
            .as_ref()
            .unwrap()
            .text_summary
            .as_deref(),
        Some("私聊回复")
    );
    assert!(group_current.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(
        group_current
            .quoted
            .as_ref()
            .unwrap()
            .text_summary
            .as_deref(),
        Some("群聊回复")
    );
    assert!(!missing_account.quoted.as_ref().unwrap().lookup_found);
}

#[test]
fn bot_outbound_visible_entity_snapshot_binds_to_refidx_not_latest_message() {
    let mut store = RefIndex::default();
    let conversation = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &conversation,
        Some("REFIDX_A".to_owned()),
        "列表 A",
        Some(test_snapshot("todo-a-1")),
    );
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &conversation,
        Some("REFIDX_B".to_owned()),
        "列表 B",
        Some(test_snapshot("todo-b-1")),
    );

    let mut quoted_a = inbound("current", None, "1删除");
    quoted_a.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_A".to_owned()),
        ..Default::default()
    });
    store.enrich_inbound(&mut quoted_a);

    assert!(quoted_a.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(
        quoted_a.visible_entity_snapshot.as_ref().unwrap().items[0].entity_id,
        "todo-a-1"
    );
}

#[test]
fn qq_group_quote_bot_outbound_by_refidx_hits_after_account_normalization() {
    let mut store = RefIndex::default();
    let conversation = ConversationTarget::Group {
        target_id: "group-1".to_owned(),
    };
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &conversation,
        Some("REFIDX_bot_group_reply".to_owned()),
        "机器人上一条群回复",
        None,
    );

    let mut current = group_inbound("gm2", Some("REFIDX_current"), "继续解释");
    current.account_id = Some("app".to_owned());
    current.quoted = Some(QuotedMessageContext {
        current_message_id: Some("gm2".to_owned()),
        current_msg_idx: Some("REFIDX_current".to_owned()),
        reference_id: Some("REFIDX_bot_group_reply".to_owned()),
        ref_msg_idx: Some("REFIDX_bot_group_reply".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("机器人上一条群回复"));
    assert_eq!(quoted.from_bot, Some(true));
    // bot 出站消息回填的 sender 应标注 is_bot=true。
    let sender = quoted.sender.as_ref().unwrap();
    assert_eq!(sender.is_bot, Some(true));
    assert_eq!(sender.source, IdentitySource::Event);
}

#[test]
fn qq_group_quote_user_message_by_refidx_hits_and_marks_user() {
    let mut store = RefIndex::default();
    store.insert_inbound(&group_inbound(
        "gm-user",
        Some("REFIDX_user_text"),
        "用户原文",
    ));
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "这句话什么意思");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_user_text".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("用户原文"));
    assert_eq!(quoted.from_bot, Some(false));
    assert!(quoted.fallback_text().contains("from=user"));
    // 用户入站消息回填的 sender 应携带稳定 ID 与 is_bot=false。
    let sender = quoted.sender.as_ref().unwrap();
    assert_eq!(sender.is_bot, Some(false));
    assert_eq!(sender.user_id.as_deref(), Some("member-1"));
    assert_eq!(sender.source, IdentitySource::Event);
    assert!(quoted.fallback_text().contains("引用发送者"));
}

#[test]
fn qq_group_mixed_media_quote_by_refidx_keeps_image_part() {
    let mut message = group_inbound("gm-image", Some("REFIDX_group_image"), "看图");
    message
        .input_parts
        .push(MessageInputPart::image(MessageMedia {
            mime_type: Some("image/jpeg".to_owned()),
            filename: Some("group.jpg".to_owned()),
            url: Some("https://example.test/group.jpg".to_owned()),
            ..Default::default()
        }));
    let mut store = RefIndex::default();
    store.insert_inbound(&message);
    let mut current = group_inbound("gm-current", Some("REFIDX_current"), "这张图呢");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_group_image".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut current);

    let quoted = current.quoted.as_ref().unwrap();
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("看图"));
    assert_eq!(quoted.media_summaries.len(), 1);
    assert!(matches!(
        quoted.input_parts[1],
        MessageInputPart::Image { .. }
    ));
}

#[test]
fn qq_group_ref_index_cross_quotes_exact_ref_id_without_latest_overwrite() {
    let mut message_a = group_inbound_from("gm-a", Some("REFIDX_A"), "内容 A", "member-a", false);
    let mut message_b = group_inbound_from("gm-b", Some("REFIDX_B"), "内容 B", "member-bot", true);
    message_b
        .input_parts
        .push(MessageInputPart::image(MessageMedia {
            mime_type: Some("image/png".to_owned()),
            filename: Some("b.png".to_owned()),
            url: Some("https://example.test/b.png".to_owned()),
            ..Default::default()
        }));

    let mut store = RefIndex::default();
    store.insert_inbound(&message_a);
    store.insert_inbound(&message_b);

    let quoted_a_first = quoted_group_lookup(&mut store, "REFIDX_A");
    let quoted_b = quoted_group_lookup(&mut store, "REFIDX_B");
    let quoted_a_again = quoted_group_lookup(&mut store, "REFIDX_A");

    assert!(quoted_a_first.lookup_found);
    assert_eq!(quoted_a_first.text_summary.as_deref(), Some("内容 A"));
    assert_eq!(quoted_a_first.from_bot, Some(false));
    assert!(quoted_a_first.media_summaries.is_empty());
    assert_eq!(quoted_a_first.input_parts.len(), 1);
    assert_eq!(quoted_a_first.input_parts[0].text_content(), Some("内容 A"));

    assert!(quoted_b.lookup_found);
    assert_eq!(quoted_b.text_summary.as_deref(), Some("内容 B"));
    assert_eq!(quoted_b.from_bot, Some(true));
    assert_eq!(quoted_b.media_summaries.len(), 1);
    assert!(matches!(
        quoted_b.input_parts[1],
        MessageInputPart::Image { .. }
    ));

    assert!(quoted_a_again.lookup_found);
    assert_eq!(quoted_a_again.text_summary.as_deref(), Some("内容 A"));
    assert_eq!(quoted_a_again.from_bot, Some(false));
    assert!(quoted_a_again.media_summaries.is_empty());
    assert_eq!(quoted_a_again.input_parts.len(), 1);

    message_a.actor.sender_id = Some("member-a-updated".to_owned());
    store.insert_inbound(&message_a);
    let quoted_b_after_a_update = quoted_group_lookup(&mut store, "REFIDX_B");
    assert_eq!(
        quoted_b_after_a_update.text_summary.as_deref(),
        Some("内容 B")
    );
    assert_eq!(quoted_b_after_a_update.from_bot, Some(true));
    assert_eq!(quoted_b_after_a_update.media_summaries.len(), 1);
}

#[test]
fn evicts_oldest_entries_after_capacity_limit() {
    let mut store = RefIndex::default();
    let conversation = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    for index in 0..=MAX_REF_ENTRIES {
        store.insert_bot_outbound(
            super::super::platform::Platform::QqOfficial,
            Some("app"),
            &conversation,
            Some(format!("bot-{index}")),
            &format!("回复 {index}"),
            None,
        );
    }

    assert!(store.entries.len() <= MAX_REF_ENTRIES);

    let mut oldest = inbound("m-oldest", None, "继续");
    oldest.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("bot-0".to_owned()),
        ..Default::default()
    });
    let latest_ref = format!("bot-{MAX_REF_ENTRIES}");
    let latest_text = format!("回复 {MAX_REF_ENTRIES}");
    let mut latest = inbound("m-latest", None, "继续");
    latest.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some(latest_ref),
        ..Default::default()
    });

    store.enrich_inbound(&mut oldest);
    store.enrich_inbound(&mut latest);

    assert!(!oldest.quoted.as_ref().unwrap().lookup_found);
    assert!(latest.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(
        latest.quoted.as_ref().unwrap().text_summary.as_deref(),
        Some(latest_text.as_str())
    );
}

#[test]
fn evicts_oldest_entries_by_global_capacity_without_scope_limit() {
    let mut store = RefIndex::new(Duration::from_secs(60), 2, 10);
    store.insert_inbound(&inbound("m1", Some("REFIDX_1"), "内容 1"));
    store.insert_inbound(&inbound("m2", Some("REFIDX_2"), "内容 2"));
    store.insert_inbound(&inbound("m3", Some("REFIDX_3"), "内容 3"));

    let mut oldest = inbound("m-oldest", None, "继续");
    oldest.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        ..Default::default()
    });
    let mut second = inbound("m-second", None, "继续");
    second.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_2".to_owned()),
        ..Default::default()
    });
    let mut latest = inbound("m-latest", None, "继续");
    latest.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_3".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut oldest);
    store.enrich_inbound(&mut second);
    store.enrich_inbound(&mut latest);

    assert!(!oldest.quoted.as_ref().unwrap().lookup_found);
    assert!(second.quoted.as_ref().unwrap().lookup_found);
    assert!(latest.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(store.capacity_evictions, 1);
    assert_eq!(store.scope_evictions, 0);
}

#[test]
fn repeated_key_update_refreshes_order_and_entry() {
    let mut store = RefIndex::new(Duration::from_secs(60), 2, 10);
    store.insert_inbound(&inbound("m1", Some("REFIDX_A"), "旧内容 A"));
    store.insert_inbound(&inbound("m2", Some("REFIDX_B"), "内容 B"));
    store.insert_inbound(&inbound("m3", Some("REFIDX_A"), "新内容 A"));
    store.insert_inbound(&inbound("m4", Some("REFIDX_C"), "内容 C"));

    let mut refreshed = inbound("m-refreshed", None, "继续");
    refreshed.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_A".to_owned()),
        ..Default::default()
    });
    let mut evicted = inbound("m-evicted", None, "继续");
    evicted.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_B".to_owned()),
        ..Default::default()
    });
    let mut latest = inbound("m-latest", None, "继续");
    latest.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_C".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut refreshed);
    store.enrich_inbound(&mut evicted);
    store.enrich_inbound(&mut latest);

    assert!(refreshed.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(
        refreshed.quoted.as_ref().unwrap().text_summary.as_deref(),
        Some("新内容 A")
    );
    assert!(!evicted.quoted.as_ref().unwrap().lookup_found);
    assert!(latest.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(store.capacity_evictions, 1);
}

#[test]
fn entries_expire_after_ttl() {
    let mut store = RefIndex::new(Duration::ZERO, 10, 10);
    store.insert_inbound(&inbound("m1", Some("REFIDX_1"), "上一条"));

    let mut current = inbound("m2", Some("REFIDX_2"), "继续");
    current.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("REFIDX_1".to_owned()),
        ..Default::default()
    });
    store.enrich_inbound(&mut current);

    assert!(!current.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(store.entries.len(), 0);
    assert_eq!(store.expired_evictions, 1);
}

#[test]
fn evicts_oldest_entries_by_scope_without_touching_other_scopes() {
    let mut store = RefIndex::new(Duration::from_secs(60), 10, 2);
    let private_conversation = ConversationTarget::Private {
        target_id: "user-1".to_owned(),
    };
    let group_conversation = ConversationTarget::Group {
        target_id: "group-1".to_owned(),
    };

    for index in 0..3 {
        store.insert_bot_outbound(
            super::super::platform::Platform::QqOfficial,
            Some("app"),
            &private_conversation,
            Some(format!("private-{index}")),
            &format!("私聊回复 {index}"),
            None,
        );
    }
    store.insert_bot_outbound(
        super::super::platform::Platform::QqOfficial,
        Some("app"),
        &group_conversation,
        Some("group-0".to_owned()),
        "群聊回复",
        None,
    );

    let mut oldest_private = inbound("m-oldest", None, "继续");
    oldest_private.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("private-0".to_owned()),
        ..Default::default()
    });
    let mut latest_private = inbound("m-latest", None, "继续");
    latest_private.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("private-2".to_owned()),
        ..Default::default()
    });
    let mut latest_group = group_inbound("g-latest", None, "继续");
    latest_group.quoted = Some(QuotedMessageContext {
        ref_msg_idx: Some("group-0".to_owned()),
        ..Default::default()
    });

    store.enrich_inbound(&mut oldest_private);
    store.enrich_inbound(&mut latest_private);
    store.enrich_inbound(&mut latest_group);

    assert!(!oldest_private.quoted.as_ref().unwrap().lookup_found);
    assert!(latest_private.quoted.as_ref().unwrap().lookup_found);
    assert!(latest_group.quoted.as_ref().unwrap().lookup_found);
    assert_eq!(store.scope_evictions, 1);
}
