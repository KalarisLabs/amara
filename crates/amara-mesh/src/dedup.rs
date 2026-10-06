use amara_core::peer::PeerId;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use amara_core::constants::DEDUP_CACHE_SIZE;

/// Deduplication cache to prevent processing the same packet twice.
/// Keyed by (source_id, message_id).
pub struct DedupCache {
    cache: Mutex<LruCache<(PeerId, [u8; 8]), ()>>,
}

impl DedupCache {
    pub fn new() -> Self {
        let capacity = NonZeroUsize::new(DEDUP_CACHE_SIZE).unwrap();
        Self {
            cache: Mutex::new(LruCache::new(capacity)),
        }
    }

    /// Check if a message has been seen before. If not, adds it to the cache.
    /// Returns true if the message is a duplicate.
    pub fn is_duplicate(&self, source: PeerId, message_id: [u8; 8]) -> bool {
        let key = (source, message_id);
        let mut cache = self.cache.lock().unwrap();
        
        if cache.contains(&key) {
            true
        } else {
            cache.put(key, ());
            false
        }
    }
}

impl Default for DedupCache {
    fn default() -> Self {
        Self::new()
    }
}
