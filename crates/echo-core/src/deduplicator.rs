//! Message deduplication utility.
//!
//! Provides Set-based deduplication with automatic pruning
//! to prevent unbounded memory growth.

use std::collections::{HashSet, VecDeque};

/// Message deduplicator using a Set with automatic pruning.
///
/// Tracks message IDs and automatically prunes old entries when the
/// maximum size is exceeded to prevent unbounded memory growth.
pub struct MessageDeduplicator {
    seen: HashSet<String>,
    order: VecDeque<String>,
    max_size: usize,
    prune_threshold: usize,
}

impl MessageDeduplicator {
    /// Create a new deduplicator with default prune ratio of 0.8.
    ///
    /// # Arguments
    /// * `max_size` - Maximum number of IDs to track (default 10000)
    pub fn new(max_size: usize) -> Self {
        Self::with_prune_ratio(max_size, 0.8)
    }

    /// Create a new deduplicator with custom prune ratio.
    ///
    /// # Arguments
    /// * `max_size` - Maximum number of IDs to track (default 10000)
    /// * `prune_ratio` - Ratio to keep when pruning (default 0.8)
    pub fn with_prune_ratio(max_size: usize, prune_ratio: f32) -> Self {
        Self {
            seen: HashSet::with_capacity(max_size),
            order: VecDeque::with_capacity(max_size),
            max_size,
            prune_threshold: (max_size as f32 * prune_ratio) as usize,
        }
    }

    /// Add an ID to the set. Automatically prunes if size exceeds max_size.
    pub fn add(&mut self, id: String) {
        if !self.seen.contains(&id) {
            self.seen.insert(id.clone());
            self.order.push_back(id);

            if self.seen.len() > self.max_size {
                self.prune();
            }
        }
    }

    /// Check and add in one operation.
    /// Returns true if the ID was new (added), false if already seen.
    pub fn check_and_add(&mut self, id: String) -> bool {
        if self.seen.contains(&id) {
            return false;
        }
        self.add(id);
        true
    }

    /// Insert an ID and return whether it was new.
    /// Alias for check_and_add for DashMap usage clarity.
    pub fn insert(&mut self, id: String) -> bool {
        self.check_and_add(id)
    }

    /// Prune the set to prune_threshold size, keeping most recently added IDs
    fn prune(&mut self) {
        // Remove oldest entries until we're at the threshold
        while self.seen.len() > self.prune_threshold {
            if let Some(old_id) = self.order.pop_front() {
                self.seen.remove(&old_id);
            } else {
                break;
            }
        }

        // Clean up order queue - remove entries that are no longer in seen
        self.order.retain(|id| self.seen.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_deduplicator_basic() {
        let mut dedup = MessageDeduplicator::with_prune_ratio(100, 0.8);

        assert!(dedup.check_and_add("msg1".to_string()));
        assert!(!dedup.check_and_add("msg1".to_string())); // Duplicate
        assert!(dedup.check_and_add("msg2".to_string()));

        assert!(dedup.seen.contains("msg1"));
        assert!(dedup.seen.contains("msg2"));
        assert!(!dedup.seen.contains("msg3"));
        assert_eq!(dedup.seen.len(), 2);
    }

    #[test]
    fn test_message_deduplicator_pruning() {
        let mut dedup = MessageDeduplicator::with_prune_ratio(10, 0.5);

        // Add 15 messages
        for i in 0..15 {
            dedup.add(format!("msg{}", i));
        }

        // Should have pruned to around 5-10 messages
        assert!(dedup.seen.len() <= 10);
        // Most recent messages should still be present
        assert!(dedup.seen.contains("msg14"));
    }
}
