//! Eviction-tracking LRU cache wrapper
//!
//! Provides O(k) cleanup of secondary indices instead of O(n*m) full scans.
//! When entries are evicted from the LRU cache, their keys are recorded
//! for efficient batch processing without renderer or UI dependencies.

use lru::LruCache;
use std::hash::Hash;
use std::num::NonZeroUsize;

/// LRU cache wrapper that tracks evicted keys for efficient secondary index cleanup.
///
/// Instead of scanning the entire secondary index to find orphaned entries,
/// we record evicted keys as they happen and process them in O(k) time where
/// k = number of evictions since last cleanup (vs O(n*m) full scan).
///
/// # Usage
/// ```ignore
/// let mut cache = EvictionTrackingCache::new(NonZeroUsize::new(100).unwrap());
/// cache.put(key, value);  // May record evictions if at capacity
///
/// // Periodically drain evictions for secondary index cleanup
/// for evicted_key in cache.drain_evictions() {
///     secondary_index.remove(&evicted_key.some_field);
/// }
/// ```
pub struct EvictionTrackingCache<K, V>
where
    K: Hash + Eq + Clone,
{
    /// Underlying LRU cache
    inner: LruCache<K, V>,
    /// Keys evicted since last drain (bounded by cache capacity)
    /// Pre-allocated to cache capacity to avoid reallocation
    evicted_keys: Vec<K>,
}

impl<K, V> EvictionTrackingCache<K, V>
where
    K: Hash + Eq + Clone,
{
    /// Create a new eviction-tracking cache with the given capacity
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: LruCache::new(capacity),
            // Small initial allocation — evictions trickle in one at a time.
            // Vec grows on demand if a burst occurs (rare).
            evicted_keys: Vec::with_capacity(capacity.get().min(64)),
        }
    }

    /// Put a key-value pair into the cache, tracking any eviction
    ///
    /// O(1) amortized - same as LruCache::put() plus potential vec push
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        // Check if we're at capacity and will evict
        if self.inner.len() >= self.inner.cap().get() {
            // Peek at LRU entry before it gets evicted
            if let Some((lru_key, _)) = self.inner.peek_lru() {
                // Only track if we're actually evicting (key doesn't exist)
                if !self.inner.contains(&key) {
                    self.evicted_keys.push(lru_key.clone());
                }
            }
        }
        self.inner.put(key, value)
    }

    /// Get a reference to a value, promoting it to most recently used
    #[inline]
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    /// Peek at a value without affecting LRU order
    #[inline]
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.inner.peek(key)
    }

    /// Clear all entries, recording all keys as evicted
    pub fn clear(&mut self) {
        // Record all keys as evicted before clearing
        for (key, _) in self.inner.iter() {
            self.evicted_keys.push(key.clone());
        }
        self.inner.clear();
    }

    /// Remove a specific key from the cache, recording it as evicted
    ///
    /// Returns the value if the key was present
    pub fn pop(&mut self, key: &K) -> Option<V> {
        match self.inner.pop(key) {
            Some(value) => {
                self.evicted_keys.push(key.clone());
                Some(value)
            }
            _ => None,
        }
    }

    /// Drain evicted keys since last drain
    ///
    /// O(k) where k = evictions since last call
    /// Returns an iterator over evicted keys, clearing the internal buffer
    #[inline]
    pub fn drain_evictions(&mut self) -> impl Iterator<Item = K> + '_ {
        self.evicted_keys.drain(..)
    }

    /// Get the number of entries in the cache
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Iterate the cache keys without affecting LRU order.
    ///
    /// Used only for lightweight telemetry probes (e.g. sampling how many local
    /// keys are absent from the global cache); do not use on the hot path.
    #[inline]
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.inner.iter().map(|(key, _)| key)
    }

    /// Whether the cache is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Resize the cache capacity
    ///
    /// When reducing capacity, evicts LRU entries and records them for cleanup
    /// When increasing capacity, no evictions occur
    pub fn resize(&mut self, new_capacity: NonZeroUsize) {
        // If shrinking, record evicted entries
        let current_size = self.inner.len();
        let new_size = new_capacity.get();

        if current_size > new_size {
            // Shrinking: need to evict (current_size - new_size) entries
            let evict_count = current_size - new_size;

            // Peek and record LRU entries that will be evicted
            let mut to_evict = Vec::with_capacity(evict_count);
            for _ in 0..evict_count {
                if let Some((key, _)) = self.inner.peek_lru() {
                    to_evict.push(key.clone());
                    // Pop the LRU entry (this removes it)
                    self.inner.pop_lru();
                }
            }

            // Record evictions for secondary index cleanup
            self.evicted_keys.extend(to_evict);
        }

        // Resize the underlying LRU cache
        self.inner.resize(new_capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eviction_tracking() {
        let mut cache: EvictionTrackingCache<i32, &str> =
            EvictionTrackingCache::new(NonZeroUsize::new(3).unwrap());

        // Fill cache
        cache.put(1, "a");
        cache.put(2, "b");
        cache.put(3, "c");

        // This should evict key 1 (LRU)
        cache.put(4, "d");

        // Drain and verify
        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert_eq!(evicted, vec![1]);
    }

    #[test]
    fn test_no_eviction_on_update() {
        let mut cache: EvictionTrackingCache<i32, &str> =
            EvictionTrackingCache::new(NonZeroUsize::new(3).unwrap());

        cache.put(1, "a");
        cache.put(2, "b");
        cache.put(3, "c");

        // Update existing key - should NOT evict
        cache.put(1, "updated");

        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert!(evicted.is_empty());
        assert_eq!(cache.get(&1), Some(&"updated"));
    }

    #[test]
    fn test_clear_records_all_evictions() {
        let mut cache: EvictionTrackingCache<i32, &str> =
            EvictionTrackingCache::new(NonZeroUsize::new(3).unwrap());

        cache.put(1, "a");
        cache.put(2, "b");

        cache.clear();

        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert_eq!(evicted.len(), 2);
        assert!(evicted.contains(&1));
        assert!(evicted.contains(&2));
    }

    #[test]
    fn test_multiple_evictions() {
        let mut cache: EvictionTrackingCache<i32, &str> =
            EvictionTrackingCache::new(NonZeroUsize::new(2).unwrap());

        cache.put(1, "a");
        cache.put(2, "b");
        cache.put(3, "c"); // evicts 1
        cache.put(4, "d"); // evicts 2

        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert_eq!(evicted, vec![1, 2]);
    }

    #[test]
    fn test_pop_records_eviction() {
        let mut cache: EvictionTrackingCache<i32, &str> =
            EvictionTrackingCache::new(NonZeroUsize::new(3).unwrap());

        cache.put(1, "a");
        cache.put(2, "b");

        // Explicit pop should record eviction
        let value = cache.pop(&1);
        assert_eq!(value, Some("a"));

        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert_eq!(evicted, vec![1]);

        // Pop non-existent key should not record eviction
        let value = cache.pop(&99);
        assert_eq!(value, None);
        let evicted: Vec<_> = cache.drain_evictions().collect();
        assert!(evicted.is_empty());
    }
}
