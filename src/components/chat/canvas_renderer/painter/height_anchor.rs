//! Height updates that preserve viewport anchoring.

use super::*;

#[derive(Clone)]
pub(super) struct ViewportScrollAnchor {
    message_id: SharedString,
    row_fraction: f32,
}

impl CanvasMessageList {
    /// Capture the first visible message and the relative position inside it.
    /// Pixel offsets and whole-buffer ratios both drift when a DPI/settings
    /// change rebuilds row heights; an identity anchor survives that rebuild.
    pub(super) fn capture_viewport_scroll_anchor(&self) -> Option<ViewportScrollAnchor> {
        if self.message_model.height_tree.is_empty() || self.is_at_bottom() {
            return None;
        }

        let (index, _) = self.get_visible_range();
        let message_id = self.message_model.message_order.get(index)?.clone();
        let row_y = self.message_model.height_tree.y_position(index);
        let row_height = self
            .message_model
            .height_tree
            .height_at(index)
            .unwrap_or(1.0)
            .max(1.0);
        let row_fraction = ((self.scroll_controller.offset - row_y) / row_height).clamp(0.0, 1.0);

        Some(ViewportScrollAnchor {
            message_id,
            row_fraction,
        })
    }

    /// Restore an identity-based viewport anchor after rebuilding the height
    /// tree. Cancel any old smooth-scroll target so it cannot immediately pull
    /// the newly restored viewport toward coordinates from the previous DPI.
    pub(super) fn restore_viewport_scroll_anchor(&mut self, anchor: ViewportScrollAnchor) -> bool {
        let Some(index) = self.get_message_index(anchor.message_id.as_ref()) else {
            return false;
        };
        let row_y = self.message_model.height_tree.y_position(index);
        let row_height = self
            .message_model
            .height_tree
            .height_at(index)
            .unwrap_or(1.0)
            .max(1.0);
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let target = (row_y + row_height * anchor.row_fraction).clamp(0.0, max_scroll);

        self.scroll_controller.offset = target;
        self.scroll_controller.state.prev_scroll_offset = target;
        self.scroll_controller.state.scroll_desired = target;
        self.scroll_controller.state.scroll_animating = false;
        self.scroll_controller.state.auto_scroll = false;
        self.viewport_state.cached_visible_render_range = None;
        self.interaction_state.hitbox_manager.invalidate_cache();
        true
    }

    pub(in crate::components::chat::canvas_renderer) fn update_height_preserving_viewport_anchor(
        &mut self,
        index: usize,
        new_height: f32,
        anchor_start_idx: usize,
        was_at_bottom: bool,
    ) -> bool {
        let Some(old_height) = self.message_model.height_tree.height_at(index) else {
            return false;
        };

        if !self.message_model.height_tree.update(index, new_height) {
            return false;
        }
        self.message_model.record_measured_height(new_height);

        if !was_at_bottom && index < anchor_start_idx {
            let delta = new_height - old_height;
            self.scroll_controller.offset = (self.scroll_controller.offset + delta).max(0.0);
            self.scroll_controller.state.scroll_desired =
                (self.scroll_controller.state.scroll_desired + delta).max(0.0);
            self.render_perf_frame_stats.height_anchor_adjustments += 1;
        }

        true
    }
}
