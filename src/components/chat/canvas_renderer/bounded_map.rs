//! Bounded concurrent memo tables for values that are cheap to recompute.

use dashmap::DashMap;
use std::hash::Hash;

/// A concurrent memo table that empties itself once it holds `capacity`
/// entries.
///
/// Meant for caches whose values are cheap to recompute (parsed colors,
/// formatted timestamps, resolved badge URLs): refilling after an occasional
/// reset costs less than tracking recency on every hit. `DashMap` shards its
/// locks, so readers on different shards do not contend.
pub(crate) struct ClearOnFullMap<K, V> {
    map: DashMap<K, V>,
    capacity: usize,
}

impl<K: Eq + Hash, V: Clone> ClearOnFullMap<K, V> {
    /// A table that resets at `capacity` entries, with room for
    /// `initial_capacity` before its first reallocation.
    pub(crate) fn new(initial_capacity: usize, capacity: usize) -> Self {
        Self {
            map: DashMap::with_capacity(initial_capacity),
            capacity,
        }
    }

    #[inline]
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        self.map.get(key).map(|value| value.clone())
    }

    /// Store `value`, first emptying the table if it is full.
    pub(crate) fn insert(&self, key: K, value: V) {
        if self.map.len() >= self.capacity {
            self.map.clear();
        }
        self.map.insert(key, value);
    }

    pub(crate) fn clear(&self) {
        self.map.clear();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::ClearOnFullMap;
    use core::prelude::v1::test;

    #[test]
    fn resets_when_capacity_is_reached() {
        let map = ClearOnFullMap::new(4, 3);
        for key in 0..3 {
            map.insert(key, key * 10);
        }
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&2), Some(20));

        map.insert(3, 30);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&0), None);
        assert_eq!(map.get(&3), Some(30));
    }
}
