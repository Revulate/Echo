//! Message order, lookup index, height tree, and total-height ownership.

use gpui::SharedString;
use rustc_hash::FxHashMap;
use std::sync::Arc;

use crate::backend::ChatMessage;
use crate::config::MAX_CACHED_MESSAGES;

use super::super::height_tree::HeightTree;

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
            measured_height_mean: super::ESTIMATED_MESSAGE_HEIGHT,
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

    #[inline]
    pub(super) fn clear_all(&mut self) {
        self.messages = Arc::new(Vec::new());
        self.message_order.clear();
        self.message_index.clear();
        self.message_index_base = 0;
        self.height_tree.clear();
        self.total_height = 0.0;
    }
}
