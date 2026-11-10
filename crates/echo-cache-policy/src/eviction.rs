//! Unified LRU-LFU hybrid eviction policy
//!
//! Provides shared eviction candidate selection for cache owners without UI dependencies.

use std::time::{Duration, Instant};

/// Trait for cache entries that can be evicted
pub trait Evictable {
    /// Get the entry's size in bytes
    fn size_bytes(&self) -> usize;
    /// Get the entry's access count
    fn access_count(&self) -> u64;
    /// Time since the entry's last access
    fn age(&self) -> Duration;
    /// Whether this entry should be protected from eviction
    fn is_protected(&self) -> bool {
        false
    }
}

/// LRU-LFU hybrid eviction policy
///
/// Score formula: ln(access_count + 1) / (age_hours + 1)
/// Higher score = keep longer, lower score = evict first
#[derive(Debug, Clone)]
pub struct EvictionPolicy {
    /// Weight for frequency component (default: 1.0)
    frequency_weight: f64,
    /// Weight for recency component (default: 1.0)
    recency_weight: f64,
    /// Size penalty factor (larger entries scored lower)
    size_penalty: bool,
    /// Protection threshold (entries with more accesses are protected)
    protection_threshold: u64,
}

impl Default for EvictionPolicy {
    fn default() -> Self {
        Self {
            frequency_weight: 1.0,
            recency_weight: 1.0,
            size_penalty: true,
            protection_threshold: 100,
        }
    }
}

impl EvictionPolicy {
    /// Create a new eviction policy with default settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable the size penalty.
    pub fn with_size_penalty(mut self, enabled: bool) -> Self {
        self.size_penalty = enabled;
        self
    }

    /// Set the access-count protection threshold.
    pub fn with_protection_threshold(mut self, threshold: u64) -> Self {
        self.protection_threshold = threshold;
        self
    }

    /// Calculate eviction score for an entry
    ///
    /// Higher score = keep longer, lower score = evict first
    #[inline]
    pub fn score<E: Evictable>(&self, entry: &E) -> f64 {
        if entry.is_protected() || entry.access_count() >= self.protection_threshold {
            return f64::MAX;
        }

        let access_count = entry.access_count() as f64;
        let age_hours = entry.age().as_secs_f64() / 3600.0;

        // Frequency component: ln(access_count + 1)
        let frequency = (access_count + 1.0).ln() * self.frequency_weight;

        // Recency component: 1 / (age_hours + 1)
        let recency = self.recency_weight / (age_hours + 1.0);

        let mut score = frequency * recency;

        // Size penalty: larger entries get lower scores
        if self.size_penalty {
            let size_kb = entry.size_bytes() as f64 / 1024.0;
            let size_factor = if size_kb < 4.0 {
                1.0
            } else if size_kb < 16.0 {
                0.9
            } else if size_kb < 64.0 {
                0.7
            } else {
                0.5
            };
            score *= size_factor;
        }

        score
    }

    /// Get eviction candidates sorted by score (lowest first)
    ///
    /// Returns entries that should be evicted to free `bytes_needed`.
    /// Uses probabilistic sampling for large caches to avoid O(n log n) sort.
    pub fn get_candidates<E: Evictable + Clone>(
        &self,
        entries: impl Iterator<Item = E>,
        bytes_needed: usize,
    ) -> Vec<E> {
        let mut scored: Vec<(E, f64)> = entries
            .filter(|e| !e.is_protected() && e.access_count() < self.protection_threshold)
            .map(|e| {
                let score = self.score(&e);
                (e, score)
            })
            .collect();

        // Sort by score ascending (lowest = evict first)
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Select entries until we have enough bytes
        let mut candidates = Vec::new();
        let mut freed = 0usize;

        for (entry, _score) in scored {
            if freed >= bytes_needed {
                break;
            }
            freed += entry.size_bytes();
            candidates.push(entry);
        }

        candidates
    }

    /// Probabilistic eviction for large caches
    ///
    /// Samples a random subset of entries instead of sorting all.
    /// More efficient for caches with 10,000+ entries.
    pub fn sample_candidates<E: Evictable + Clone>(
        &self,
        entries: Vec<E>,
        sample_percent: f64,
        bytes_needed: usize,
    ) -> Vec<E> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let sample_size = ((entries.len() as f64 * sample_percent / 100.0) as usize).max(10);

        // Deterministic sampling based on current time
        let now = Instant::now();
        let mut hasher = DefaultHasher::new();
        now.elapsed().as_nanos().hash(&mut hasher);
        let seed = hasher.finish() as usize;

        // Sample entries
        let sampled: Vec<E> = entries
            .into_iter()
            .enumerate()
            .filter(|(i, _)| (i.wrapping_add(seed)) % 100 < (sample_percent as usize))
            .map(|(_, e)| e)
            .take(sample_size * 2) // Take more than needed to ensure we have enough
            .collect();

        self.get_candidates(sampled.into_iter(), bytes_needed)
    }
}

/// Entry wrapper for scoring (used when entry doesn't impl Evictable directly)
#[derive(Clone)]
pub struct EvictableEntry {
    pub key: u64,
    pub size_bytes: usize,
    pub access_count: u64,
    pub age: Duration,
    pub protected: bool,
}

impl EvictableEntry {
    pub fn new(key: u64, size_bytes: usize, access_count: u64, age: Duration) -> Self {
        Self {
            key,
            size_bytes,
            access_count,
            age,
            protected: false,
        }
    }
}

impl Evictable for EvictableEntry {
    fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    fn access_count(&self) -> u64 {
        self.access_count
    }

    fn age(&self) -> Duration {
        self.age
    }

    fn is_protected(&self) -> bool {
        self.protected
    }
}

/// Ring buffer for deferred access tracking
///
/// Instead of updating access time on every read (which requires a write lock),
/// we buffer access records and flush them periodically.
pub struct AccessBuffer {
    buffer: parking_lot::Mutex<Vec<u64>>,
    capacity: usize,
}

impl AccessBuffer {
    /// Create a new access buffer
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: parking_lot::Mutex::new(Vec::with_capacity(capacity)),
            capacity,
        }
    }

    /// Record an access (lock-free fast path)
    #[inline]
    pub fn push(&self, key: u64) {
        if let Some(mut guard) = self.buffer.try_lock()
            && guard.len() < self.capacity
        {
            guard.push(key);
        }
        // If full, just drop the access record (acceptable for LRU approximation)
    }

    pub fn drain(&self) -> Vec<u64> {
        let mut guard = self.buffer.lock();
        std::mem::take(&mut *guard)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.buffer.lock().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }
}

impl Default for AccessBuffer {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_calculation() {
        let policy = EvictionPolicy::new();

        // Recent, frequently accessed = high score
        let entry1 = EvictableEntry::new(1, 1000, 100, Duration::ZERO);
        let score1 = policy.score(&entry1);

        // Old, rarely accessed = low score
        let entry2 = EvictableEntry::new(2, 1000, 1, Duration::from_secs(3600));
        let score2 = policy.score(&entry2);

        assert!(score1 > score2);
    }

    #[test]
    fn test_size_penalty() {
        let policy = EvictionPolicy::new().with_size_penalty(true);

        // Same access pattern, different sizes
        let small = EvictableEntry::new(1, 1000, 10, Duration::ZERO);
        let large = EvictableEntry::new(2, 100_000, 10, Duration::ZERO);

        let score_small = policy.score(&small);
        let score_large = policy.score(&large);

        // Smaller entry should have higher score (kept longer)
        assert!(score_small > score_large);
    }

    #[test]
    fn test_get_candidates() {
        let policy = EvictionPolicy::new().with_size_penalty(false);

        let entries = vec![
            EvictableEntry::new(1, 100, 50, Duration::ZERO), // High frequency
            EvictableEntry::new(2, 100, 1, Duration::from_secs(7200)), // Low freq, old
            EvictableEntry::new(3, 100, 5, Duration::from_secs(3600)), // Medium
        ];

        let candidates = policy.get_candidates(entries.into_iter(), 150);

        // Should evict entries 2 and 3 (lowest scores)
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|e| e.key == 2));
    }

    #[test]
    fn test_access_buffer() {
        let buffer = AccessBuffer::new(10);

        for i in 0..5 {
            buffer.push(i);
        }

        assert_eq!(buffer.len(), 5);
        assert!(!buffer.is_full());

        let drained = buffer.drain();
        assert_eq!(drained.len(), 5);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_protected_entries() {
        let policy = EvictionPolicy::new().with_protection_threshold(50);

        let protected = EvictableEntry {
            key: 1,
            size_bytes: 100,
            access_count: 100, // Above threshold
            age: Duration::from_secs(7200),
            protected: false,
        };

        // Protected entry should have max score
        assert_eq!(policy.score(&protected), f64::MAX);
    }
}
