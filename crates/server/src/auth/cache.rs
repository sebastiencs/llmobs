//! Hash lookup cache (D140).
//!
//! A hit lives 60 s from insertion and is not refreshed on read. Misses are not
//! stored. At 10 000 entries the oldest insertion is dropped.

use std::collections::HashMap;
use std::sync::Mutex;

/// How long a cached lookup is reused.
const TTL_NS: i64 = 60 * 1_000_000_000;

/// Cap on stored hits.
const MAX_ENTRIES: usize = 10_000;

/// Cached successful lookups, keyed by the SHA-256 hex of the credential.
pub(crate) struct KeyCache<T> {
    entries: Mutex<HashMap<String, Entry<T>>>,
}

struct Entry<T> {
    value: T,
    stored_at: i64,
}

impl<T> KeyCache<T> {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry<T>>> {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl<T: Clone> KeyCache<T> {
    /// The value stored for `hash`, when it is still inside the 60 s window.
    pub(crate) fn get(&self, hash: &str, now: i64) -> Option<T> {
        let mut entries = self.lock();
        let expired = entries
            .get(hash)
            .is_some_and(|entry| now >= entry.stored_at.saturating_add(TTL_NS));
        if expired {
            entries.remove(hash);
            return None;
        }
        entries.get(hash).map(|entry| entry.value.clone())
    }

    /// Stores `value` under `hash`. A full cache drops the oldest entry first.
    pub(crate) fn put(&self, hash: String, value: T, now: i64) {
        let mut entries = self.lock();
        if !entries.contains_key(&hash) && entries.len() >= MAX_ENTRIES {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            hash,
            Entry {
                value,
                stored_at: now,
            },
        );
    }

    /// Drops `hash` if it is cached.
    pub(crate) fn evict(&self, hash: &str) {
        self.lock().remove(hash);
    }
}

#[cfg(test)]
mod tests {
    use super::KeyCache;

    #[test]
    fn a_hit_expires_without_a_refresh_and_a_miss_is_not_stored() {
        let cache = KeyCache::new();
        assert!(cache.get("missing", 0).is_none());
        cache.put("hash".to_owned(), 7, 0);
        assert_eq!(cache.get("hash", 60_000_000_000 - 1), Some(7));
        assert_eq!(cache.get("hash", 60_000_000_000), None);
        assert!(cache.get("missing", 1).is_none());
    }

    #[test]
    fn evict_drops_one_entry() {
        let cache = KeyCache::new();
        cache.put("hash".to_owned(), 1, 0);
        cache.evict("hash");
        assert!(cache.get("hash", 0).is_none());
    }
}
