//! 平台消息引用绑定索引。
//!
//! 这里只保存平台归一化后的消息摘要、引用发送者和机器人出站消息绑定的可见实体快照，
//! 解决 QQ `REFIDX_*`、OneBot `message_id` 无法直接回查原文或出站展示实体的问题。
//! Gateway 不解析 Todo、Memory、RSS 等业务 domain；引用命中后仅把
//! `VisibleEntitySnapshot` 原样回填给 Core。
//! 当前实现为进程内缓存，重启后历史引用会失效；业务上下文组装仍由 Core 完成。

mod merge;
pub(crate) mod qq;

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use qq_maid_common::identity_context::MessageActorContext;
use qq_maid_common::input_part::{MediaStatus, MessageInputPart, MessageMedia, QuotedMediaSummary};
use qq_maid_core::service::{CoreGroupMemberRole, VisibleEntitySnapshot};
use tracing::{debug, warn};

use super::{
    logging::mask_identifier,
    platform::{ConversationTarget, InboundMessage},
};
use merge::merge_passive_observation_parts;

pub(crate) type SharedRefIndex = Arc<Mutex<RefIndex>>;

const MAX_REF_ENTRIES: usize = 4096;
const MAX_REF_ENTRIES_PER_SCOPE: usize = 512;
const MAX_REF_TEXT_SUMMARY_CHARS: usize = 2000;
const REF_INDEX_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefIndexKey {
    platform: String,
    app_id: String,
    peer_kind: String,
    peer_id: String,
    ref_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefIndexScopeKey {
    platform: String,
    app_id: String,
    peer_kind: String,
    peer_id: String,
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
    source: RefIndexEntrySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefIndexEntrySource {
    /// 已走完正常入站媒体下载与归一化流程，可作为引用 payload 的权威快照。
    ProcessedInbound,
    /// 被 mode policy 忽略后的轻量观察，正文可靠，但媒体可能尚未下载。
    PassiveObservation,
    /// 机器人成功发送后按平台引用 ID 绑定的出站正文与可见实体快照。
    BotOutbound,
}

impl RefIndexEntrySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProcessedInbound => "processed_inbound",
            Self::PassiveObservation => "passive_observation",
            Self::BotOutbound => "bot_outbound",
        }
    }

    fn is_passive(self) -> bool {
        self == Self::PassiveObservation
    }
}

#[derive(Debug)]
pub(crate) struct RefIndex {
    entries: HashMap<RefIndexKey, RefIndexRecord>,
    order: VecDeque<RefIndexKey>,
    ttl: Duration,
    max_entries: usize,
    max_entries_per_scope: usize,
    expired_evictions: usize,
    capacity_evictions: usize,
    scope_evictions: usize,
}

#[derive(Debug, Clone)]
struct RefIndexRecord {
    entry: RefIndexEntry,
    inserted_at: Instant,
}

impl RefIndex {
    pub(crate) fn new(ttl: Duration, max_entries: usize, max_entries_per_scope: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            max_entries,
            max_entries_per_scope,
            expired_evictions: 0,
            capacity_evictions: 0,
            scope_evictions: 0,
        }
    }

    pub(crate) fn insert_inbound(&mut self, inbound: &InboundMessage) {
        self.insert_inbound_with_source(inbound, RefIndexEntrySource::ProcessedInbound);
    }

    /// 记录被策略忽略的入站消息。此入口不会提升媒体完整度，命中时必须与事件 payload 合并。
    pub(crate) fn insert_passive_observation(&mut self, inbound: &InboundMessage) {
        self.insert_inbound_with_source(inbound, RefIndexEntrySource::PassiveObservation);
    }

    fn insert_inbound_with_source(
        &mut self,
        inbound: &InboundMessage,
        source: RefIndexEntrySource,
    ) {
        let entry = entry_from_inbound(inbound, source);
        for ref_id in ref_ids_for_current_message(inbound) {
            self.insert(inbound, ref_id, entry.clone());
        }
    }

    pub(crate) fn insert_bot_outbound(
        &mut self,
        platform: super::platform::Platform,
        account_id: Option<&str>,
        conversation: &ConversationTarget,
        platform_reference_id: Option<String>,
        text: &str,
        visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    ) {
        let Some(platform_reference_id) = clean_optional(platform_reference_id) else {
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
            source: RefIndexEntrySource::BotOutbound,
        };
        let key = key_for(platform, account_id, conversation, &platform_reference_id);
        self.insert_key(key, entry);
    }

    pub(crate) fn enrich_inbound(&mut self, inbound: &mut InboundMessage) {
        self.prune_expired(Instant::now());
        let ref_id = ref_id_for_quoted_message(inbound);
        let Some(quoted) = inbound.quoted.as_mut() else {
            return;
        };
        let Some(ref_id) = ref_id else {
            // ref_msg_idx / reference_id 缺失但 msg_elements 携带有效 payload 时，
            // 标记为 quoted_payload_without_reference_id，引用正文和媒体仍可进入模型。
            if quoted_has_payload_fallback(quoted) {
                quoted.lookup_found = true;
                quoted.fallback_reason = Some("quoted_payload_without_reference_id".to_owned());
            } else {
                quoted.lookup_found = false;
                quoted.fallback_reason = Some("missing_reference_id".to_owned());
            }
            return;
        };
        let key = key_for(
            inbound.platform,
            inbound.account_id.as_deref(),
            &inbound.conversation,
            &ref_id,
        );
        if let Some(record) = self.entries.get(&key) {
            let entry = &record.entry;
            quoted.lookup_found = true;
            quoted.text_summary = entry.text_summary.clone();
            if entry.source.is_passive() {
                let event_parts = std::mem::take(&mut quoted.input_parts);
                quoted.input_parts =
                    merge_passive_observation_parts(&entry.input_parts, event_parts);
                quoted.media_summaries = quoted
                    .input_parts
                    .iter()
                    .filter_map(QuotedMediaSummary::from_input_part)
                    .collect();
            } else {
                // 完整入站和机器人出站记录继续覆盖展示 payload，保留 #582 后的正文去污染语义。
                quoted.media_summaries = entry.media_summaries.clone();
                quoted.input_parts = entry.input_parts.clone();
            }
            quoted.from_bot = Some(entry.from_bot);
            quoted.sender = entry.sender.clone();
            quoted.timestamp = entry.timestamp.clone();
            quoted.fallback_reason = None;
            // 引用命中机器人出站消息时，把出站消息绑定的可见实体快照原样交回 Core。
            inbound.visible_entity_snapshot = entry.visible_entity_snapshot.clone();
            log_ref_index_hit("quoted_lookup", &key, entry);
        } else {
            let payload_fallback_available = quoted_has_payload_fallback(quoted);
            log_ref_index_miss(&self.entries, &key, payload_fallback_available);
            if payload_fallback_available {
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
        let now = Instant::now();
        self.prune_expired(now);
        // 被动观察不能把同一引用键下已完成下载的入站记录或机器人出站记录降级。
        if entry.source.is_passive()
            && self
                .entries
                .get(&key)
                .is_some_and(|record| !record.entry.source.is_passive())
        {
            return;
        }
        if self.entries.contains_key(&key) {
            self.remove_from_order(&key);
        }
        self.order.push_back(key.clone());
        self.entries.insert(
            key.clone(),
            RefIndexRecord {
                entry: entry.clone(),
                inserted_at: now,
            },
        );
        self.prune_scope_capacity(&key.scope());
        self.prune_global_capacity();
        log_ref_index_insert(&key, &entry, self);
        log_ref_index_metrics(self);
    }

    fn prune_expired(&mut self, now: Instant) {
        while let Some(oldest) = self.order.front().cloned() {
            let Some(record) = self.entries.get(&oldest) else {
                self.order.pop_front();
                continue;
            };
            if now.duration_since(record.inserted_at) < self.ttl {
                break;
            }
            self.order.pop_front();
            if self.entries.remove(&oldest).is_some() {
                self.expired_evictions += 1;
                log_ref_index_eviction("expired", &oldest, self);
            }
        }
    }

    fn prune_scope_capacity(&mut self, scope: &RefIndexScopeKey) {
        while self.scope_len(scope) > self.max_entries_per_scope {
            if !self.evict_oldest_in_scope(scope, "scope_capacity") {
                break;
            }
        }
    }

    fn prune_global_capacity(&mut self) {
        while self.entries.len() > self.max_entries {
            let passive_position = self.order.iter().position(|key| {
                self.entries
                    .get(key)
                    .is_some_and(|record| record.entry.source.is_passive())
            });
            let oldest = passive_position
                .and_then(|position| self.order.remove(position))
                .or_else(|| self.order.pop_front());
            if let Some(oldest) = oldest {
                if self.entries.remove(&oldest).is_some() {
                    self.capacity_evictions += 1;
                    log_ref_index_eviction("global_capacity", &oldest, self);
                }
            } else {
                break;
            }
        }
    }

    fn evict_oldest_in_scope(&mut self, scope: &RefIndexScopeKey, reason: &'static str) -> bool {
        // 普通群消息量可能远大于机器人交互量；同 scope 超限时优先淘汰被动观察，
        // 避免它们先挤掉机器人出站记录或已经完成媒体下载的入站记录。
        let passive_position = self.order.iter().position(|key| {
            key.matches_scope(scope)
                && self
                    .entries
                    .get(key)
                    .is_some_and(|record| record.entry.source.is_passive())
        });
        let Some(position) =
            passive_position.or_else(|| self.order.iter().position(|key| key.matches_scope(scope)))
        else {
            return false;
        };
        let Some(oldest) = self.order.remove(position) else {
            return false;
        };
        if self.entries.remove(&oldest).is_some() {
            self.scope_evictions += 1;
            log_ref_index_eviction(reason, &oldest, self);
            return true;
        }
        false
    }

    fn scope_len(&self, scope: &RefIndexScopeKey) -> usize {
        self.entries
            .keys()
            .filter(|key| key.matches_scope(scope))
            .count()
    }

    fn remove_from_order(&mut self, key: &RefIndexKey) {
        self.order.retain(|existing| existing != key);
    }
}

impl Default for RefIndex {
    fn default() -> Self {
        Self::new(REF_INDEX_TTL, MAX_REF_ENTRIES, MAX_REF_ENTRIES_PER_SCOPE)
    }
}

impl RefIndexKey {
    fn scope(&self) -> RefIndexScopeKey {
        RefIndexScopeKey {
            platform: self.platform.clone(),
            app_id: self.app_id.clone(),
            peer_kind: self.peer_kind.clone(),
            peer_id: self.peer_id.clone(),
        }
    }

    fn matches_scope(&self, scope: &RefIndexScopeKey) -> bool {
        self.platform == scope.platform
            && self.app_id == scope.app_id
            && self.peer_kind == scope.peer_kind
            && self.peer_id == scope.peer_id
    }
}

pub(crate) fn ref_index() -> SharedRefIndex {
    Arc::new(Mutex::new(RefIndex::default()))
}

fn ref_ids_for_current_message(inbound: &InboundMessage) -> Vec<String> {
    let platform_ref_id = match inbound.platform {
        // QQ 官方只有 REFIDX/current_msg_idx 是权威引用索引键，不能退回 message_id。
        super::platform::Platform::QqOfficial => inbound.current_msg_idx.as_deref(),
        // OneBot reply segment 直接引用平台 message_id；echo 与业务实体 ID 均不参与索引。
        super::platform::Platform::OneBot11 => Some(inbound.message_id.as_str()),
        super::platform::Platform::WechatService => None,
    };
    [platform_ref_id]
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        })
        .collect()
}

fn ref_id_for_quoted_message(inbound: &InboundMessage) -> Option<String> {
    let value = match inbound.platform {
        super::platform::Platform::QqOfficial => inbound
            .quoted
            .as_ref()
            .and_then(|quoted| quoted.ref_msg_idx.as_deref()),
        super::platform::Platform::OneBot11 => inbound
            .quoted
            .as_ref()
            .and_then(|quoted| quoted.reference_id.as_deref()),
        super::platform::Platform::WechatService => None,
    }?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn quoted_has_payload_fallback(quoted: &qq_maid_common::input_part::QuotedMessageContext) -> bool {
    quoted
        .text_summary
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || !quoted.media_summaries.is_empty()
        || !quoted.input_parts.is_empty()
}

fn entry_from_inbound(inbound: &InboundMessage, source: RefIndexEntrySource) -> RefIndexEntry {
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
        source,
    }
}

fn effective_index_parts(inbound: &InboundMessage) -> Vec<MessageInputPart> {
    let parts = if !inbound.input_parts.is_empty() {
        inbound.input_parts.clone()
    } else {
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
        parts
    };
    // RefIndex 只保存当前消息自身的 parts，不复制 quoted 结构，因此不会形成递归
    // 媒体树。QQ 拍平到当前正文中的展示文本仍由普通 Text part 原样处理。
    sanitize_index_parts(
        parts,
        matches!(inbound.platform, super::platform::Platform::QqOfficial),
    )
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

fn log_ref_index_insert(key: &RefIndexKey, entry: &RefIndexEntry, store: &RefIndex) {
    debug!(
        platform = %key.platform,
        account = %mask_identifier(&key.app_id),
        account_present = key.app_id != "-",
        peer_kind = %key.peer_kind,
        peer_id = %mask_identifier(&key.peer_id),
        ref_id = %mask_identifier(&key.ref_id),
        entries = store.entries.len(),
        max_entries = store.max_entries,
        scope_entries = store.scope_len(&key.scope()),
        max_entries_per_scope = store.max_entries_per_scope,
        from_bot = entry.from_bot,
        source = entry.source.as_str(),
        text_chars = entry
            .text_summary
            .as_deref()
            .map(|text| text.chars().count())
            .unwrap_or(0),
        media_count = entry.media_summaries.len(),
        input_part_count = entry.input_parts.len(),
        "ref_index 已写入条目"
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
        source = entry.source.as_str(),
        text_present = entry.text_summary.is_some(),
        media_count = entry.media_summaries.len(),
        reason,
        "ref_index 已命中条目"
    );
}

fn log_ref_index_miss(
    entries: &HashMap<RefIndexKey, RefIndexRecord>,
    query: &RefIndexKey,
    payload_fallback_available: bool,
) {
    let same_ref_candidates = entries
        .keys()
        .filter(|key| key.platform == query.platform && key.ref_id == query.ref_id)
        .collect::<Vec<_>>();
    let first_candidate = same_ref_candidates.first().copied();
    macro_rules! emit_miss {
        ($level:ident) => {
            $level!(
                platform = %query.platform,
                account = %mask_identifier(&query.app_id),
                account_present = query.app_id != "-",
                peer_kind = %query.peer_kind,
                peer_id = %mask_identifier(&query.peer_id),
                ref_id = %mask_identifier(&query.ref_id),
                payload_fallback_available,
                same_ref_candidate_count = same_ref_candidates.len(),
                candidate_account = %first_candidate
                    .map(|key| mask_identifier(&key.app_id))
                    .unwrap_or_default(),
                candidate_account_present = first_candidate.is_some_and(|key| key.app_id != "-"),
                candidate_peer_kind = first_candidate
                    .map(|key| key.peer_kind.as_str())
                    .unwrap_or(""),
                candidate_peer_id = %first_candidate
                    .map(|key| mask_identifier(&key.peer_id))
                    .unwrap_or_default(),
                "ref_index 未命中条目"
            );
        };
    }
    // 事件已自带引用 payload 时可以无损降级，不应作为运行告警；只有引用内容确实
    // 无法恢复时保留 WARN，便于区分索引断档与可预期的进程内缓存 miss。
    if payload_fallback_available {
        emit_miss!(debug);
    } else {
        emit_miss!(warn);
    }
}

fn log_ref_index_eviction(reason: &'static str, key: &RefIndexKey, store: &RefIndex) {
    debug!(
        platform = %key.platform,
        account = %mask_identifier(&key.app_id),
        account_present = key.app_id != "-",
        peer_kind = %key.peer_kind,
        peer_id = %mask_identifier(&key.peer_id),
        ref_id = %mask_identifier(&key.ref_id),
        reason,
        entries = store.entries.len(),
        max_entries = store.max_entries,
        scope_entries = store.scope_len(&key.scope()),
        max_entries_per_scope = store.max_entries_per_scope,
        expired_evictions = store.expired_evictions,
        capacity_evictions = store.capacity_evictions,
        scope_evictions = store.scope_evictions,
        "ref_index 已淘汰条目"
    );
}

fn log_ref_index_metrics(store: &RefIndex) {
    debug!(
        entries = store.entries.len(),
        scopes = store
            .entries
            .keys()
            .map(RefIndexKey::scope)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        max_entries = store.max_entries,
        max_entries_per_scope = store.max_entries_per_scope,
        ttl_seconds = store.ttl.as_secs(),
        expired_evictions = store.expired_evictions,
        capacity_evictions = store.capacity_evictions,
        scope_evictions = store.scope_evictions,
        "ref_index 统计"
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

fn sanitize_index_parts(
    parts: Vec<MessageInputPart>,
    clear_remote_media_urls: bool,
) -> Vec<MessageInputPart> {
    parts
        .into_iter()
        .map(|part| sanitize_index_part(part, clear_remote_media_urls))
        .collect()
}

fn sanitize_index_part(part: MessageInputPart, clear_remote_media_urls: bool) -> MessageInputPart {
    match part {
        MessageInputPart::Text { text, source } => MessageInputPart::Text {
            text: truncate_summary_text(&text),
            source,
        },
        MessageInputPart::Image { media } => MessageInputPart::Image {
            media: sanitize_index_media(media, clear_remote_media_urls),
        },
        MessageInputPart::File { media } => MessageInputPart::File {
            media: sanitize_index_media(media, clear_remote_media_urls),
        },
        MessageInputPart::Unknown { media, reason } => MessageInputPart::Unknown {
            media: sanitize_index_media(media, clear_remote_media_urls),
            reason,
        },
    }
}

fn sanitize_index_media(mut media: MessageMedia, clear_remote_media_urls: bool) -> MessageMedia {
    // QQ 临时媒体 URL 可能带 rkey/auth_token；下载完成后只保留本地缓存路径。
    // 其他平台仍保留普通 http(s) 引用，但 data URL 对所有平台都不得进入内存索引。
    let is_data_url = media
        .url
        .as_deref()
        .is_some_and(|value| value.trim_start().to_ascii_lowercase().starts_with("data:"));
    if clear_remote_media_urls || is_data_url {
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
mod tests;
