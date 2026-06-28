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
        self.check_and_insert(message_id, Instant::now())
    }

    pub fn contains_recent(&self, message_id: &str) -> bool {
        self.contains_recent_at(message_id, Instant::now())
    }

    pub fn contains_recent_at(&self, message_id: &str, now: Instant) -> bool {
        if message_id.trim().is_empty() {
            return false;
        }
        let mut seen = self
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        seen.retain(|_, first_seen| now.duration_since(*first_seen) <= self.ttl);
        seen.contains_key(message_id)
    }

    pub fn check_and_insert(&self, message_id: &str, now: Instant) -> bool {
        if message_id.trim().is_empty() {
            return false;
        }

        let mut seen = self
            .seen
            .lock()
            .expect("dedupe lock should not be poisoned");
        seen.retain(|_, first_seen| now.duration_since(*first_seen) <= self.ttl);
        if seen.contains_key(message_id) {
            return true;
        }
        seen.insert(message_id.to_owned(), now);
        false
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
}
