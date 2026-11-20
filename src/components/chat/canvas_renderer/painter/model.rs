//! Message order, lookup index, height tree, and total-height ownership.

use gpui::SharedString;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use crate::backend::ChatMessage;
use crate::config::MAX_CACHED_MESSAGES;

use echo_render_core::height_tree::HeightTree;

/// Estimated height used while async layout is pending (keeps scrolling responsive under load).
const ESTIMATED_MESSAGE_HEIGHT: f32 = 42.0;

pub(super) struct MessageListModel {
    pub(super) messages: Arc<Vec<Arc<ChatMessage>>>,
    pub(super) message_order: Vec<SharedString>,
    pub(super) message_index: FxHashMap<SharedString, u64>,
    pub(super) message_index_base: u64,
    pub(super) height_tree: HeightTree,
    pub(super) total_height: f32,
    measured_height_mean: f32,
    measured_height_samples: u32,
}

/// Measured rows averaged into the unresolved-row height estimate.
const HEIGHT_ESTIMATE_WINDOW: u32 = 256;

impl MessageListModel {
    pub(super) fn initial() -> Self {
        Self {
            messages: Arc::new(Vec::new()),
            message_order: Vec::new(),
            message_index: FxHashMap::default(),
            message_index_base: 0,
            height_tree: HeightTree::with_capacity(MAX_CACHED_MESSAGES),
            total_height: 0.0,
            measured_height_mean: ESTIMATED_MESSAGE_HEIGHT,
            measured_height_samples: 0,
        }
    }

    /// Height used for rows whose layout is not resolved yet: a running mean
    /// of this split's measured rows, or the fixed estimate before any.
    #[inline]
    pub(super) fn estimated_row_height(&self) -> f32 {
        self.measured_height_mean
    }

    pub(super) fn record_measured_height(&mut self, height: f32) {
        if !height.is_finite() || height <= 0.0 {
            return;
        }
        self.measured_height_samples =
            (self.measured_height_samples + 1).min(HEIGHT_ESTIMATE_WINDOW);
        self.measured_height_mean +=
            (height - self.measured_height_mean) / self.measured_height_samples as f32;
    }

    #[inline]
    pub(super) fn sync_total_height(&mut self) {
        self.total_height = self.height_tree.total_height();
    }
    #[inline]
    pub(super) fn clear_message_index(&mut self) {
        self.message_index.clear();
        self.message_index_base = 0;
    }

    #[inline]
    pub(super) fn reserve_message_index(&mut self, additional: usize) {
        self.message_index.reserve(additional);
    }

    #[inline]
    pub(super) fn insert_message_index(&mut self, msg_id: SharedString, relative_index: usize) {
        self.message_index.insert(
            msg_id,
            self.message_index_base
                .saturating_add(relative_index as u64),
        );
    }

    #[inline]
    pub(super) fn advance_message_index_base(&mut self, removed: usize) {
        self.message_index_base = self.message_index_base.saturating_add(removed as u64);
    }

    pub(super) fn remove_rotated_front_indices(&mut self, removed: usize) {
        for evicted_id in self.message_order.drain(..removed) {
            self.message_index.remove(&evicted_id);
        }
        self.advance_message_index_base(removed);
    }

    /// After a plain append the new ids join the order only in the next
    /// `update_layouts` append pass. When a sync comes first, and the order
    /// is exactly the rows' prefix with every height present and none
    /// pending, add the missing ids now as rows waiting for their heights,
    /// which is what that pass would do. Returns how many were added; the
    /// caller adds them to its pending count.
    pub(super) fn complete_lagging_order(&mut self, pending_heights: usize) -> usize {
        let have = self.message_order.len();
        if have == 0
            || have >= self.messages.len()
            || pending_heights != 0
            || self.height_tree.len() != have
            || !super::message_delta::ids_align(&self.message_order, &self.messages)
        {
            return 0;
        }
        let messages = Arc::clone(&self.messages);
        for (index, message) in messages.iter().enumerate().skip(have) {
            if self.get_message_index(&message.id) != Some(index) {
                // Not indexed as the append path leaves it: undo and let the
                // caller rebuild.
                self.message_order.truncate(have);
                return 0;
            }
            self.message_order
                .push(SharedString::from(message.id.clone()));
        }
        messages.len() - have
    }

    /// Remove the first `dropped` rows (possibly none) and add `appended`
    /// after the rest, as
    /// [`super::message_delta::front_drop`] found for the next snapshot. The
    /// dropped heights go now; the appended rows get theirs from the
    /// pending-row pass of `update_layouts`, like any appended row, so the
    /// caller adds `appended.len()` to its pending count.
    ///
    /// Returns false, changing nothing, unless the result has the order and
    /// index a rebuild from the new rows would have (the kept rows keep their
    /// current heights, where a rebuild would take cached or estimated ones):
    /// the order holds one id per current row, the heights cover the order
    /// except the `pending_heights` newest rows, no dropped row is still
    /// pending, each dropped id is indexed at its own row (so no later row
    /// repeats it), and no appended id is indexed yet.
    pub(super) fn drop_front_and_append(
        &mut self,
        dropped: usize,
        appended: &[Arc<ChatMessage>],
        pending_heights: usize,
    ) -> bool {
        let len = self.message_order.len();
        if len != self.messages.len()
            || (dropped == 0 && appended.is_empty())
            || dropped >= len
            || self.height_tree.len() + pending_heights != len
            || dropped > self.height_tree.len()
            || self.message_order[..dropped]
                .iter()
                .enumerate()
                .any(|(index, id)| self.get_message_index(id) != Some(index))
            || appended
                .iter()
                .any(|message| self.get_message_index(&message.id).is_some())
        {
            return false;
        }
        self.remove_rotated_front_indices(dropped);
        for _ in 0..dropped {
            self.height_tree.remove_first();
        }
        let base = self.message_order.len();
        self.message_order.reserve(appended.len());
        for (offset, message) in appended.iter().enumerate() {
            let id = SharedString::from(message.id.clone());
            self.message_order.push(id.clone());
            self.insert_message_index(id, base + offset);
        }
        self.sync_total_height();
        true
    }

    /// How `next` drops rows from the front of the current ones, when the
    /// order is complete enough to tell (one id per current row) after
    /// completing a lagging order, whose added rows go to `pending_heights`.
    pub(super) fn front_drop_shape(
        &mut self,
        next: &[Arc<ChatMessage>],
        pending_heights: &mut usize,
    ) -> Option<super::message_delta::FrontDrop> {
        *pending_heights += self.complete_lagging_order(*pending_heights);
        (self.message_order.len() == self.messages.len())
            .then(|| super::message_delta::front_drop(&self.message_order, next))
            .flatten()
    }

    /// Apply the front drop `front` of `next` within the size limit of
    /// incremental rotations (see [`Self::drop_front_and_append`]), adding
    /// the appended rows to `pending_heights`. Returns whether a kept row
    /// changed in place, or `None`, changing nothing, when the caller must
    /// rebuild.
    pub(super) fn apply_front_drop(
        &mut self,
        next: &[Arc<ChatMessage>],
        front: super::message_delta::FrontDrop,
        pending_heights: &mut usize,
    ) -> Option<bool> {
        let limit = super::CanvasMessageList::incremental_rotation_limit(next.len());
        if front.dropped > limit && front.dropped.saturating_mul(2) > next.len() {
            return None;
        }
        let kept = next.len() - front.appended;
        let kept_changed = self.kept_rows_changed(front.dropped, &next[..kept]);
        if !self.drop_front_and_append(front.dropped, &next[kept..], *pending_heights) {
            return None;
        }
        *pending_heights += front.appended;
        Some(kept_changed)
    }

    /// Whether any of the current rows from `from` on is not, by pointer,
    /// the row at the same position in `kept` (the rows the next snapshot
    /// keeps, in order): a row replaced in place whose offscreen layout must
    /// be checked again.
    pub(super) fn kept_rows_changed(&self, from: usize, kept: &[Arc<ChatMessage>]) -> bool {
        self.messages
            .get(from..)
            .unwrap_or_default()
            .iter()
            .zip(kept)
            .any(|(old, new)| !Arc::ptr_eq(old, new))
    }

    /// An append whose current rows are `next`'s first `self.messages.len()`
    /// rows. The new rows are indexed now; they join the order at the next
    /// `update_layouts` append pass, or now as rows waiting for their heights
    /// when rows already wait (that pass then runs only its pending-row
    /// pass), adding to `pending_heights`.
    pub(super) fn append_rows(&mut self, next: &[Arc<ChatMessage>], pending_heights: &mut usize) {
        let old_count = self.messages.len();
        let join_pending = *pending_heights > 0;
        for (index, message) in next.iter().enumerate().skip(old_count) {
            let id = SharedString::from(message.id.clone());
            if join_pending {
                self.message_order.push(id.clone());
            }
            self.insert_message_index(id, index);
        }
        if join_pending {
            *pending_heights += next.len().saturating_sub(old_count);
        }
    }

    /// Replace the order, index and heights with `next`'s rows and `heights`
    /// (one per row). The caller drops any pending rows.
    pub(super) fn reset_rows(&mut self, next: &[Arc<ChatMessage>], heights: &[f32]) {
        debug_assert_eq!(next.len(), heights.len());
        self.clear_message_index();
        self.reserve_message_index(next.len());
        self.message_order.clear();
        self.height_tree.clear();
        self.message_order.reserve(next.len());
        self.height_tree.reserve(next.len());
        for (index, (message, &height)) in next.iter().zip(heights).enumerate() {
            let id = SharedString::from(message.id.clone());
            self.message_order.push(id.clone());
            self.insert_message_index(id, index);
            self.height_tree.push(height);
        }
        self.sync_total_height();
    }

    /// A history merge whose current rows sit in `next` from `existing_start`
    /// on, contiguous: the rows before them are prepended with `heights` (one
    /// per prepended row) and any rows after them are appended as rows
    /// waiting for their heights, after the rows already waiting (the waiting
    /// rows are the order's last rows, so the old ones come first), adding to
    /// `pending_heights`. Returns the prepended rows' total height.
    pub(super) fn insert_around(
        &mut self,
        next: &[Arc<ChatMessage>],
        existing_start: usize,
        heights: &[f32],
        pending_heights: &mut usize,
    ) -> f32 {
        debug_assert_eq!(existing_start, heights.len());
        let existing_end = existing_start + self.message_order.len();
        let prepended_height = self.height_tree.prepend_many(heights);
        let mut order = Vec::with_capacity(next.len());
        order.extend(
            next[..existing_start]
                .iter()
                .map(|message| SharedString::from(message.id.clone())),
        );
        order.append(&mut self.message_order);
        order.extend(
            next.get(existing_end..)
                .unwrap_or_default()
                .iter()
                .map(|message| SharedString::from(message.id.clone())),
        );
        self.message_order = order;
        self.rebuild_message_index_from_order();
        *pending_heights += next.len().saturating_sub(existing_end);
        self.sync_total_height();
        prepended_height
    }

    /// The `needs_layout_rebuild` shortcut of `update_layouts`: the order is
    /// the rows' ids and the heights cover them, rows waiting for their
    /// heights included, so a progressive refresh can replace the rebuild.
    pub(super) fn order_and_heights_match(&self, pending_heights: usize) -> bool {
        self.message_order.len() == self.messages.len()
            && self.height_tree.len() + pending_heights == self.messages.len()
            && self
                .message_order
                .iter()
                .zip(self.messages.iter())
                .all(|(id, message)| id.as_ref() == message.id.as_ref())
    }

    #[inline]
    pub(super) fn get_message_index(&self, msg_id: &str) -> Option<usize> {
        let absolute = *self.message_index.get(msg_id)?;
        let relative = absolute.checked_sub(self.message_index_base)?;
        let relative = usize::try_from(relative).ok()?;
        (relative < self.messages.len()).then_some(relative)
    }

    pub(super) fn rebuild_message_index_from_order(&mut self) {
        self.clear_message_index();
        self.reserve_message_index(self.message_order.len());
        for idx in 0..self.message_order.len() {
            self.insert_message_index(self.message_order[idx].clone(), idx);
        }
    }
}
