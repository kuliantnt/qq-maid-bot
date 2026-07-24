use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct MessageDedupe {
    inner: Arc<DedupeInner>,
}

#[derive(Debug)]
struct DedupeInner {
    ttl: Duration,
    seen: Mutex<HashMap<String, DedupeEntry>>,
    next_token: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
enum DedupeEntry {
    Reserved { token: u64 },
    Committed { at: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Duplicate;

#[derive(Debug)]
pub struct MessageReservation {
    inner: Arc<DedupeInner>,
    token: u64,
    keys: Vec<String>,
    active: bool,
}

impl MessageReservation {
    pub fn commit(mut self) {
        self.commit_at(Instant::now());
    }

    pub fn commit_at(&mut self, now: Instant) {
        if !self.active {
            return;
        }
        let mut seen = self
            .inner
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        // commit 只确认当前 token 持有的 reservation；如果条目已被更新，不覆盖后来的状态。
        for key in &self.keys {
            if matches!(
                seen.get(key),
                Some(DedupeEntry::Reserved { token, .. }) if *token == self.token
            ) {
                seen.insert(key.clone(), DedupeEntry::Committed { at: now });
            }
        }
        self.active = false;
    }

    pub fn rollback(mut self) {
        self.rollback_inner();
    }

    fn rollback_inner(&mut self) {
        if !self.active {
            return;
        }
        let mut seen = self
            .inner
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        // rollback 必须带 token 校验，避免旧失败请求删掉后续新 reservation 或已 commit 记录。
        for key in &self.keys {
            if matches!(
                seen.get(key),
                Some(DedupeEntry::Reserved { token, .. }) if *token == self.token
            ) {
                seen.remove(key);
            }
        }
        self.active = false;
    }
}

impl Drop for MessageReservation {
    fn drop(&mut self) {
        self.rollback_inner();
    }
}

impl MessageDedupe {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DedupeInner {
                ttl,
                seen: Mutex::new(HashMap::new()),
                next_token: AtomicU64::new(1),
            }),
        }
    }

    pub fn is_duplicate(&self, message_id: &str) -> bool {
        self.check_and_insert_message(message_id, Instant::now())
    }

    pub fn contains_recent(&self, message_id: &str) -> bool {
        self.contains_recent_message(message_id, Instant::now())
    }

    pub fn check_and_insert_message(&self, message_id: &str, now: Instant) -> bool {
        self.check_and_insert_many([dedupe_message_key(message_id)], now)
    }

    pub fn contains_recent_message(&self, message_id: &str, now: Instant) -> bool {
        self.contains_recent_key(&dedupe_message_key(message_id), now)
    }

    pub fn contains_recent_event(&self, event_id: &str, now: Instant) -> bool {
        self.contains_recent_key(&dedupe_event_key(event_id), now)
    }

    pub fn reserve_many<I>(&self, ids: I, now: Instant) -> Result<MessageReservation, Duplicate>
    where
        I: IntoIterator<Item = String>,
    {
        let ids = ids
            .into_iter()
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(MessageReservation {
                inner: self.inner.clone(),
                token: 0,
                keys: Vec::new(),
                active: false,
            });
        }

        let mut seen = self
            .inner
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        Self::retain_recent_locked(&mut seen, self.inner.ttl, now);
        // 必须先完成全量命中检查再 reservation，保证一组物理 ID 的检查和写入原子完成。
        if ids.iter().any(|id| seen.contains_key(id)) {
            return Err(Duplicate);
        }
        let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed);
        for id in &ids {
            seen.insert(id.clone(), DedupeEntry::Reserved { token });
        }
        Ok(MessageReservation {
            inner: self.inner.clone(),
            token,
            keys: ids,
            active: true,
        })
    }

    pub fn check_and_insert_many<I>(&self, ids: I, now: Instant) -> bool
    where
        I: IntoIterator<Item = String>,
    {
        match self.reserve_many(ids, now) {
            Ok(mut reservation) => {
                reservation.commit_at(now);
                false
            }
            Err(Duplicate) => true,
        }
    }

    pub fn contains_recent_at(&self, message_id: &str, now: Instant) -> bool {
        self.contains_recent_message(message_id, now)
    }

    fn contains_recent_key(&self, key: &str, now: Instant) -> bool {
        if key.trim().is_empty() {
            return false;
        }
        let mut seen = self
            .inner
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        Self::retain_recent_locked(&mut seen, self.inner.ttl, now);
        seen.contains_key(key)
    }

    fn retain_recent_locked(seen: &mut HashMap<String, DedupeEntry>, ttl: Duration, now: Instant) {
        seen.retain(|_, entry| match entry {
            // Reserved 的生命周期由 MessageReservation 的 commit/rollback/Drop 管理；
            // 不能按 committed TTL 清理，否则长时间恢复中的活跃 reservation 会失效。
            DedupeEntry::Reserved { .. } => true,
            DedupeEntry::Committed { at } => match now.checked_duration_since(*at) {
                Some(age) => age <= ttl,
                // 测试或调用方传入的时间可能早于 reservation/commit 时间；此时不能因时间回拨清掉有效条目。
                None => true,
            },
        });
    }

    pub fn check_and_insert(&self, message_id: &str, now: Instant) -> bool {
        self.check_and_insert_message(message_id, now)
    }
}

pub(super) fn dedupe_message_key(message_id: &str) -> String {
    if message_id.trim().is_empty() {
        String::new()
    } else {
        format!("message:{message_id}")
    }
}

pub(super) fn dedupe_event_key(event_id: &str) -> String {
    if event_id.trim().is_empty() {
        String::new()
    } else {
        format!("event:{event_id}")
    }
}

/// 构建 QQ 官方入站消息的复合去重键。
///
/// 规则：platform + scene + peer_id + message_id + msg_idx。
/// - scene 至少区分 C2C 和群聊。
/// - peer_id 使用当前私聊用户或群聊 group_openid。
/// - msg_idx 存在时必须进入去重键；缺失时回退到纯 message_id 兼容行为。
pub(super) fn dedupe_qq_composite_key(
    scene: &str,
    peer_id: &str,
    message_id: &str,
    msg_idx: Option<&str>,
) -> String {
    let message_id = message_id.trim();
    let peer_id = peer_id.trim();
    if message_id.is_empty() || peer_id.is_empty() {
        return String::new();
    }
    match msg_idx.map(str::trim).filter(|value| !value.is_empty()) {
        Some(idx) => format!(
            "qq:{scene}:peer={}:{}:message={}:{}:idx={}:{}",
            peer_id.len(),
            peer_id,
            message_id.len(),
            message_id,
            idx.len(),
            idx
        ),
        // 旧事件没有 msg_idx 时必须保持历史 `message_id` 去重语义，避免重放恢复期
        // 因新协议键切换而重复投递。带 msg_idx 的新事件则不会发生跨会话碰撞。
        None => dedupe_message_key(message_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_within_ttl_and_expires_afterwards() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();

        assert!(!dedupe.check_and_insert("m1", now));
        assert!(dedupe.check_and_insert("m1", now + Duration::from_secs(5)));
        assert!(!dedupe.check_and_insert("m1", now + Duration::from_secs(11)));
    }

    #[test]
    fn message_and_event_ids_have_separate_namespaces() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();

        assert!(!dedupe.check_and_insert_many([dedupe_message_key("same")], now));
        assert!(!dedupe.check_and_insert_many([dedupe_event_key("same")], now));
        assert!(dedupe.check_and_insert_many([dedupe_message_key("same")], now));
        assert!(dedupe.check_and_insert_many([dedupe_event_key("same")], now));
    }

    #[test]
    fn many_id_check_is_atomic_when_any_id_was_seen() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();

        assert!(!dedupe.check_and_insert_many([dedupe_event_key("e1")], now));
        assert!(
            dedupe.check_and_insert_many([dedupe_message_key("m1"), dedupe_event_key("e1")], now)
        );
        assert!(!dedupe.check_and_insert_many([dedupe_message_key("m1")], now));
    }

    #[test]
    fn reservation_rolls_back_without_committing_duplicate() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();
        let reservation = dedupe
            .reserve_many([dedupe_message_key("m1")], now)
            .expect("reservation should succeed");
        assert!(dedupe.contains_recent_message("m1", now));
        reservation.rollback();
        assert!(!dedupe.contains_recent_message("m1", now));
    }

    #[test]
    fn committed_reservation_remains_duplicate() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();
        dedupe
            .reserve_many([dedupe_event_key("e1")], now)
            .expect("reservation should succeed")
            .commit();
        assert!(dedupe.reserve_many([dedupe_event_key("e1")], now).is_err());
    }

    #[test]
    fn active_reservation_is_not_removed_by_committed_ttl_cleanup() {
        let dedupe = MessageDedupe::new(Duration::from_millis(1));
        let now = Instant::now();
        let reservation = dedupe
            .reserve_many([dedupe_message_key("m1")], now)
            .expect("reservation should succeed");

        assert!(
            dedupe
                .reserve_many([dedupe_message_key("m1")], now + Duration::from_secs(1))
                .is_err()
        );
        reservation.rollback();
        assert!(
            dedupe
                .reserve_many([dedupe_message_key("m1")], now + Duration::from_secs(1))
                .is_ok()
        );
    }

    #[test]
    fn rollback_does_not_delete_newer_reservation_or_committed_entry() {
        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();

        let old_committed = dedupe
            .reserve_many([dedupe_message_key("m1")], now)
            .expect("reservation should succeed");
        let mut stale_after_commit = MessageReservation {
            inner: old_committed.inner.clone(),
            token: old_committed.token,
            keys: old_committed.keys.clone(),
            active: true,
        };
        old_committed.commit();
        stale_after_commit.rollback_inner();
        assert!(dedupe.contains_recent_message("m1", now));

        let old = dedupe
            .reserve_many([dedupe_message_key("m2")], now)
            .expect("reservation should succeed");
        let mut stale_after_newer = MessageReservation {
            inner: old.inner.clone(),
            token: old.token,
            keys: old.keys.clone(),
            active: true,
        };
        {
            let mut seen = dedupe.inner.seen.lock().unwrap();
            seen.insert(
                dedupe_message_key("m2"),
                DedupeEntry::Reserved {
                    token: old.token + 1,
                },
            );
        }
        old.rollback();
        stale_after_newer.rollback_inner();
        assert!(dedupe.contains_recent_message("m2", now));
    }

    // --- QQ 复合去重键测试 ---

    #[test]
    fn qq_composite_key_same_msg_id_and_msg_idx_is_duplicate() {
        let key1 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", Some("idx-1"));
        let key2 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", Some("idx-1"));
        assert_eq!(key1, key2);
        assert!(!key1.is_empty());

        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();
        assert!(!dedupe.check_and_insert(&key1, now));
        assert!(dedupe.check_and_insert(&key2, now));
    }

    #[test]
    fn qq_composite_key_same_msg_id_different_msg_idx_not_duplicate() {
        let key1 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", Some("idx-1"));
        let key2 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", Some("idx-2"));
        assert_ne!(key1, key2);

        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();
        assert!(!dedupe.check_and_insert(&key1, now));
        assert!(!dedupe.check_and_insert(&key2, now));
    }

    #[test]
    fn qq_composite_key_different_sessions_no_cross_collision() {
        let key1 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", Some("idx-1"));
        let key2 = dedupe_qq_composite_key("c2c", "user-2", "msg-1", Some("idx-1"));
        assert_ne!(key1, key2);
    }

    #[test]
    fn qq_composite_key_missing_msg_idx_compatible() {
        let key1 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", None);
        let key2 = dedupe_qq_composite_key("c2c", "user-1", "msg-1", None);
        assert_eq!(key1, key2);
        assert_eq!(key1, dedupe_message_key("msg-1"));
    }

    #[test]
    fn qq_composite_key_cross_scene_no_collision() {
        let c2c_key = dedupe_qq_composite_key("c2c", "peer-1", "msg-1", Some("idx-1"));
        let group_key = dedupe_qq_composite_key("group", "peer-1", "msg-1", Some("idx-1"));
        assert_ne!(c2c_key, group_key);
    }

    #[test]
    fn qq_composite_key_empty_input_returns_empty() {
        assert!(dedupe_qq_composite_key("c2c", "", "msg-1", None).is_empty());
        assert!(dedupe_qq_composite_key("c2c", "user-1", "", None).is_empty());
    }
}
