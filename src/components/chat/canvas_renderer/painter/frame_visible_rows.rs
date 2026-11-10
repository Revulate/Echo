//! Visible-row frame planning and cache updates.

use super::*;

pub(super) struct VisibleRowsFrameResult {
    pub(super) needs_notify: bool,
}

impl CanvasMessageList {
    pub(super) fn update_visible_rows_for_render_frame(
        &mut self,
        start_idx: usize,
        end_idx: usize,
        scroll_offset: f32,
        stable_visible_row_fast_path: bool,
        scroll_visible_row_fast_path: bool,
        window: &mut Window,
    ) -> VisibleRowsFrameResult {
        let metrics_enabled = crate::backend::metrics::is_enabled();
        let visible_row_scan_start = RenderTimer::start(metrics_enabled);
        let mut needs_notify = false;

        if stable_visible_row_fast_path {
            crate::backend::metrics::record_emote_stage(
                "canvas.visible_scan_reused",
                visible_row_scan_start.elapsed(),
                self.viewport_state.visible_render_rows.len(),
            );
        } else if scroll_visible_row_fast_path {
            let mut state = self.visible_row_build_frame_state();
            let delta_y = self.viewport_state.cached_visible_render_scroll_offset - scroll_offset;
            let mut retained_rows = std::mem::take(&mut self.viewport_state.scratch_retained_rows);
            retained_rows.clear();
            for mut row in self.viewport_state.visible_render_rows.drain(..) {
                if (start_idx..end_idx).contains(&row.message_index) {
                    row.viewport_y += delta_y;
                    retained_rows.insert(row.message_index, row);
                }
            }

            self.reserve_visible_row_capacity(start_idx, end_idx);
            let mut viewport_y =
                self.message_model.height_tree.y_position(start_idx) - scroll_offset;
            for idx in start_idx..end_idx {
                match retained_rows.remove(&idx) {
                    Some(mut row) => {
                        row.viewport_y = viewport_y;
                        self.viewport_state.visible_render_rows.push(row);
                    }
                    _ => {
                        match self
                            .build_visible_render_row_for_index(idx, viewport_y, window, &mut state)
                        {
                            Some(row) => {
                                self.viewport_state.visible_render_rows.push(row);
                            }
                            _ => {
                                crate::log_ui!(
                                    debug,
                                    channel_id = %self.channel_id,
                                    idx,
                                    msg_id = %self
                                        .message_model
                                        .message_order
                                        .get(idx)
                                        .map(|id| id.as_ref())
                                        .unwrap_or("<no-order-entry>"),
                                    "Visible row build returned no layout (blank hole) [scroll path]"
                                );
                            }
                        }
                    }
                }

                viewport_y += self.message_model.height_tree.height_at(idx).unwrap_or(0.0);
            }

            self.apply_visible_row_build_state(&state);
            needs_notify |= state.needs_notify;
            self.cache_visible_row_range(start_idx, end_idx, scroll_offset, state.quantized_width);
            self.trace_visible_row_geometry(state.quantized_width);
            crate::backend::metrics::record_emote_stage(
                "canvas.visible_scan_scroll_translated",
                visible_row_scan_start.elapsed(),
                self.viewport_state.visible_render_rows.len(),
            );
            retained_rows.clear();
            self.viewport_state.scratch_retained_rows = retained_rows;
        } else {
            self.viewport_state.visible_render_rows.clear();
            self.reserve_visible_row_capacity(start_idx, end_idx);

            let mut state = self.visible_row_build_frame_state();
            let mut viewport_y =
                self.message_model.height_tree.y_position(start_idx) - scroll_offset;
            for idx in start_idx..end_idx {
                match self.build_visible_render_row_for_index(idx, viewport_y, window, &mut state) {
                    Some(row) => {
                        self.viewport_state.visible_render_rows.push(row);
                    }
                    _ => {
                        crate::log_ui!(
                            debug,
                            channel_id = %self.channel_id,
                            idx,
                            msg_id = %self
                                .message_model
                                .message_order
                                .get(idx)
                                .map(|id| id.as_ref())
                                .unwrap_or("<no-order-entry>"),
                            "Visible row build returned no layout (blank hole) [rebuild path]"
                        );
                    }
                }

                viewport_y += self.message_model.height_tree.height_at(idx).unwrap_or(0.0);
            }

            self.apply_visible_row_build_state(&state);
            needs_notify |= state.needs_notify;
            self.trace_visible_row_geometry(state.quantized_width);
            crate::backend::metrics::record_emote_stage(
                "canvas.visible_scan",
                visible_row_scan_start.elapsed(),
                self.viewport_state.visible_render_rows.len(),
            );

            self.cache_visible_row_range(start_idx, end_idx, scroll_offset, state.quantized_width);

            let layout_image_preload_start = RenderTimer::start(metrics_enabled);
            let layout_image_preload_count =
                self.preload_static_images_for_visible_layouts(start_idx, end_idx);
            crate::backend::metrics::record_emote_stage(
                "canvas.visible_layout_image_scan",
                layout_image_preload_start.elapsed(),
                layout_image_preload_count,
            );
        }

        VisibleRowsFrameResult { needs_notify }
    }

    fn trace_visible_row_geometry(&mut self, quantized_width: u32) {
        if !relayout_trace::enabled() {
            return;
        }
        let mut trace = self
            .viewport_state
            .relayout_geometry
            .take()
            .unwrap_or_default();
        trace.observe(
            &self.channel_id,
            quantized_width,
            self.show_timestamp,
            self.layout_store.runtime.relayout_cause,
            self.viewport_state
                .visible_render_rows
                .iter()
                .map(|row| &row.layout),
        );
        self.viewport_state.relayout_geometry = Some(trace);
    }

    fn visible_row_build_frame_state(&self) -> VisibleRowBuildFrameState {
        VisibleRowBuildFrameState {
            quantized_width: self.layout_quantized_width_for_cache(),
            effective_layout_width: self.layout_effective_width_for_builds(),
            current_emote_version: crate::backend::slots::resolver::get_emote_version(),
            viewport_anchor_start_idx: self.get_viewport_range().0,
            was_at_bottom_before_metric_refresh: self.is_at_bottom(),
            fallback_metric_refreshes: 0,
            visible_cache_miss_rebuilds: 0,
            fallback_metric_heights_updated: false,
            visible_layouts_replaced: false,
            needs_notify: false,
        }
    }

    fn reserve_visible_row_capacity(&mut self, start_idx: usize, end_idx: usize) {
        let target_len = end_idx.saturating_sub(start_idx);
        if self.viewport_state.visible_render_rows.capacity() < target_len {
            self.viewport_state.visible_render_rows.reserve(
                target_len.saturating_sub(self.viewport_state.visible_render_rows.capacity()),
            );
        }
    }

    fn apply_visible_row_build_state(&mut self, state: &VisibleRowBuildFrameState) {
        if state.fallback_metric_heights_updated {
            self.message_model.sync_total_height();
            self.interaction_state.hitbox_manager.invalidate_cache();
            if state.was_at_bottom_before_metric_refresh {
                self.scroll_to_bottom();
            }
        } else if state.visible_layouts_replaced {
            self.interaction_state.hitbox_manager.invalidate_cache();
        }
    }

    fn cache_visible_row_range(
        &mut self,
        start_idx: usize,
        end_idx: usize,
        scroll_offset: f32,
        quantized_width: u32,
    ) {
        self.viewport_state.cached_visible_render_range = Some((start_idx, end_idx));
        self.viewport_state.cached_visible_render_scroll_offset = scroll_offset;
        self.viewport_state.cached_visible_render_quantized_width = quantized_width;
        self.viewport_state.cached_visible_render_message_count = self.message_model.messages.len();
        self.viewport_state.visible_rows_have_fallback_metrics = self
            .viewport_state
            .visible_render_rows
            .iter()
            .any(|row| !row.layout.uses_window_text_metrics);
    }
}
