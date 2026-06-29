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
}
