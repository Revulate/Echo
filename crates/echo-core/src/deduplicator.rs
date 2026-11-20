//! Message deduplication utility.
//!
//! Provides Set-based deduplication with automatic pruning
//! to prevent unbounded memory growth.

use std::collections::{HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

/// Message deduplicator using a Set with automatic pruning.
///
/// Tracks message IDs and automatically prunes old entries when the
/// maximum size is exceeded to prevent unbounded memory growth.
///
/// IDs are stored as 128-bit keys (see [`id_key`]) rather than strings, and
/// the set grows on demand instead of reserving `max_size` up front.
pub struct MessageDeduplicator {
    seen: HashSet<u128>,
    order: VecDeque<u128>,
    max_size: usize,
    prune_threshold: usize,
}

/// The stored key for a message ID.
///
/// A UUID in canonical form (36 bytes, lowercase hex, hyphens at 8, 13, 18 and
/// 23), which is how Twitch writes chat message and EventSub delivery IDs, maps
/// to its 128-bit value. That mapping is one-to-one on canonical strings, so
/// two such IDs share a key only when they are the same string. Every other ID,
/// including a UUID with uppercase digits, gets two independent 64-bit SipHash
/// values (std `DefaultHasher`, whose fixed keys make the result stable within
/// a process). A false duplicate therefore needs a 128-bit collision between a
/// hashed ID and one of the at most `max_size + 1` keys live in the same set:
/// with 10,001 live keys that is below 10,001^2 / 2^128, about 3e-31, per set.
fn id_key(id: &str) -> u128 {
    if let Some(value) = canonical_uuid_value(id) {
        return value;
    }
    let mut low = DefaultHasher::new();
    id.hash(&mut low);
    let mut high = DefaultHasher::new();
    0x9Eu8.hash(&mut high);
    id.hash(&mut high);
    (u128::from(high.finish()) << 64) | u128::from(low.finish())
}

fn canonical_uuid_value(id: &str) -> Option<u128> {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    let mut value = 0u128;
    for (index, &byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return None;
            }
            continue;
        }
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return None,
        };
        value = (value << 4) | u128::from(digit);
    }
    Some(value)
}

impl MessageDeduplicator {
    /// Heap bytes the key set and the order queue have allocated, for memory
    /// reports: their capacity, with one control byte per set slot.
    pub fn heap_bytes(&self) -> usize {
        let key = std::mem::size_of::<u128>();
        self.seen.capacity() * (key + 1) + self.order.capacity() * key
    }

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
            seen: HashSet::new(),
            order: VecDeque::new(),
            max_size,
            prune_threshold: (max_size as f32 * prune_ratio) as usize,
        }
    }

    /// Add an ID to the set. Automatically prunes if size exceeds max_size.
    pub fn add(&mut self, id: &str) {
        self.add_key(id_key(id));
    }

    fn add_key(&mut self, key: u128) {
        if self.seen.insert(key) {
            self.order.push_back(key);

            if self.seen.len() > self.max_size {
                self.prune();
            }
        }
    }

    /// Check and add in one operation.
    /// Returns true if the ID was new (added), false if already seen.
    pub fn check_and_add(&mut self, id: &str) -> bool {
        let key = id_key(id);
        if self.seen.contains(&key) {
            return false;
        }
        self.add_key(key);
        true
    }

    /// Insert an ID and return whether it was new.
    /// Alias for check_and_add for DashMap usage clarity.
    pub fn insert(&mut self, id: &str) -> bool {
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

    fn seen(dedup: &MessageDeduplicator, id: &str) -> bool {
        dedup.seen.contains(&id_key(id))
    }

    /// The string-keyed deduplicator this module replaced, kept as the
    /// reference for the observable behaviour.
    struct StringDeduplicator {
        seen: HashSet<String>,
        order: VecDeque<String>,
        max_size: usize,
        prune_threshold: usize,
    }

    impl StringDeduplicator {
        fn with_prune_ratio(max_size: usize, prune_ratio: f32) -> Self {
            Self {
                seen: HashSet::new(),
                order: VecDeque::new(),
                max_size,
                prune_threshold: (max_size as f32 * prune_ratio) as usize,
            }
        }

        fn check_and_add(&mut self, id: String) -> bool {
            if self.seen.contains(&id) {
                return false;
            }
            self.seen.insert(id.clone());
            self.order.push_back(id);
            if self.seen.len() > self.max_size {
                while self.seen.len() > self.prune_threshold {
                    if let Some(old_id) = self.order.pop_front() {
                        self.seen.remove(&old_id);
                    } else {
                        break;
                    }
                }
                self.order.retain(|id| self.seen.contains(id));
            }
            true
        }
    }

    fn uuid_like(n: u32) -> String {
        format!("{n:08x}-4977-403a-8a94-33c6bac34fb8")
    }

    #[test]
    fn test_message_deduplicator_basic() {
        let mut dedup = MessageDeduplicator::with_prune_ratio(100, 0.8);

        assert!(dedup.check_and_add("msg1"));
        assert!(!dedup.check_and_add("msg1")); // Duplicate
        assert!(dedup.check_and_add("msg2"));

        assert!(seen(&dedup, "msg1"));
        assert!(seen(&dedup, "msg2"));
        assert!(!seen(&dedup, "msg3"));
        assert_eq!(dedup.seen.len(), 2);
    }

    #[test]
    fn test_message_deduplicator_pruning() {
        let mut dedup = MessageDeduplicator::with_prune_ratio(10, 0.5);

        // Add 15 messages
        for i in 0..15 {
            dedup.add(&format!("msg{}", i));
        }

        // Should have pruned to around 5-10 messages
        assert!(dedup.seen.len() <= 10);
        // Most recent messages should still be present
        assert!(seen(&dedup, "msg14"));
    }

    #[test]
    fn canonical_uuids_key_by_value_and_other_ids_stay_distinct() {
        let id = "b34ccfc7-4977-403a-8a94-33c6bac34fb8";
        assert_eq!(id_key(id), 0xb34ccfc7_4977_403a_8a94_33c6bac34fb8);
        assert_eq!(id_key("00000000-0000-0000-0000-000000000000"), 0);

        // Strings that are not canonical UUIDs are hashed, so the uppercase
        // spelling, the unhyphenated form and a misplaced hyphen all stay
        // distinct IDs, exactly as they were as strings.
        let variants = [
            "B34CCFC7-4977-403A-8A94-33C6BAC34FB8",
            "b34ccfc74977403a8a9433c6bac34fb8",
            "b34ccfc7-4977-403a-8a9-433c6bac34fb8",
            "{b34ccfc7-4977-403a-8a94-33c6bac34fb8}",
            "",
            "msg1",
        ];
        let mut dedup = MessageDeduplicator::new(100);
        assert!(dedup.check_and_add(id));
        for variant in variants {
            assert_eq!(canonical_uuid_value(variant), None, "{variant}");
            assert!(dedup.check_and_add(variant), "{variant}");
            assert!(!dedup.check_and_add(variant), "{variant}");
        }
        assert!(!dedup.check_and_add(id));
    }

    #[test]
    fn nothing_is_reserved_until_ids_arrive() {
        let dedup = MessageDeduplicator::new(10_000);
        assert_eq!(dedup.seen.capacity(), 0);
        assert_eq!(dedup.order.capacity(), 0);
    }

    #[test]
    fn prune_drops_the_oldest_ids_down_to_the_threshold() {
        let mut dedup = MessageDeduplicator::with_prune_ratio(10, 0.5);
        for n in 0..=10 {
            assert!(dedup.check_and_add(&uuid_like(n)));
        }
        // The eleventh ID crosses max_size, so the set prunes to 5, keeping
        // the most recent ones in insertion order.
        assert_eq!(dedup.seen.len(), 5);
        let kept: Vec<u128> = dedup.order.iter().copied().collect();
        let expected: Vec<u128> = (6..=10).map(|n| id_key(&uuid_like(n))).collect();
        assert_eq!(kept, expected);
        for n in 6..=10 {
            assert!(!dedup.check_and_add(&uuid_like(n)));
        }
        assert!(
            dedup.check_and_add(&uuid_like(0)),
            "a pruned ID is new again"
        );
    }

    #[test]
    fn matches_the_string_deduplicator_on_mixed_ids() {
        let mut compact = MessageDeduplicator::with_prune_ratio(64, 0.8);
        let mut reference = StringDeduplicator::with_prune_ratio(64, 0.8);
        let mut state = 0x2545_F491_u32;
        for step in 0..5_000u32 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let n = state % 200;
            let id = match step % 4 {
                0 => format!("local-{n}"),
                1 => uuid_like(n).to_ascii_uppercase(),
                _ => uuid_like(n),
            };
            assert_eq!(
                compact.check_and_add(&id),
                reference.check_and_add(id.clone()),
                "step {step} id {id}"
            );
            assert_eq!(compact.seen.len(), reference.seen.len());
            assert!(compact.seen.len() <= 64);
        }
        let reference_order: Vec<u128> = reference.order.iter().map(|id| id_key(id)).collect();
        assert_eq!(
            compact.order.iter().copied().collect::<Vec<_>>(),
            reference_order
        );
    }
}
