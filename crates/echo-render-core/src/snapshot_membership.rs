//! Ordered membership guard for retained render snapshots.
//!
//! A snapshot may be reused only while it covers exactly the current dynamic
//! direct-render set. The cursor is allocation-free and accepts entries as the
//! renderer discovers them, so callers do not need to materialize an
//! intermediate list merely to compare membership.

/// Checks that retained snapshot entries and current direct-render entries have
/// identical ordered `(message_index, layout_identity)` membership.
pub struct OrderedSnapshotMembership<I> {
    cached: I,
    matches: bool,
}

impl<I> OrderedSnapshotMembership<I>
where
    I: Iterator<Item = (usize, usize)>,
{
    #[inline]
    pub fn new(cached: I) -> Self {
        Self {
            cached,
            matches: true,
        }
    }

    /// Record one currently direct-rendered row in visible order.
    #[inline]
    pub fn observe_direct_row(&mut self, message_index: usize, layout_identity: usize) {
        if self.cached.next() != Some((message_index, layout_identity)) {
            self.matches = false;
        }
    }

    /// True only when every current direct row matched and no cached row remains.
    #[inline]
    pub fn finish(mut self) -> bool {
        self.matches && self.cached.next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::OrderedSnapshotMembership;

    #[test]
    fn loading_to_ready_row_rejects_a_snapshot_that_only_had_fallback_rows() {
        let mut membership = OrderedSnapshotMembership::new([].into_iter());
        membership.observe_direct_row(12, 0xCAFE);
        assert!(!membership.finish());

        let mut mixed = OrderedSnapshotMembership::new([(1, 0xA), (3, 0xC)].into_iter());
        mixed.observe_direct_row(1, 0xA);
        mixed.observe_direct_row(2, 0xB);
        mixed.observe_direct_row(3, 0xC);
        assert!(!mixed.finish());
    }

    #[test]
    fn ready_to_fallback_row_rejects_a_snapshot_with_a_stale_direct_row() {
        let membership = OrderedSnapshotMembership::new([(12, 0xCAFE)].into_iter());
        assert!(!membership.finish());

        let mut mixed = OrderedSnapshotMembership::new([(1, 0xA), (2, 0xB), (3, 0xC)].into_iter());
        mixed.observe_direct_row(1, 0xA);
        mixed.observe_direct_row(3, 0xC);
        assert!(!mixed.finish());
    }

    #[test]
    fn direct_rows_require_matching_order_and_layout_identity() {
        let mut matching = OrderedSnapshotMembership::new([(3, 0xA), (8, 0xB)].into_iter());
        matching.observe_direct_row(3, 0xA);
        matching.observe_direct_row(8, 0xB);
        assert!(matching.finish());

        let mut replaced_layout = OrderedSnapshotMembership::new([(3, 0xA)].into_iter());
        replaced_layout.observe_direct_row(3, 0xB);
        assert!(!replaced_layout.finish());
    }
}
