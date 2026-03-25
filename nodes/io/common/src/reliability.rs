#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::io_context::IoContext;

#[derive(Debug)]
pub struct DedupCache {
    ttl: Duration,
    max_entries: usize,
    entries: HashMap<String, Instant>,
    expirations: VecDeque<(Instant, String)>,
}

impl DedupCache {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries: max_entries.max(1),
            entries: HashMap::new(),
            expirations: VecDeque::new(),
        }
    }

    pub fn is_duplicate_and_mark(&mut self, key: &str) -> bool {
        self.is_duplicate_and_mark_at(key, Instant::now())
    }

    fn is_duplicate_and_mark_at(&mut self, key: &str, now: Instant) -> bool {
        self.evict_expired(now);

        if let Some(expires_at) = self.entries.get(key) {
            if *expires_at > now {
                return true;
            }
        }

        let expires_at = now + self.ttl;
        self.entries.insert(key.to_string(), expires_at);
        self.expirations.push_back((expires_at, key.to_string()));
        self.enforce_max_entries();
        false
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some((expires_at, _key)) = self.expirations.front() {
            if *expires_at > now {
                break;
            }
            let (expires_at, key) = self.expirations.pop_front().unwrap();
            if self.entries.get(&key) == Some(&expires_at) {
                self.entries.remove(&key);
            }
        }
    }

    fn enforce_max_entries(&mut self) {
        while self.entries.len() > self.max_entries {
            let Some((expires_at, key)) = self.expirations.pop_front() else {
                break;
            };
            if self.entries.get(&key) == Some(&expires_at) {
                self.entries.remove(&key);
            }
        }
    }
}

/// Canonical best-effort dedup key for inbound IO messages.
///
/// Follows the general convention `(channel, external_conversation_id, external_message_id)`.
pub fn dedup_key(channel: &str, conversation_id: &str, message_id: &str) -> String {
    format!("{channel}:{conversation_id}:{message_id}")
}

pub fn dedup_key_from_io_context(io: &IoContext) -> String {
    dedup_key(&io.channel, &io.conversation.id, &io.message.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_rejects_within_ttl() {
        let mut cache = DedupCache::new(Duration::from_secs(10), 100);
        let now = Instant::now();
        assert!(!cache.is_duplicate_and_mark_at("k1", now));
        assert!(cache.is_duplicate_and_mark_at("k1", now + Duration::from_millis(1)));
    }

    #[test]
    fn dedup_allows_after_ttl() {
        let mut cache = DedupCache::new(Duration::from_millis(10), 100);
        let now = Instant::now();
        assert!(!cache.is_duplicate_and_mark_at("k1", now));
        assert!(!cache.is_duplicate_and_mark_at("k1", now + Duration::from_millis(11)));
    }

    #[test]
    fn dedup_enforces_max_entries() {
        let mut cache = DedupCache::new(Duration::from_secs(60), 2);
        let now = Instant::now();
        assert!(!cache.is_duplicate_and_mark_at("a", now));
        assert!(!cache.is_duplicate_and_mark_at("b", now));
        assert!(!cache.is_duplicate_and_mark_at("c", now)); // should evict oldest
        assert!(!cache.is_duplicate_and_mark_at("a", now)); // "a" was evicted
    }
}
