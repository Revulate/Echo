//! Height-indexed tree for O(log n) message position lookups
//!
//! Uses a Fenwick tree (Binary Indexed Tree) for O(log n) updates and prefix queries.
//! Optimized for 1,000-10,000 message chat scenarios without UI dependencies.

use std::collections::VecDeque;

/// Fenwick tree (Binary Indexed Tree) for efficient prefix sums
/// Supports O(log n) point updates and O(log n) prefix queries
#[derive(Debug, Clone)]
struct FenwickTree {
    /// 1-indexed tree storage
    tree: Vec<f32>,
}

impl FenwickTree {
    /// Create a new Fenwick tree with given capacity
    fn new(capacity: usize) -> Self {
        Self {
            tree: vec![0.0; capacity + 1],
        }
    }

    /// Add delta to element at index (0-indexed input)
    /// O(log n) - updates all affected nodes in the tree
    #[inline]
    fn update(&mut self, idx: usize, delta: f32) {
        let mut i = idx + 1; // Convert to 1-indexed
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i.isolate_lowest_one();
        }
    }

    /// Get prefix sum [0, idx] (0-indexed input, inclusive)
    /// O(log n) - traverses tree structure
    #[inline]
    fn prefix_sum(&self, idx: usize) -> f32 {
        let mut i = idx + 1; // Convert to 1-indexed
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i &= i - 1; // i -= lowbit(i), equivalent to i -= i & i.wrapping_neg()
        }
        sum
    }

    /// Get sum of range [left, right] (0-indexed, inclusive)
    #[inline]
    fn range_sum(&self, left: usize, right: usize) -> f32 {
        if left == 0 {
            self.prefix_sum(right)
        } else {
            self.prefix_sum(right) - self.prefix_sum(left - 1)
        }
    }

    /// Clear all values
    fn clear(&mut self) {
        self.tree.fill(0.0);
    }
}

/// Height tree for efficient message position queries
///
/// Maintains heights via Fenwick tree to enable:
/// - O(log n) y-position lookup by index
/// - O(log n) index lookup by y-coordinate
/// - O(log n) height update at any index
/// - O(1) amortized buffer rotation (via VecDeque + offset tracking)
///
/// Uses Fenwick tree for O(log n) updates instead of O(n) prefix sum rebuilds.
/// This is critical for async layout completion which updates heights mid-buffer.
#[derive(Debug, Clone)]
pub struct HeightTree {
    /// Heights of each message (VecDeque for O(1) pop_front)
    heights: VecDeque<f32>,
    /// Fenwick tree for O(log n) prefix sum queries and updates
    fenwick: FenwickTree,
    /// Physical start index in the Fenwick tree (shifts on remove_first)
    start_idx: usize,
    /// Physical capacity of the Fenwick tree
    capacity: usize,
}

/// Default capacity - power of 2 for efficient Fenwick operations
const DEFAULT_CAPACITY: usize = 4096; // Grows automatically via compact_and_expand

impl HeightTree {
    /// Create a new empty height tree
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        // Round up to power of 2 for efficient Fenwick operations
        let capacity = capacity.next_power_of_two().max(16);
        Self {
            heights: VecDeque::with_capacity(capacity),
            fenwick: FenwickTree::new(capacity),
            start_idx: 0,
            capacity,
        }
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.heights.clear();
        self.fenwick.clear();
        self.start_idx = 0;
    }

    /// Reserve capacity for additional heights
    /// Prevents reallocations during incremental appends
    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.heights.reserve(additional);
        // Size by live rows only; the dead prefix is reclaimed by compaction.
        let live = self.heights.len() + additional;
        if self.start_idx + live > self.capacity {
            self.compact_and_expand(live);
        }
    }

    /// Compact the Fenwick tree by moving all elements to start at index 0.
    ///
    /// Capacity only grows when `min_capacity` exceeds half of it, so steady
    /// `remove_first` + `push` rotation compacts in place instead of doubling
    /// forever. A tree left far oversized (e.g. after a large history trim)
    /// shrinks back to twice the live need.
    fn compact_and_expand(&mut self, min_capacity: usize) {
        let target = min_capacity.saturating_mul(2).next_power_of_two().max(16);
        let new_capacity =
            if min_capacity > self.capacity / 2 || target.saturating_mul(4) <= self.capacity {
                target
            } else {
                self.capacity
            };

        if new_capacity == self.capacity {
            self.fenwick.clear();
        } else {
            self.fenwick = FenwickTree::new(new_capacity);
        }

        // Rebuild at index 0
        for (i, &h) in self.heights.iter().enumerate() {
            self.fenwick.update(i, h);
        }

        self.start_idx = 0;
        self.capacity = new_capacity;
    }

    /// Push a new height
    /// O(log n) via Fenwick tree update
    pub fn push(&mut self, height: f32) {
        let physical_idx = self.start_idx + self.heights.len();

        // Check if we need to compact/expand
        if physical_idx >= self.capacity {
            self.compact_and_expand(self.heights.len() + 1);
            // After compaction, physical_idx is now heights.len()
            let new_physical_idx = self.heights.len();
            self.fenwick.update(new_physical_idx, height);
        } else {
            self.fenwick.update(physical_idx, height);
        }

        self.heights.push_back(height);
    }

    /// Prepend heights and rebuild the Fenwick tree.
    ///
    /// History loads add older messages before the current viewport. This path is
    /// intentionally O(n): it happens on history fetches, not per frame, and keeps
    /// scroll compensation straightforward.
    pub fn prepend_many(&mut self, prepended: &[f32]) -> f32 {
        if prepended.is_empty() {
            return 0.0;
        }

        let mut heights = VecDeque::with_capacity(prepended.len() + self.heights.len());
        let mut prepended_total = 0.0;

        for &height in prepended {
            prepended_total += height;
            heights.push_back(height);
        }

        for &height in &self.heights {
            heights.push_back(height);
        }

        let capacity = heights.len().next_power_of_two().max(16).max(self.capacity);
        let mut fenwick = FenwickTree::new(capacity);
        for (idx, &height) in heights.iter().enumerate() {
            fenwick.update(idx, height);
        }

        self.heights = heights;
        self.fenwick = fenwick;
        self.start_idx = 0;
        self.capacity = capacity;

        prepended_total
    }

    /// Remove first element
    /// O(log n) - zeros out the element in Fenwick tree
    pub fn remove_first(&mut self) {
        if self.heights.is_empty() {
            return;
        }

        // Zero out the height in Fenwick tree
        let old_height = self.heights[0];
        self.fenwick.update(self.start_idx, -old_height);

        self.heights.pop_front();
        self.start_idx += 1;

        // Compact if start_idx is getting too large (> half capacity)
        if self.start_idx > self.capacity / 2 && self.heights.len() < self.capacity / 4 {
            self.compact_and_expand(self.heights.len());
        }
    }

    /// Get total height of all messages
    /// O(log n) via Fenwick tree range query
    #[inline]
    pub fn total_height(&self) -> f32 {
        if self.heights.is_empty() {
            return 0.0;
        }
        let end_idx = self.start_idx + self.heights.len() - 1;
        self.fenwick.range_sum(self.start_idx, end_idx)
    }

    /// Get number of messages
    #[inline]
    pub fn len(&self) -> usize {
        self.heights.len()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// Get y-position of message at index (O(log n) via Fenwick tree)
    /// Returns the cumulative height of all messages before this index
    #[inline]
    pub fn y_position(&self, idx: usize) -> f32 {
        if idx == 0 || self.heights.is_empty() {
            return 0.0;
        }
        // A hidden split can rotate rows in before render measures them, so a
        // jump may ask for a row past the measured heights. Clamp instead of
        // reading past the tree.
        let idx = idx.min(self.heights.len());
        // Sum heights [0, idx-1] in logical space = [start_idx, start_idx + idx - 1] in physical space
        let physical_end = self.start_idx + idx - 1;
        self.fenwick.range_sum(self.start_idx, physical_end)
    }

    /// Get height at specific index
    #[inline]
    pub fn height_at(&self, index: usize) -> Option<f32> {
        self.heights.get(index).copied()
    }

    /// Update the height at `index` in O(log n).
    /// Returns true when an update was applied.
    pub fn update(&mut self, index: usize, new_height: f32) -> bool {
        if index >= self.heights.len() {
            return false;
        }

        let old_height = self.heights[index];
        let diff = new_height - old_height;

        // Early return if height unchanged
        if diff.abs() < 0.001 {
            return false;
        }

        // Update height storage
        self.heights[index] = new_height;

        // Update Fenwick tree - O(log n)!
        let physical_idx = self.start_idx + index;
        self.fenwick.update(physical_idx, diff);
        true
    }

    /// Find index of message containing y-coordinate (O(log^2 n) via binary search + prefix query)
    /// Returns the index of the message whose y-range contains the coordinate
    pub fn index_at_y(&self, y: f32) -> usize {
        if y <= 0.0 || self.heights.is_empty() {
            return 0;
        }

        let total = self.total_height();
        if y >= total {
            return self.heights.len().saturating_sub(1);
        }

        // Binary search: find smallest idx where y_position(idx+1) > y
        // Equivalent to: cumulative_height(0..=idx) > y
        let mut lo = 0;
        let mut hi = self.heights.len();

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let cum_height = self.y_position(mid + 1); // Height up to and including mid
            if cum_height <= y {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        lo.min(self.heights.len().saturating_sub(1))
    }

    /// Find visible range of messages for a viewport
    /// Returns (start_idx, end_idx) - exclusive end
    pub fn visible_range(
        &self,
        scroll_offset: f32,
        viewport_height: f32,
        overdraw: f32,
    ) -> (usize, usize) {
        if self.heights.is_empty() {
            return (0, 0);
        }

        let view_top = scroll_offset - overdraw;
        let view_bottom = scroll_offset + viewport_height + overdraw;

        let start_idx = self.index_at_y(view_top);
        let end_idx = (self.index_at_y(view_bottom) + 1).min(self.heights.len());

        (start_idx, end_idx)
    }
}

impl Default for HeightTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y_position_clamps_rows_not_yet_measured() {
        let mut tree = HeightTree::with_capacity(16);
        for _ in 0..16 {
            tree.push(10.0);
        }
        for _ in 0..12 {
            tree.remove_first();
        }
        // The model holds more rows than the tree has heights for, as a hidden
        // split does after rotating rows in without a render.
        assert_eq!(tree.y_position(15), 40.0);
        assert_eq!(tree.y_position(4), 40.0);
        assert_eq!(tree.y_position(2), 20.0);
    }

    fn tree_from_heights(heights: &[f32]) -> HeightTree {
        let mut tree = HeightTree::new();
        for &h in heights {
            tree.push(h);
        }
        tree
    }

    #[test]
    fn test_basic_operations() {
        let tree = tree_from_heights(&[23.0, 46.0, 23.0, 69.0]);

        assert_eq!(tree.len(), 4);
        assert_eq!(tree.total_height(), 161.0);

        assert_eq!(tree.y_position(0), 0.0);
        assert_eq!(tree.y_position(1), 23.0);
        assert_eq!(tree.y_position(2), 69.0);
        assert_eq!(tree.y_position(3), 92.0);
    }

    #[test]
    fn test_index_at_y() {
        let tree = tree_from_heights(&[23.0, 46.0, 23.0, 69.0]);

        // Test points within each message
        assert_eq!(tree.index_at_y(0.0), 0);
        assert_eq!(tree.index_at_y(10.0), 0);
        assert_eq!(tree.index_at_y(23.0), 1);
        assert_eq!(tree.index_at_y(50.0), 1);
        assert_eq!(tree.index_at_y(69.0), 2);
        assert_eq!(tree.index_at_y(92.0), 3);
        assert_eq!(tree.index_at_y(150.0), 3);
    }

    #[test]
    fn test_visible_range() {
        let mut tree = HeightTree::new();
        for _ in 0..100 {
            tree.push(23.0);
        }

        // Viewport at top
        let (start, end) = tree.visible_range(0.0, 100.0, 50.0);
        assert!(start == 0);
        assert!(end <= 10); // ~150px / 23px = ~6-7 messages

        // Viewport in middle
        let (start, end) = tree.visible_range(500.0, 100.0, 50.0);
        assert!(start > 15); // 450px / 23px = ~19
        assert!(end > start);
    }

    #[test]
    fn test_remove_first() {
        let mut tree = tree_from_heights(&[10.0, 20.0, 30.0, 40.0]);

        assert_eq!(tree.total_height(), 100.0);

        tree.remove_first();
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.total_height(), 90.0);
        assert_eq!(tree.y_position(0), 0.0);
        assert_eq!(tree.y_position(1), 20.0);
        assert_eq!(tree.y_position(2), 50.0);

        tree.remove_first();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.total_height(), 70.0);
        assert_eq!(tree.y_position(0), 0.0);
        assert_eq!(tree.y_position(1), 30.0);
    }

    #[test]
    fn test_update_height() {
        let mut tree = tree_from_heights(&[10.0, 20.0, 30.0, 40.0]);

        assert_eq!(tree.total_height(), 100.0);
        assert_eq!(tree.y_position(2), 30.0); // 10 + 20

        // Update middle element
        tree.update(1, 50.0); // 20 -> 50, diff = +30

        assert_eq!(tree.total_height(), 130.0); // 10 + 50 + 30 + 40
        assert_eq!(tree.y_position(0), 0.0);
        assert_eq!(tree.y_position(1), 10.0);
        assert_eq!(tree.y_position(2), 60.0); // 10 + 50
        assert_eq!(tree.y_position(3), 90.0); // 10 + 50 + 30
    }

    #[test]
    fn test_many_removals() {
        let mut tree = HeightTree::new();

        // Add initial elements
        for i in 0..100 {
            tree.push(10.0 + i as f32);
        }

        // Remove many from front, add to back
        for i in 0..500 {
            tree.remove_first();
            tree.push(110.0 + i as f32);
        }

        // Should still work correctly
        assert_eq!(tree.len(), 100);
        assert!(tree.total_height() > 0.0);

        // Test index_at_y still works
        let mid_y = tree.total_height() / 2.0;
        let mid_idx = tree.index_at_y(mid_y);
        assert!(mid_idx < tree.len());
    }

    fn assert_matches_reference(tree: &HeightTree, reference: &VecDeque<f32>) {
        assert_eq!(tree.len(), reference.len());
        let expected_total: f32 = reference.iter().sum();
        assert!((tree.total_height() - expected_total).abs() < 0.5);
        for idx in (0..reference.len()).step_by(97) {
            let expected: f32 = reference.iter().take(idx).sum();
            assert!((tree.y_position(idx) - expected).abs() < 0.5);
            assert_eq!(tree.index_at_y(expected + 0.5), idx);
        }
    }

    #[test]
    fn steady_rotation_keeps_capacity_bounded() {
        const ROWS: usize = 1000;
        let mut tree = HeightTree::new();
        let mut reference = VecDeque::new();
        for i in 0..ROWS {
            let height = 18.0 + (i % 5) as f32;
            tree.push(height);
            reference.push_back(height);
        }

        for i in 0..100_000 {
            tree.remove_first();
            reference.pop_front();
            let height = 18.0 + (i % 7) as f32;
            tree.push(height);
            reference.push_back(height);
            assert!(tree.capacity <= 4096, "capacity grew to {}", tree.capacity);
            if i % 9_973 == 0 {
                assert_matches_reference(&tree, &reference);
            }
        }

        assert_matches_reference(&tree, &reference);
    }

    #[test]
    fn reserve_during_rotation_keeps_capacity_bounded() {
        const ROWS: usize = 1500;
        let mut tree = HeightTree::new();
        let mut reference = VecDeque::new();
        for i in 0..ROWS {
            let height = 18.0 + (i % 5) as f32;
            tree.reserve(1);
            tree.push(height);
            reference.push_back(height);
        }

        let mut capacity_changes = 0;
        let mut last_capacity = tree.capacity;
        for i in 0..100_000 {
            tree.remove_first();
            reference.pop_front();
            let height = 18.0 + (i % 7) as f32;
            tree.reserve(1);
            tree.push(height);
            reference.push_back(height);
            assert!(tree.capacity <= 4096, "capacity grew to {}", tree.capacity);
            if tree.capacity != last_capacity {
                capacity_changes += 1;
                last_capacity = tree.capacity;
            }
            if i % 9_973 == 0 {
                assert_matches_reference(&tree, &reference);
            }
        }

        assert!(
            capacity_changes <= 1,
            "capacity churned {capacity_changes} times"
        );
        assert_matches_reference(&tree, &reference);
    }

    #[test]
    fn oversized_tree_shrinks_after_trim() {
        let mut tree = HeightTree::new();
        let mut reference = VecDeque::new();
        for _ in 0..20_000 {
            tree.push(20.0);
            reference.push_back(20.0);
        }
        assert!(tree.capacity >= 20_000);

        for i in 0..40_000 {
            tree.remove_first();
            reference.pop_front();
            if reference.len() < 1000 {
                let height = 18.0 + (i % 3) as f32;
                tree.push(height);
                reference.push_back(height);
            }
        }

        assert!(
            tree.capacity <= 4096,
            "capacity stayed at {}",
            tree.capacity
        );
        assert_matches_reference(&tree, &reference);
    }
}
