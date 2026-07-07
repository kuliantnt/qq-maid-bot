//! Gateway 短时缓存。
//!
//! 这里只保存用于群聊触发策略的 bot 出站真实消息 ID 和独立 ref_index ID；
//! 引用上下文内容统一走 `ref_index`。

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use tracing::debug;

use super::logging::mask_identifier;

const BOT_OUTBOUND_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_BOT_OUTBOUND_MESSAGE_IDS: usize = 4096;
const MAX_BOT_OUTBOUND_REF_INDEX_IDS: usize = 4096;

#[derive(Debug)]
pub(crate) struct BotOutboundCache {
    message_ids: BoundedIdSet,
    ref_index_ids: BoundedIdSet,
}

impl BotOutboundCache {
    pub(crate) fn new(ttl: Duration, max_message_ids: usize, max_ref_index_ids: usize) -> Self {
        Self {
            message_ids: BoundedIdSet::new(
                "message_id",
                ttl,
                max_message_ids,
                "bot outbound message id cache",
            ),
            ref_index_ids: BoundedIdSet::new(
                "ref_index_id",
                ttl,
                max_ref_index_ids,
                "bot outbound ref_index id cache",
            ),
        }
    }

    pub(crate) fn insert(&mut self, message_id: Option<String>) {
        if let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) {
            self.message_ids.insert(message_id);
        }
    }

    pub(crate) fn insert_ref_index_id(&mut self, ref_index_id: Option<String>) {
        if let Some(ref_index_id) = ref_index_id.filter(|value| !value.trim().is_empty()) {
            self.ref_index_ids.insert(ref_index_id);
        }
    }

    pub(crate) fn contains(&mut self, message_id: &str) -> bool {
        self.message_ids.contains(message_id)
    }

    pub(crate) fn contains_ref_index_id(&mut self, ref_index_id: &str) -> bool {
        self.ref_index_ids.contains(ref_index_id)
    }
}

impl Default for BoundedIdSet {
    fn default() -> Self {
        Self::new(
            "id",
            BOT_OUTBOUND_CACHE_TTL,
            MAX_BOT_OUTBOUND_MESSAGE_IDS,
            "bot outbound id cache",
        )
    }
}

#[derive(Debug)]
struct BoundedIdSet {
    kind: &'static str,
    log_label: &'static str,
    ttl: Duration,
    max_entries: usize,
    entries: HashMap<String, Instant>,
    order: VecDeque<String>,
    expired_evictions: usize,
    capacity_evictions: usize,
}

impl BoundedIdSet {
    fn new(kind: &'static str, ttl: Duration, max_entries: usize, log_label: &'static str) -> Self {
        Self {
            kind,
            log_label,
            ttl,
            max_entries,
            entries: HashMap::new(),
            order: VecDeque::new(),
            expired_evictions: 0,
            capacity_evictions: 0,
        }
    }

    fn insert(&mut self, value: String) {
        let now = Instant::now();
        self.prune_expired(now);
        if self.entries.contains_key(&value) {
            self.remove_from_order(&value);
        }
        self.order.push_back(value.clone());
        self.entries.insert(value, now);
        self.prune_capacity();
        self.log_metrics();
    }

    fn contains(&mut self, value: &str) -> bool {
        self.prune_expired(Instant::now());
        self.entries.contains_key(value)
    }

    fn prune_expired(&mut self, now: Instant) {
        while let Some(oldest) = self.order.front().cloned() {
            let Some(inserted_at) = self.entries.get(&oldest).copied() else {
                self.order.pop_front();
                continue;
            };
            if now.duration_since(inserted_at) < self.ttl {
                break;
            }
            self.order.pop_front();
            if self.entries.remove(&oldest).is_some() {
                self.expired_evictions += 1;
                self.log_eviction("expired", &oldest);
            }
        }
    }

    fn prune_capacity(&mut self) {
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                self.capacity_evictions += 1;
                self.log_eviction("capacity", &oldest);
            }
        }
    }

    fn remove_from_order(&mut self, value: &str) {
        self.order.retain(|existing| existing != value);
    }

    fn log_eviction(&self, reason: &'static str, value: &str) {
        debug!(
            cache = self.log_label,
            id_kind = self.kind,
            id = %mask_identifier(value),
            reason,
            entries = self.entries.len(),
            max_entries = self.max_entries,
            expired_evictions = self.expired_evictions,
            capacity_evictions = self.capacity_evictions,
            "gateway short-lived cache evicted id"
        );
    }

    fn log_metrics(&self) {
        debug!(
            cache = self.log_label,
            id_kind = self.kind,
            entries = self.entries.len(),
            max_entries = self.max_entries,
            ttl_seconds = self.ttl.as_secs(),
            expired_evictions = self.expired_evictions,
            capacity_evictions = self.capacity_evictions,
            "gateway short-lived cache metrics"
        );
    }
}

impl Default for BotOutboundCache {
    fn default() -> Self {
        Self::new(
            BOT_OUTBOUND_CACHE_TTL,
            MAX_BOT_OUTBOUND_MESSAGE_IDS,
            MAX_BOT_OUTBOUND_REF_INDEX_IDS,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_cache_evicts_by_capacity() {
        let mut cache = BotOutboundCache::new(Duration::from_secs(60), 2, 2);
        cache.insert(Some("m1".to_owned()));
        cache.insert(Some("m2".to_owned()));
        cache.insert(Some("m3".to_owned()));

        assert!(!cache.contains("m1"));
        assert!(cache.contains("m2"));
        assert!(cache.contains("m3"));
    }

    #[test]
    fn ref_index_cache_expires_by_ttl() {
        let mut cache = BotOutboundCache::new(Duration::ZERO, 2, 2);
        cache.insert_ref_index_id(Some("REFIDX_1".to_owned()));

        assert!(!cache.contains_ref_index_id("REFIDX_1"));
    }

    #[test]
    fn message_id_and_ref_index_id_have_separate_limits() {
        let mut cache = BotOutboundCache::new(Duration::from_secs(60), 1, 1);
        cache.insert(Some("m1".to_owned()));
        cache.insert_ref_index_id(Some("REFIDX_1".to_owned()));
        cache.insert(Some("m2".to_owned()));
        cache.insert_ref_index_id(Some("REFIDX_2".to_owned()));

        assert!(!cache.contains("m1"));
        assert!(cache.contains("m2"));
        assert!(!cache.contains_ref_index_id("REFIDX_1"));
        assert!(cache.contains_ref_index_id("REFIDX_2"));
    }
}
