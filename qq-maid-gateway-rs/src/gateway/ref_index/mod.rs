//! QQ 消息引用绑定索引。
//!
//! 这里只保存平台归一化后的消息摘要、引用发送者和机器人出站消息绑定的可见实体快照，
//! 解决 QQ `REFIDX_*` 无法回查原文或出站展示实体的问题。Gateway 不解析 Todo、Memory、RSS
//! 等业务 domain；引用命中后仅把 `VisibleEntitySnapshot` 原样回填给 Core。
//! 当前实现为进程内缓存，重启后历史引用会失效；业务上下文组装仍由 Core 完成。

pub(crate) mod qq;

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use qq_maid_common::identity_context::MessageActorContext;
use qq_maid_common::input_part::{MediaStatus, MessageInputPart, MessageMedia, QuotedMediaSummary};
use qq_maid_core::service::{CoreGroupMemberRole, VisibleEntitySnapshot};
use tracing::{debug, warn};

use super::{
    logging::mask_identifier,
    platform::{ConversationTarget, InboundMessage},
};

pub(crate) type SharedRefIndex = Arc<Mutex<RefIndex>>;

const MAX_REF_ENTRIES: usize = 4096;
const MAX_REF_TEXT_SUMMARY_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefIndexKey {
    platform: String,
    app_id: String,
    peer_kind: String,
    peer_id: String,
    ref_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefIndexEntry {
    pub(crate) text_summary: Option<String>,
    pub(crate) media_summaries: Vec<QuotedMediaSummary>,
    pub(crate) input_parts: Vec<MessageInputPart>,
    pub(crate) from_bot: bool,
    /// 被引用消息发送者身份摘要；insert_inbound 时从 actor 回填，供后续 quote 查询回填 sender。
    pub(crate) sender: Option<MessageActorContext>,
    pub(crate) timestamp: Option<String>,
    /// 机器人出站消息展示的通用可见实体快照；Gateway 只按 ref id 绑定和回填，不解析业务域。
    pub(crate) visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

#[derive(Debug, Default)]
pub(crate) struct RefIndex {
    entries: HashMap<RefIndexKey, RefIndexEntry>,
    order: VecDeque<RefIndexKey>,
}

impl RefIndex {
    pub(crate) fn insert_inbound(&mut self, inbound: &InboundMessage) {
        let entry = entry_from_inbound(inbound);
        for ref_id in ref_ids_for_current_message(inbound) {
            self.insert(inbound, ref_id, entry.clone());
        }
    }

    pub(crate) fn insert_bot_outbound(
        &mut self,
        platform: super::platform::Platform,
        account_id: Option<&str>,
        conversation: &ConversationTarget,
        ref_index_id: Option<String>,
        text: &str,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) {
        let Some(ref_index_id) = clean_optional(ref_index_id) else {
            return;
        };
        let entry = RefIndexEntry {
            text_summary: clean_summary(Some(text.to_owned())),
            media_summaries: Vec::new(),
            input_parts: if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![MessageInputPart::text(truncate_summary_text(text))]
            },
            from_bot: true,
            // 机器人出站消息的发送者即机器人本身；稳定 ID 未知，标注 is_bot=true。
            sender: Some(MessageActorContext {
                is_bot: Some(true),
                source: qq_maid_common::identity_context::IdentitySource::Event,
                ..Default::default()
            }),
            timestamp: None,
            visible_entity_snapshot,
        };
        let key = key_for(platform, account_id, conversation, &ref_index_id);
        self.insert_key(key, entry);
    }

    pub(crate) fn enrich_inbound(&self, inbound: &mut InboundMessage) {
        let Some(quoted) = inbound.quoted.as_mut() else {
            return;
        };
        let Some(ref_id) = quoted.ref_msg_idx.as_deref().map(str::to_owned) else {
            quoted.lookup_found = false;
            quoted.fallback_reason = Some("missing_reference_id".to_owned());
            return;
        };
        let key = key_for(
            inbound.platform,
            inbound.account_id.as_deref(),
            &inbound.conversation,
            &ref_id,
        );
        if let Some(entry) = self.entries.get(&key) {
            quoted.lookup_found = true;
            quoted.text_summary = entry.text_summary.clone();
            quoted.media_summaries = entry.media_summaries.clone();
            quoted.input_parts = entry.input_parts.clone();
            quoted.from_bot = Some(entry.from_bot);
            quoted.sender = entry.sender.clone();
            quoted.fallback_reason = None;
            // 引用命中机器人出站消息时，把出站消息绑定的可见实体快照原样交回 Core。
            inbound.visible_entity_snapshot = entry.visible_entity_snapshot.clone();
            log_ref_index_hit("quoted_lookup", &key, entry);
        } else {
            log_ref_index_miss(&self.entries, &key);
            if quoted_has_payload_fallback(quoted) {
                quoted.lookup_found = true;
                quoted.from_bot = None;
                quoted.fallback_reason = Some("quoted_payload".to_owned());
            } else {
                quoted.lookup_found = false;
                quoted.fallback_reason = Some("ref_index_miss".to_owned());
            }
        }
    }

    fn insert(&mut self, inbound: &InboundMessage, ref_id: String, entry: RefIndexEntry) {
        let key = key_for(
            inbound.platform,
            inbound.account_id.as_deref(),
            &inbound.conversation,
            &ref_id,
        );
        self.insert_key(key, entry);
    }

    fn insert_key(&mut self, key: RefIndexKey, entry: RefIndexEntry) {
        if !self.entries.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        log_ref_index_insert(&key, &entry);
        self.entries.insert(key, entry);
        while self.entries.len() > MAX_REF_ENTRIES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

pub(crate) fn ref_index() -> SharedRefIndex {
    Arc::new(Mutex::new(RefIndex::default()))
}

fn ref_ids_for_current_message(inbound: &InboundMessage) -> Vec<String> {
    [inbound.current_msg_idx.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        })
        .collect()
}

fn quoted_has_payload_fallback(quoted: &qq_maid_common::input_part::QuotedMessageContext) -> bool {
    quoted
        .text_summary
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || !quoted.media_summaries.is_empty()
        || !quoted.input_parts.is_empty()
}

fn entry_from_inbound(inbound: &InboundMessage) -> RefIndexEntry {
    let text_summary = clean_summary(Some(inbound.text.clone()));
    let input_parts = effective_index_parts(inbound);
    let media_summaries = input_parts
        .iter()
        .filter_map(QuotedMediaSummary::from_input_part)
        .collect::<Vec<_>>();
    // 保存被索引消息的发送者身份，供后续引用该消息时回填 quoted.sender。
    // display_name 等展示字段在 Phase 1 阶段常为 None，由 Phase 3 成员详情补全。
    let sender = Some(MessageActorContext {
        user_id: inbound.actor.sender_id.clone(),
        union_id: inbound.actor.union_id.clone(),
        display_name: inbound.actor.display_name.clone(),
        display_name_source: inbound
            .actor
            .display_name
            .as_ref()
            .map(|_| inbound.actor.source.as_str().to_owned()),
        group_member_role: inbound
            .actor
            .group_member_role
            .map(|role| CoreGroupMemberRole::from(role).as_str().to_owned()),
        is_bot: Some(inbound.actor.is_bot),
        source: inbound.actor.source,
    });
    RefIndexEntry {
        text_summary,
        media_summaries,
        input_parts,
        from_bot: inbound.actor.is_bot,
        sender,
        timestamp: inbound.timestamp.clone(),
        visible_entity_snapshot: None,
    }
}

fn effective_index_parts(inbound: &InboundMessage) -> Vec<MessageInputPart> {
    if !inbound.input_parts.is_empty() {
        return sanitize_index_parts(inbound.input_parts.clone());
    }
    let mut parts = Vec::new();
    if !inbound.text.trim().is_empty() {
        parts.push(MessageInputPart::text(truncate_summary_text(&inbound.text)));
    }
    parts.extend(
        inbound
            .attachments
            .iter()
            .map(|attachment| attachment.to_input_part(inbound.platform)),
    );
    sanitize_index_parts(parts)
}

fn key_for(
    platform: super::platform::Platform,
    account_id: Option<&str>,
    conversation: &ConversationTarget,
    ref_id: &str,
) -> RefIndexKey {
    RefIndexKey {
        platform: platform.as_str().to_owned(),
        app_id: account_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .to_owned(),
        peer_kind: conversation.kind().to_owned(),
        peer_id: conversation.target_id().to_owned(),
        ref_id: ref_id.to_owned(),
    }
}

fn log_ref_index_insert(key: &RefIndexKey, entry: &RefIndexEntry) {
    debug!(
        platform = %key.platform,
        account = %mask_identifier(&key.app_id),
        account_present = key.app_id != "-",
        peer_kind = %key.peer_kind,
        peer_id = %mask_identifier(&key.peer_id),
        ref_id = %mask_identifier(&key.ref_id),
        from_bot = entry.from_bot,
        text_chars = entry
            .text_summary
            .as_deref()
            .map(|text| text.chars().count())
            .unwrap_or(0),
        media_count = entry.media_summaries.len(),
        input_part_count = entry.input_parts.len(),
        "ref_index insert"
    );
}

fn log_ref_index_hit(reason: &'static str, key: &RefIndexKey, entry: &RefIndexEntry) {
    debug!(
        platform = %key.platform,
        account = %mask_identifier(&key.app_id),
        account_present = key.app_id != "-",
        peer_kind = %key.peer_kind,
        peer_id = %mask_identifier(&key.peer_id),
        ref_id = %mask_identifier(&key.ref_id),
        from_bot = entry.from_bot,
        text_present = entry.text_summary.is_some(),
        media_count = entry.media_summaries.len(),
        reason,
        "ref_index hit"
    );
}

fn log_ref_index_miss(entries: &HashMap<RefIndexKey, RefIndexEntry>, query: &RefIndexKey) {
    let same_ref_candidates = entries
        .keys()
        .filter(|key| key.platform == query.platform && key.ref_id == query.ref_id)
        .collect::<Vec<_>>();
    let first_candidate = same_ref_candidates.first().copied();
    warn!(
        platform = %query.platform,
        account = %mask_identifier(&query.app_id),
        account_present = query.app_id != "-",
        peer_kind = %query.peer_kind,
        peer_id = %mask_identifier(&query.peer_id),
        ref_id = %mask_identifier(&query.ref_id),
        same_ref_candidate_count = same_ref_candidates.len(),
        candidate_account = %first_candidate
            .map(|key| mask_identifier(&key.app_id))
            .unwrap_or_default(),
        candidate_account_present = first_candidate.is_some_and(|key| key.app_id != "-"),
        candidate_peer_kind = first_candidate.map(|key| key.peer_kind.as_str()).unwrap_or(""),
        candidate_peer_id = %first_candidate
            .map(|key| mask_identifier(&key.peer_id))
            .unwrap_or_default(),
        "ref_index miss"
    );
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn clean_summary(value: Option<String>) -> Option<String> {
    clean_optional(value).map(|value| truncate_summary_text(&value))
}

fn truncate_summary_text(value: &str) -> String {
    let trimmed = value.trim();
    let char_count = trimmed.chars().count();
    if char_count <= MAX_REF_TEXT_SUMMARY_CHARS {
        return trimmed.to_owned();
    }
    let mut output = trimmed
        .chars()
        .take(MAX_REF_TEXT_SUMMARY_CHARS)
        .collect::<String>();
    output.push_str("...");
    output
}

fn sanitize_index_parts(parts: Vec<MessageInputPart>) -> Vec<MessageInputPart> {
    parts.into_iter().map(sanitize_index_part).collect()
}

fn sanitize_index_part(part: MessageInputPart) -> MessageInputPart {
    match part {
        MessageInputPart::Text { text, source } => MessageInputPart::Text {
            text: truncate_summary_text(&text),
            source,
        },
        MessageInputPart::Image { media } => MessageInputPart::Image {
            media: sanitize_index_media(media),
        },
        MessageInputPart::File { media } => MessageInputPart::File {
            media: sanitize_index_media(media),
        },
        MessageInputPart::Unknown { media, reason } => MessageInputPart::Unknown {
            media: sanitize_index_media(media),
            reason,
        },
    }
}

fn sanitize_index_media(mut media: MessageMedia) -> MessageMedia {
    // ref_index 只保存轻量引用元信息。data URL 可能携带 base64 内容，不能进入内存索引。
    if media
        .url
        .as_deref()
        .is_some_and(|value| value.trim_start().to_ascii_lowercase().starts_with("data:"))
    {
        media.url = None;
        if media
            .local_path
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            media.status = MediaStatus::MissingReadableUrl;
        }
    }
    media
}

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_common::identity_context::IdentitySource;
    use qq_maid_common::input_part::{MessageMedia, QuotedMessageContext};
    use qq_maid_core::service::{VisibleEntityItem, VisibleEntitySnapshot};

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

    fn quoted_group_lookup(store: &RefIndex, ref_id: &str) -> QuotedMessageContext {
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
        let store = RefIndex::default();
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
        let store = RefIndex::default();
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
        let mut message_a =
            group_inbound_from("gm-a", Some("REFIDX_A"), "内容 A", "member-a", false);
        let mut message_b =
            group_inbound_from("gm-b", Some("REFIDX_B"), "内容 B", "member-bot", true);
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

        let quoted_a_first = quoted_group_lookup(&store, "REFIDX_A");
        let quoted_b = quoted_group_lookup(&store, "REFIDX_B");
        let quoted_a_again = quoted_group_lookup(&store, "REFIDX_A");

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
        let quoted_b_after_a_update = quoted_group_lookup(&store, "REFIDX_B");
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
}
