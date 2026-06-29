use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct MessageDedupe {
    ttl: Duration,
    seen: Mutex<HashMap<String, Instant>>,
}

impl MessageDedupe {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            seen: Mutex::new(HashMap::new()),
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

    pub fn check_and_insert_many<I>(&self, ids: I, now: Instant) -> bool
    where
        I: IntoIterator<Item = String>,
    {
        let ids = ids
            .into_iter()
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return false;
        }

        let mut seen = self
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        seen.retain(|_, first_seen| now.duration_since(*first_seen) <= self.ttl);
        // 必须先完成全量命中检查再写入，避免部分 ID 命中时把未处理消息错误标记为已处理。
        if ids.iter().any(|id| seen.contains_key(id)) {
            return true;
        }
        for id in ids {
            seen.insert(id, now);
        }
        false
    }

    pub fn contains_recent_at(&self, message_id: &str, now: Instant) -> bool {
        self.contains_recent_message(message_id, now)
    }

    fn contains_recent_key(&self, key: &str, now: Instant) -> bool {
        if key.trim().is_empty() {
            return false;
        }
        let mut seen = self
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        seen.retain(|_, first_seen| now.duration_since(*first_seen) <= self.ttl);
        seen.contains_key(key)
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
}
