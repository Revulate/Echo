//! Width-bucket stabilization and resize handling.

use super::*;

impl CanvasMessageList {
    pub(super) fn queue_layout_rebuild(&mut self) {
        self.message_state.needs_layout_rebuild = true;
        self.message_state.pending_new_message_count = 0;
        self.interaction_state.hitbox_manager.invalidate_cache();
    }

    #[inline]
    pub(super) fn incremental_rotation_limit(message_count: usize) -> usize {
        message_count
            .saturating_div(3)
            .clamp(50, MAX_INCREMENTAL_ROTATION_ABS)
    }

    #[inline]
    pub(super) fn full_rebuild_prime_range(
        message_count: usize,
        prefer_tail: bool,
        visible_range: Option<(usize, usize)>,
    ) -> (usize, usize) {
        let budget = FULL_REBUILD_LAYOUT_PRIME_BUDGET.min(message_count);
        if message_count == 0 || message_count <= budget {
            return (0, message_count);
        }

        if prefer_tail {
            return (message_count.saturating_sub(budget), message_count);
        }

        if let Some((start, end)) = visible_range {
            let start = start.min(message_count);
            let end = end.min(message_count);
            if start < end {
                let visible_len = end.saturating_sub(start);
                let before = budget.saturating_sub(visible_len) / 2;
                let range_start = start.saturating_sub(before);
                let range_end = range_start.saturating_add(budget).min(message_count);
                return (range_end.saturating_sub(budget), range_end);
            }
        }

        (0, budget)
    }

    #[inline]
    pub(super) fn should_prime_full_rebuild_layout(
        index: usize,
        prime_range: (usize, usize),
    ) -> bool {
        index >= prime_range.0 && index < prime_range.1
    }

    #[inline]
    pub(super) fn should_apply_quantized_width_bucket_change(
        old_quantized: u32,
        pending_q: u32,
        container_width: f32,
        elapsed_ms: u128,
    ) -> bool {
        echo_chat_render::width_bucket::should_apply_quantized_width_bucket_change(
            old_quantized,
            pending_q,
            container_width,
            elapsed_ms,
        )
    }

    pub(super) fn maybe_apply_stabilized_width_bucket_change(&mut self) {
        let Some(pending_q) = self.layout_store.runtime.pending_layout_quantized_width else {
            return;
        };

        if pending_q == self.layout_quantized_width_for_cache() {
            self.layout_store.runtime.clear_pending_width_bucket();
            return;
        }

        let Some(since) = self.layout_store.runtime.pending_layout_quantized_since else {
            let now = std::time::Instant::now();
            self.layout_store.runtime.pending_layout_quantized_since = Some(now);
            self.layout_store
                .runtime
                .pending_layout_quantized_run_since
                .get_or_insert(now);
            return;
        };

        let elapsed_ms = since.elapsed().as_millis();
        let run_due = self
            .layout_store
            .runtime
            .pending_layout_quantized_run_since
            .is_some_and(|start| start.elapsed().as_millis() >= WIDTH_BUCKET_FORCE_APPLY_MS);
        let old_quantized = self.layout_quantized_width_for_cache();
        if !run_due
            && !Self::should_apply_quantized_width_bucket_change(
                old_quantized,
                pending_q,
                self.container_width,
                elapsed_ms,
            )
        {
            return;
        }
        let force_apply = run_due || elapsed_ms >= WIDTH_BUCKET_FORCE_APPLY_MS;

        let was_at_bottom = self.is_at_bottom();
        self.message_state.pending_scroll_ratio = None;
        if was_at_bottom {
            self.message_state.pending_scroll_to_bottom = true;
        }

        self.layout_store.runtime.active_layout_quantized_width = pending_q;
        self.layout_store.runtime.clear_pending_width_bucket();
        self.layout_store.runtime.restart_width_refresh();
        self.interaction_state.hitbox_manager.invalidate_cache();
        self.note_relayout_cause("width_bucket");

        crate::log_ui!(
            debug,
            channel_id = %self.channel_id,
            old_quantized,
            new_quantized = pending_q,
            width = self.container_width,
            force_apply,
            "Applied stabilized quantized width change (progressive layout refresh queued)"
        );
    }

    pub fn set_container_width(&mut self, width: f32) {
        if width <= 0.0 {
            return;
        }

        let width_diff = (width - self.container_width).abs();

        let old_quantized = self.layout_quantized_width_for_cache();
        let new_quantized = quantize_width(width);
        let quantized_changed = old_quantized != new_quantized;

        if quantized_changed || width_diff > 1.0 {
            crate::log_ui!(
                trace,
                channel_id = %self.channel_id,
                current = self.container_width,
                requested = width,
                diff = width_diff,
                old_quantized,
                new_quantized,
                quantized_changed,
                resize_in_progress = self.layout_store.runtime.resize_in_progress,
                "set_container_width"
            );
        }

        self.container_width = width;
        if quantized_changed {
            if self.message_model.message_order.is_empty()
                && self.message_model.height_tree.is_empty()
            {
                self.layout_store.runtime.active_layout_quantized_width = new_quantized;
                self.layout_store.runtime.clear_pending_width_bucket();
                self.queue_layout_rebuild();
                crate::log_ui!(
                    trace,
                    channel_id = %self.channel_id,
                    old_quantized,
                    new_quantized,
                    "Applied initial quantized width immediately"
                );
                return;
            }

            let runtime = &mut self.layout_store.runtime;
            if runtime.pending_layout_quantized_width != Some(new_quantized) {
                let now = std::time::Instant::now();
                // A run left behind by a canvas that stopped rendering (hidden
                // tab) must not force the next, unrelated change straight through.
                if runtime
                    .pending_layout_quantized_run_since
                    .is_none_or(|start| {
                        start.elapsed().as_millis() > 2 * WIDTH_BUCKET_FORCE_APPLY_MS
                    })
                {
                    runtime.pending_layout_quantized_run_since = Some(now);
                }
                runtime.pending_layout_quantized_width = Some(new_quantized);
                runtime.pending_layout_quantized_since = Some(now);
            }
            crate::log_ui!(
                trace,
                channel_id = %self.channel_id,
                active_quantized = old_quantized,
                pending_quantized = new_quantized,
                stabilize_ms = WIDTH_BUCKET_STABILIZE_MS,
                "Queued quantized width change (stabilizing)"
            );
        } else {
            self.layout_store.runtime.clear_pending_width_bucket();
            crate::log_ui!(
                trace,
                quantized = new_quantized,
                "Container width updated, active quantized width unchanged"
            );
        }
    }
}
