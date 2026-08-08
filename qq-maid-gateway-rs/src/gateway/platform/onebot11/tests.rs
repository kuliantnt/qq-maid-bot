use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::gateway::{
    dedupe::MessageDedupe,
    platform::{core_scope_key, render_text_for_core, to_core_request},
};

fn event(value: Value) -> OneBotEvent {
    serde_json::from_value(value).unwrap()
}

fn message(outcome: OneBotInboundOutcome) -> InboundMessage {
    let OneBotInboundOutcome::Message(message) = outcome else {
        panic!("expected adapted message, got {outcome:?}");
    };
    *message
}

fn ignored(outcome: OneBotInboundOutcome) -> OneBotIgnoreReason {
    let OneBotInboundOutcome::Ignored(reason) = outcome else {
        panic!("expected ignored event, got {outcome:?}");
    };
    reason
}

fn private_event(self_id: Value, user_id: Value, message_id: Value) -> OneBotEvent {
    event(json!({
        "time": 1720000000,
        "self_id": self_id,
        "post_type": "message",
        "message_type": "private",
        "user_id": user_id,
        "message_id": message_id,
        "sender": {"nickname": "测试用户"},
        "message": [
            {"type": "text", "data": {"text": "你好"}},
            {"type": "text", "data": {"text": "，世界"}}
        ]
    }))
}

fn group_event(message: Value) -> OneBotEvent {
    event(json!({
        "time": 1720000001,
        "self_id": "10001",
        "post_type": "message",
        "message_type": "group",
        "user_id": "20002",
        "group_id": "30003",
        "message_id": "40004",
        "sender": {"card": "群名片", "nickname": "昵称", "role": "admin"},
        "message": message
    }))
}

#[test]
fn private_text_accepts_numeric_and_string_ids() {
    let cases = [
        (json!(10001), json!(20002), json!(30003)),
        (json!("10001"), json!("20002"), json!("30003")),
    ];

    for (self_id, user_id, message_id) in cases {
        let inbound = message(inbound_from_event(&private_event(
            self_id, user_id, message_id,
        )));
        assert_eq!(inbound.platform, Platform::OneBot11);
        assert_eq!(inbound.account_id.as_deref(), Some("10001"));
        assert_eq!(
            inbound.conversation,
            ConversationTarget::Private {
                target_id: "20002".to_owned()
            }
        );
        assert_eq!(inbound.actor.sender_id.as_deref(), Some("20002"));
        assert_eq!(inbound.actor.display_name.as_deref(), Some("测试用户"));
        assert_eq!(inbound.message_id, "30003");
        assert_eq!(inbound.timestamp.as_deref(), Some("1720000000"));
        assert_eq!(inbound.text, "你好，世界");
        assert_eq!(
            inbound
                .input_parts
                .iter()
                .filter_map(MessageInputPart::text_content)
                .collect::<Vec<_>>(),
            vec!["你好，世界"]
        );
        assert_eq!(render_text_for_core(&inbound), inbound.text);
        assert_eq!(
            core_scope_key(&inbound).unwrap(),
            "platform:onebot:account:10001:private:20002"
        );
    }
}

#[test]
fn group_trigger_table_distinguishes_self_at_other_at_and_self_message() {
    let cases = [
        (
            "at current bot",
            group_event(json!([
                {"type": "at", "data": {"qq": 10001}},
                {"type": "text", "data": {"text": " 请帮忙"}}
            ])),
            None,
        ),
        (
            "not triggered",
            group_event(json!([{"type": "text", "data": {"text": "路过"}}])),
            Some(OneBotIgnoreReason::GroupNotTriggered),
        ),
        (
            "at another member",
            group_event(json!([
                {"type": "at", "data": {"qq": "90009"}},
                {"type": "text", "data": {"text": " 看一下"}}
            ])),
            Some(OneBotIgnoreReason::GroupNotTriggered),
        ),
        (
            "self message",
            event(json!({
                "self_id": "10001",
                "post_type": "message",
                "message_type": "group",
                "user_id": "10001",
                "group_id": "30003",
                "message_id": "40004",
                "message": [{"type": "at", "data": {"qq": "10001"}}]
            })),
            Some(OneBotIgnoreReason::SelfMessage),
        ),
    ];

    for (name, event, expected_ignored) in cases {
        let outcome = inbound_from_event(&event);
        match expected_ignored {
            Some(reason) => assert_eq!(ignored(outcome), reason, "{name}"),
            None => {
                let inbound = message(outcome);
                assert_eq!(
                    inbound.conversation,
                    ConversationTarget::Group {
                        target_id: "30003".to_owned()
                    },
                    "{name}"
                );
                assert!(inbound.mentioned_bot, "{name}");
                assert_eq!(inbound.text, " 请帮忙", "{name}");
                assert_eq!(
                    inbound.actor.group_member_role,
                    Some(GroupMemberRoleKind::Admin),
                    "{name}"
                );
            }
        }
    }
}

#[test]
fn direct_group_slash_candidate_without_at_preserves_core_context() {
    let inbound = message(inbound_from_event(&group_event(json!([
        {"type": "text", "data": {"text": " /memory list"}}
    ]))));

    assert_eq!(inbound.text, " /memory list");
    assert!(!inbound.mentioned_bot);
    assert_eq!(inbound.message_id, "40004");
    assert_eq!(inbound.actor.sender_id.as_deref(), Some("20002"));
    assert_eq!(
        inbound.actor.group_member_role,
        Some(GroupMemberRoleKind::Admin)
    );
    assert_eq!(
        inbound.conversation,
        ConversationTarget::Group {
            target_id: "30003".to_owned()
        }
    );
    let request = to_core_request(&inbound, inbound.text.clone()).unwrap();
    assert!(!request.addressed_to_bot);
}

#[test]
fn structured_bot_at_maps_to_addressed_core_request() {
    let inbound = message(inbound_from_event(&group_event(json!([
        {"type": "at", "data": {"qq": "10001"}},
        {"type": "text", "data": {"text": " /unknown"}}
    ]))));

    let request = to_core_request(&inbound, inbound.text.clone()).unwrap();
    assert!(request.addressed_to_bot);
}

#[test]
fn custom_prefix_controls_direct_group_command_candidates() {
    let prefix = CommandPrefix::parse("#").unwrap();

    let custom = inbound_from_event_with_media_limit(
        &group_event(json!([
            {"type": "text", "data": {"text": " #help"}}
        ])),
        crate::config::DEFAULT_MEDIA_MAX_BYTES,
        prefix,
    );
    assert_eq!(message(custom).text, " #help");

    for text in ["/help", "##help"] {
        let outcome = inbound_from_event_with_media_limit(
            &group_event(json!([
                {"type": "text", "data": {"text": text}}
            ])),
            crate::config::DEFAULT_MEDIA_MAX_BYTES,
            prefix,
        );
        assert_eq!(ignored(outcome), OneBotIgnoreReason::GroupNotTriggered);
    }
}

#[test]
fn structured_bot_at_keeps_custom_prefix_group_trigger() {
    let inbound = message(inbound_from_event_with_media_limit(
        &group_event(json!([
            {"type": "at", "data": {"qq": "10001"}},
            {"type": "text", "data": {"text": " #help"}}
        ])),
        crate::config::DEFAULT_MEDIA_MAX_BYTES,
        CommandPrefix::parse("#").unwrap(),
    ));

    assert!(inbound.mentioned_bot);
    assert_eq!(inbound.text, " #help");
}

#[test]
fn removes_only_trigger_at_and_preserves_ordered_text_and_mentions() {
    let inbound = message(inbound_from_event(&group_event(json!([
        {"type": "text", "data": {"text": "请"}},
        {"type": "at", "data": {"qq": "10001"}},
        {"type": "text", "data": {"text": "帮"}},
        {"type": "at", "data": {"qq": 90009}},
        {"type": "text", "data": {"text": "看看"}}
    ]))));

    assert_eq!(inbound.text, "请帮看看");
    assert_eq!(
        inbound
            .input_parts
            .iter()
            .filter_map(MessageInputPart::text_content)
            .collect::<Vec<_>>(),
        vec!["请帮看看"]
    );
    assert_eq!(render_text_for_core(&inbound), inbound.text);
    assert_eq!(inbound.mentions.len(), 2);
    assert!(inbound.mentions[0].is_self);
    assert_eq!(inbound.mentions[0].target.user_id.as_deref(), Some("10001"));
    assert!(!inbound.mentions[1].is_self);
    assert_eq!(inbound.mentions[1].target.user_id.as_deref(), Some("90009"));
    assert_eq!(inbound.mentions[1].target.display_name, None);
    assert_eq!(inbound.mentions[1].target.is_bot, None);
}

#[test]
fn sender_role_table_maps_known_values_and_marks_unknown_value() {
    let cases = [
        ("owner", GroupMemberRoleKind::Owner),
        ("admin", GroupMemberRoleKind::Admin),
        ("member", GroupMemberRoleKind::Member),
        ("future_role", GroupMemberRoleKind::Unknown),
    ];

    for (role, expected) in cases {
        let inbound = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "group",
            "user_id": "20002",
            "group_id": "30003",
            "message_id": role,
            "sender": {"role": role},
            "message": [{"type": "at", "data": {"qq": "10001"}}]
        }))));
        assert_eq!(inbound.actor.group_member_role, Some(expected), "{role}");
    }
}

#[test]
fn empty_text_and_unknown_segment_degrade_without_dropping_message() {
    let empty = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "private",
        "user_id": "20002",
        "message_id": "empty",
        "message": [{"type": "text", "data": {"text": ""}}]
    }))));
    assert!(empty.text.is_empty());
    assert!(empty.input_parts.is_empty());

    let unknown = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "private",
        "user_id": "20002",
        "message_id": "unknown",
        "message": [
            {"type": "future_segment", "data": {"anything": {"nested": true}}},
            {"type": "text", "data": {"text": "仍可处理"}}
        ]
    }))));
    assert_eq!(unknown.text, "仍可处理");
    assert_eq!(unknown.input_parts.len(), 2);
    assert!(matches!(
        unknown.input_parts[0],
        MessageInputPart::Unknown {
            media: MessageMedia {
                status: MediaStatus::UnsupportedType,
                ..
            },
            ..
        }
    ));
    assert_eq!(unknown.input_parts[1].text_content(), Some("仍可处理"));
}

#[test]
fn reply_segment_maps_platform_message_id_without_qq_refidx_fields() {
    for reply_id in [json!(123456789), json!("123456789")] {
        let inbound = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "current-1",
            "message": [
                {"type": "reply", "data": {
                    "id": reply_id,
                    "text": "事件自带引用正文",
                    "user_id": 30003
                }},
                {"type": "text", "data": {"text": "继续"}}
            ]
        }))));

        let quoted = inbound.quoted.expect("reply should create quoted context");
        assert_eq!(quoted.current_message_id.as_deref(), Some("current-1"));
        assert_eq!(quoted.reference_id.as_deref(), Some("123456789"));
        assert_eq!(quoted.current_msg_idx, None);
        assert_eq!(quoted.ref_msg_idx, None);
        assert_eq!(quoted.text_summary.as_deref(), Some("事件自带引用正文"));
        assert_eq!(
            quoted.input_parts[0].text_content(),
            Some("事件自带引用正文")
        );
        assert_eq!(
            quoted.sender.and_then(|sender| sender.user_id),
            Some("30003".to_owned())
        );
    }
}

#[test]
fn text_image_and_file_segments_preserve_order_and_safe_metadata() {
    let inbound = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "private",
        "user_id": "20002",
        "message_id": "media-1",
        "message": [
            {"type": "text", "data": {"text": "前"}},
            {"type": "image", "data": {
                "file": "photo.png",
                "url": "https://example.test/photo.png?token=secret",
                "size": "1024",
                "image_id": "image-1"
            }},
            {"type": "text", "data": {"text": "中"}},
            {"type": "file", "data": {
                "file_id": 9988,
                "name": "report.pdf",
                "size": 2048,
                "mime_type": "application/pdf"
            }},
            {"type": "text", "data": {"text": "后"}}
        ]
    }))));

    assert_eq!(inbound.text, "前中后");
    assert_eq!(inbound.input_parts.len(), 5);
    assert_eq!(inbound.input_parts[0].text_content(), Some("前"));
    let MessageInputPart::Image { media: image } = &inbound.input_parts[1] else {
        panic!("expected image part");
    };
    assert_eq!(image.filename.as_deref(), Some("photo.png"));
    assert_eq!(image.mime_type.as_deref(), Some("image/png"));
    assert_eq!(image.size_bytes, Some(1024));
    assert_eq!(
        image.remote_url(),
        Some("https://example.test/photo.png?token=secret")
    );
    assert_eq!(image.media_id.as_deref(), Some("image-1"));
    assert_eq!(image.status, MediaStatus::Available);
    assert_eq!(inbound.input_parts[2].text_content(), Some("中"));
    let MessageInputPart::File { media: file } = &inbound.input_parts[3] else {
        panic!("expected file part");
    };
    assert_eq!(file.filename.as_deref(), Some("report.pdf"));
    assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
    assert_eq!(file.file_id.as_deref(), Some("9988"));
    assert_eq!(file.status, MediaStatus::MissingReadableUrl);
    assert_eq!(inbound.input_parts[4].text_content(), Some("后"));
}

#[test]
fn unsafe_local_base64_and_oversized_media_degrade_without_leaking_paths() {
    let inbound = message(inbound_from_event_with_media_limit(
        &event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "media-unsafe",
            "message": [
                {"type": "image", "data": {
                    "file": "C:\\Users\\someone\\secret.png",
                    "url": "file:///C:/Users/someone/secret.png",
                    "name": "C:\\Users\\someone\\secret.png"
                }},
                {"type": "image", "data": {"file": "base64://abcdef"}},
                {"type": "image", "data": {
                    "file": "large.jpg",
                    "url": "https://example.test/large.jpg",
                    "size": 11
                }}
            ]
        })),
        10,
        CommandPrefix::default(),
    ));

    for part in &inbound.input_parts[..2] {
        let MessageInputPart::Image { media } = part else {
            panic!("expected image part");
        };
        assert_eq!(media.url, None);
        assert_eq!(media.local_path, None);
        assert_eq!(media.filename, None);
        assert_eq!(media.file_id, None);
        assert_eq!(media.status, MediaStatus::MissingReadableUrl);
        assert!(!part.fallback_text().contains("Users"));
        assert!(!part.fallback_text().contains("base64"));
    }
    let MessageInputPart::Image { media: oversized } = &inbound.input_parts[2] else {
        panic!("expected oversized image part");
    };
    assert_eq!(oversized.status, MediaStatus::SizeExceeded);
    assert_eq!(oversized.size_bytes, Some(11));
}

#[test]
fn media_failure_extensions_map_to_real_fallback_statuses() {
    let inbound = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "private",
        "user_id": "20002",
        "message_id": "media-status",
        "message": [
            {"type": "image", "data": {
                "url": "https://example.test/expired.jpg",
                "status": "expired"
            }},
            {"type": "file", "data": {
                "name": "failed.pdf",
                "download_status": "download_failed"
            }}
        ]
    }))));

    assert_eq!(
        inbound.input_parts[0].media().unwrap().status,
        MediaStatus::Expired
    );
    assert_eq!(
        inbound.input_parts[1].media().unwrap().status,
        MediaStatus::DownloadFailed
    );
    assert!(render_text_for_core(&inbound).contains("[图片"));
    assert!(render_text_for_core(&inbound).contains("[文件"));
}

#[test]
fn unknown_events_message_sent_and_cq_strings_are_safely_ignored() {
    let cases = [
        (
            event(json!({
                "self_id": "10001",
                "post_type": "notice",
                "notice_type": "group_recall"
            })),
            OneBotIgnoreReason::NonMessageEvent,
        ),
        (
            event(json!({
                "self_id": "10001",
                "post_type": "message_sent",
                "message_type": "private",
                "user_id": "20002",
                "message_id": "sent",
                "message": [{"type": "text", "data": {"text": "echo"}}]
            })),
            OneBotIgnoreReason::MessageSent,
        ),
        (
            event(json!({
                "self_id": "10001",
                "post_type": "message",
                "message_type": "private",
                "user_id": "20002",
                "message_id": "cq",
                "message": "hello[CQ:at,qq=10001]"
            })),
            OneBotIgnoreReason::UnsupportedMessageEncoding,
        ),
    ];

    for (event, reason) in cases {
        assert_eq!(ignored(inbound_from_event(&event)), reason);
    }
}

#[test]
fn dedupe_key_is_stable_for_duplicates_and_isolated_by_account_and_conversation() {
    let base = message(inbound_from_event(&private_event(
        json!(10001),
        json!(20002),
        json!(30003),
    )));
    let duplicate = message(inbound_from_event(&private_event(
        json!("10001"),
        json!("20002"),
        json!("30003"),
    )));
    let other_account = message(inbound_from_event(&private_event(
        json!(10002),
        json!(20002),
        json!(30003),
    )));
    let group = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "group",
        "user_id": "20002",
        "group_id": "90009",
        "message_id": "30003",
        "message": [{"type": "at", "data": {"qq": "10001"}}]
    }))));
    let other_group = message(inbound_from_event(&event(json!({
        "self_id": "10001",
        "post_type": "message",
        "message_type": "group",
        "user_id": "20002",
        "group_id": "90010",
        "message_id": "30003",
        "message": [{"type": "at", "data": {"qq": "10001"}}]
    }))));

    let base_key = base.dedupe_message_key().unwrap();
    assert_eq!(
        duplicate.dedupe_message_key().as_deref(),
        Some(base_key.as_str())
    );
    assert_ne!(
        other_account.dedupe_message_key().as_deref(),
        Some(base_key.as_str())
    );
    assert_ne!(
        group.dedupe_message_key().as_deref(),
        Some(base_key.as_str())
    );
    assert_ne!(other_group.dedupe_message_key(), group.dedupe_message_key());

    let dedupe = MessageDedupe::new(Duration::from_secs(10));
    let now = Instant::now();
    assert!(!dedupe.check_and_insert_many([base_key.clone()], now));
    assert!(dedupe.check_and_insert_many([base_key], now));
    assert!(!dedupe.check_and_insert_many([other_account.dedupe_message_key().unwrap()], now));
    assert!(!dedupe.check_and_insert_many([group.dedupe_message_key().unwrap()], now));
    assert!(!dedupe.check_and_insert_many([other_group.dedupe_message_key().unwrap()], now));
}
