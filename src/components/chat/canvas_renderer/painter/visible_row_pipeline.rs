//! Visible-row build and reuse decisions.

use super::*;

/// Rows whose original text still shows fade; the collapsed stand-in is
/// already muted and reads at full strength.
fn fade_deleted_row(msg: &ChatMessage) -> bool {
    msg.flags.is_deleted
        && msg.flags.deleted_presentation != echo_core::message::DeletedPresentation::Collapsed
}

impl CanvasMessageList {
    pub(super) fn build_visible_render_row_for_index(
        &mut self,
        idx: usize,
        viewport_y: f32,
        window: &mut Window,
        state: &mut VisibleRowBuildFrameState,
    ) -> Option<VisibleRenderRow> {
        let _layout_context = self.enter_layout_context();
        let msg_id = self.message_model.message_order.get(idx)?.clone();
        let cache_key = LayoutCacheKey::from_shared_quantized(
            msg_id,
            state.quantized_width,
            self.show_timestamp,
        );
        let content_fingerprint = self
            .message_model
            .messages
            .get(idx)
            .map(|message| message_content_fingerprint(message));

        if let Some(mut layout) = self.get_cached_layout(&cache_key).filter(|layout| {
            content_fingerprint.is_some_and(|fingerprint| !layout.is_stale_for_message(fingerprint))
        }) {
            if idx >= state.viewport_anchor_start_idx
                && self.update_height_preserving_viewport_anchor(
                    idx,
                    layout.height,
                    state.viewport_anchor_start_idx,
                    state.was_at_bottom_before_metric_refresh,
                )
            {
                state.fallback_metric_heights_updated = true;
            }

            if !layout.uses_window_text_metrics
                && state.fallback_metric_refreshes < VISIBLE_FALLBACK_METRIC_REFRESH_BUDGET
            {
                let msg_for_layout = self.message_model.messages.get(idx).cloned();
                if let Some(msg) = msg_for_layout {
                    let mut refreshed_layout = build_message_layout(
                        &msg,
                        &self.channel_name,
                        state.effective_layout_width,
                        self.show_timestamp,
                        Some(window),
                    );
                    refreshed_layout.mark_emote_versions_current(state.current_emote_version);
                    let new_height = refreshed_layout.height;
                    self.cache_layout(cache_key.clone(), refreshed_layout);
                    if idx >= state.viewport_anchor_start_idx
                        && self.update_height_preserving_viewport_anchor(
                            idx,
                            new_height,
                            state.viewport_anchor_start_idx,
                            state.was_at_bottom_before_metric_refresh,
                        )
                    {
                        state.fallback_metric_heights_updated = true;
                    }
                    if let Some(refreshed) = self.get_cached_layout(&cache_key) {
                        layout = refreshed;
                    }
                    state.fallback_metric_refreshes += 1;
                    state.visible_layouts_replaced = true;
                    state.needs_notify = true;
                }
            }

            return Some(VisibleRenderRow {
                layout,
                message_index: idx,
                viewport_y,
                is_alternate_row: self
                    .message_model
                    .messages
                    .get(idx)
                    .map(|m| m.flags.is_alternate_row)
                    .unwrap_or(false),
                is_deleted: self
                    .message_model
                    .messages
                    .get(idx)
                    .map(|m| fade_deleted_row(m))
                    .unwrap_or(false),
            });
        }

        let msg = self.message_model.messages.get(idx).cloned()?;
        self.render_perf_frame_stats.layout_cache_misses += 1;

        let mut layout_for_row = None;
        if state.visible_cache_miss_rebuilds < VISIBLE_CACHE_MISS_SYNC_REBUILD_BUDGET {
            let mut rebuilt_layout = build_message_layout(
                &msg,
                &self.channel_name,
                state.effective_layout_width,
                self.show_timestamp,
                Some(window),
            );
            rebuilt_layout.mark_emote_versions_current(state.current_emote_version);
            let new_height = rebuilt_layout.height;
            self.cache_layout(cache_key.clone(), rebuilt_layout);
            if idx >= state.viewport_anchor_start_idx
                && self.update_height_preserving_viewport_anchor(
                    idx,
                    new_height,
                    state.viewport_anchor_start_idx,
                    state.was_at_bottom_before_metric_refresh,
                )
            {
                state.fallback_metric_heights_updated = true;
            }
            layout_for_row = self.get_cached_layout(&cache_key);
            state.visible_cache_miss_rebuilds += 1;
            state.visible_layouts_replaced = true;
            state.needs_notify = true;
        }

        if layout_for_row.is_none() {
            self.render_perf_frame_stats.async_layout_requests += 1;
            let _ = self.request_async_layout(&msg);
            let fingerprint = message_content_fingerprint(&msg);
            layout_for_row = self
                .find_any_layout_for_message(&cache_key.message_id)
                .filter(|layout| layout.content_fingerprint == fingerprint);
            state.needs_notify = true;

            let (visible_start, visible_end) = self.get_visible_range();
            self.preload_layouts_for_visible_range(visible_start, visible_end);
        }

        if layout_for_row.is_none() {
            let mut rebuilt_layout = build_message_layout(
                &msg,
                &self.channel_name,
                state.effective_layout_width,
                self.show_timestamp,
                Some(window),
            );
            rebuilt_layout.mark_emote_versions_current(state.current_emote_version);
            let new_height = rebuilt_layout.height;
            self.cache_layout(cache_key.clone(), rebuilt_layout);
            if idx >= state.viewport_anchor_start_idx
                && self.update_height_preserving_viewport_anchor(
                    idx,
                    new_height,
                    state.viewport_anchor_start_idx,
                    state.was_at_bottom_before_metric_refresh,
                )
            {
                state.fallback_metric_heights_updated = true;
            }
            layout_for_row = self.get_cached_layout(&cache_key);
            state.visible_layouts_replaced = true;
            state.needs_notify = true;
            crate::log_ui!(
                debug,
                channel_id = %self.channel_id,
                idx,
                msg_id = %cache_key.message_id,
                "Visible row had no cached layout at any width; built synchronously past budget"
            );
        }

        let layout = layout_for_row?;
        Some(VisibleRenderRow {
            layout,
            message_index: idx,
            viewport_y,
            is_alternate_row: msg.flags.is_alternate_row,
            is_deleted: fade_deleted_row(&msg),
        })
    }

    #[inline]
    fn layout_mode_outdated(&self) -> bool {
        self.layout_scale_factor.to_bits() != self.layout_store.runtime.last_layout_scale_bits
            || self.mod_buttons_active != self.layout_store.runtime.last_layout_mod_buttons
    }

    #[inline]
    pub(super) fn can_reuse_visible_rows_for_stable_frame(
        &self,
        start_idx: usize,
        end_idx: usize,
        scroll_offset: f32,
        quantized_width: u32,
    ) -> bool {
        if self.wake_state.has_async_layout_in_flight_last_frame
            || self
                .layout_store
                .runtime
                .pending_layout_quantized_width
                .is_some()
            || self.message_state.needs_layout_rebuild
            || self.message_state.pending_new_message_count > 0
            || self.layout_store.runtime.has_stale_layouts
            || self.viewport_state.visible_rows_have_fallback_metrics
            || self.message_model.message_order.len() != self.message_model.messages.len()
            || self.message_model.height_tree.len() != self.message_model.messages.len()
            || self.viewport_state.cached_visible_render_range != Some((start_idx, end_idx))
            || self.viewport_state.cached_visible_render_quantized_width != quantized_width
            || self.viewport_state.cached_visible_render_message_count
                != self.message_model.messages.len()
            || (self.viewport_state.cached_visible_render_scroll_offset - scroll_offset).abs() > 0.5
            || settings::get_layout_settings_version()
                != self.layout_store.runtime.last_layout_settings_version_seen
            || global_layout_cache::get_invalidation_version()
                != self.layout_store.runtime.last_invalidation_version
            || global_layout_cache::layout_eviction_seq()
                != self.layout_store.runtime.last_layout_eviction_seq
            || crate::state::VersionRegistry::channel_emote_version(&self.channel_name)
                != self.layout_store.runtime.last_checked_channel_emote_version
            || self.layout_mode_outdated()
        {
            return false;
        }

        true
    }

    /// Diagnostic mirror of `can_reuse_visible_rows_for_stable_frame`: one bit
    /// per failed gate term (`STABLE_GATE_TERM_NAMES` order), all terms
    /// evaluated. Only called when perf diagnostics are on.
    pub(super) fn stable_frame_gate_failures(
        &self,
        start_idx: usize,
        end_idx: usize,
        scroll_offset: f32,
        quantized_width: u32,
    ) -> u32 {
        let message_count = self.message_model.messages.len();
        let runtime = &self.layout_store.runtime;
        let terms = [
            self.wake_state.has_async_layout_in_flight_last_frame,
            runtime.pending_layout_quantized_width.is_some(),
            self.message_state.needs_layout_rebuild,
            self.message_state.pending_new_message_count > 0,
            runtime.has_stale_layouts,
            self.viewport_state.visible_rows_have_fallback_metrics,
            self.message_model.message_order.len() != message_count,
            self.message_model.height_tree.len() != message_count,
            self.viewport_state.cached_visible_render_range != Some((start_idx, end_idx)),
            self.viewport_state.cached_visible_render_quantized_width != quantized_width,
            self.viewport_state.cached_visible_render_message_count != message_count,
            (self.viewport_state.cached_visible_render_scroll_offset - scroll_offset).abs() > 0.5,
            settings::get_layout_settings_version() != runtime.last_layout_settings_version_seen,
            global_layout_cache::get_invalidation_version() != runtime.last_invalidation_version,
            global_layout_cache::layout_eviction_seq() != runtime.last_layout_eviction_seq,
            crate::state::VersionRegistry::channel_emote_version(&self.channel_name)
                != runtime.last_checked_channel_emote_version,
            self.layout_mode_outdated(),
        ];
        terms
            .iter()
            .enumerate()
            .fold(0, |mask, (bit, failed)| mask | (u32::from(*failed) << bit))
    }

    #[inline]
    pub(super) fn can_translate_visible_rows_for_scroll_frame(
        &self,
        start_idx: usize,
        end_idx: usize,
        quantized_width: u32,
    ) -> bool {
        let Some((cached_start_idx, cached_end_idx)) =
            self.viewport_state.cached_visible_render_range
        else {
            return false;
        };
        let ranges_overlap = start_idx < cached_end_idx && cached_start_idx < end_idx;
        if self.wake_state.has_async_layout_in_flight_last_frame
            || self
                .layout_store
                .runtime
                .pending_layout_quantized_width
                .is_some()
            || self.message_state.needs_layout_rebuild
            || self.message_state.pending_new_message_count > 0
            || self.layout_store.runtime.has_stale_layouts
            || self.viewport_state.visible_rows_have_fallback_metrics
            || self.message_model.message_order.len() != self.message_model.messages.len()
            || self.message_model.height_tree.len() != self.message_model.messages.len()
            || !ranges_overlap
            || self.viewport_state.cached_visible_render_quantized_width != quantized_width
            || self.viewport_state.cached_visible_render_message_count
                != self.message_model.messages.len()
            || settings::get_layout_settings_version()
                != self.layout_store.runtime.last_layout_settings_version_seen
            || global_layout_cache::get_invalidation_version()
                != self.layout_store.runtime.last_invalidation_version
            || global_layout_cache::layout_eviction_seq()
                != self.layout_store.runtime.last_layout_eviction_seq
            || crate::state::VersionRegistry::channel_emote_version(&self.channel_name)
                != self.layout_store.runtime.last_checked_channel_emote_version
            || self.layout_mode_outdated()
        {
            return false;
        }

        true
    }
}
