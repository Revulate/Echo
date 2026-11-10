//! Stale layout visible-first refresh.

use super::*;
use crate::components::chat::canvas_renderer::async_layout::LayoutStatus;

/// Number of visible stale layouts to rebuild synchronously per tick (visible-first refresh).
const STALE_VISIBLE_SYNC_REFRESH_BUDGET: usize = 4;
/// Background stale-layout scan budget per tick (offscreen phased refresh).
const STALE_BACKGROUND_SCAN_BUDGET: usize = 48;
/// Low stale-layout scan budget for mounted sibling splits that are visible but not focused.
const STALE_INACTIVE_BACKGROUND_SCAN_BUDGET: usize = 8;

impl CanvasMessageList {
    pub(super) fn refresh_stale_layouts_visible_first(
        &mut self,
        window: &mut Window,
        current_emote_version: u64,
        effective_layout_width: f32,
        quantized_width: u32,
    ) {
        let _layout_context = self.enter_layout_context();
        let msg_count = self.message_model.messages.len();
        if msg_count == 0
            || self.message_model.message_order.len() != msg_count
            || self.message_model.height_tree.len() != msg_count
        {
            return;
        }

        let (visible_start_raw, visible_end_raw) = self.get_visible_range();
        let visible_start = visible_start_raw.min(msg_count);
        let visible_end = visible_end_raw.min(msg_count);
        let (viewport_start_raw, viewport_end_raw) = self.get_viewport_range();
        let viewport_start = viewport_start_raw.min(msg_count);
        let viewport_end = viewport_end_raw.min(msg_count);
        let visible_len = visible_end.saturating_sub(visible_start);
        let background_budget = if self.is_active_split {
            STALE_BACKGROUND_SCAN_BUDGET
        } else {
            STALE_INACTIVE_BACKGROUND_SCAN_BUDGET
        }
        .min(msg_count);
        let mut work_indices = SmallVec::<[usize; 128]>::new();

        for idx in visible_start..visible_end {
            work_indices.push(idx);
        }

        let mut wrapped = false;
        if msg_count > 0 && background_budget > 0 {
            let mut cursor = self
                .layout_store
                .runtime
                .stale_layout_refresh_cursor
                .min(msg_count - 1);
            for _ in 0..background_budget {
                let idx = cursor;
                cursor += 1;
                if cursor >= msg_count {
                    cursor = 0;
                    wrapped = true;
                }

                if idx < visible_start || idx >= visible_end {
                    work_indices.push(idx);
                }
            }
            self.layout_store.runtime.stale_layout_refresh_cursor = cursor;
        }

        let was_at_bottom = self.is_at_bottom();
        let viewport_anchor_start = viewport_start_raw;
        let messages = self.message_model.messages.clone();
        let mut heights_updated = 0usize;
        let mut sync_visible_rebuilt = 0usize;
        let mut cache_repairs = 0usize;
        let mut async_requested = 0usize;

        for &idx in work_indices.iter() {
            let Some(msg) = messages.get(idx) else {
                continue;
            };

            let Some(shared_id) = self.message_model.message_order.get(idx).cloned() else {
                continue;
            };
            let cache_key = LayoutCacheKey::from_shared_quantized(
                shared_id,
                quantized_width,
                self.show_timestamp,
            );

            let content_fingerprint = message_content_fingerprint(msg);
            let local_layout = self.layout_store.layouts.peek(&cache_key).cloned();
            let local_stale = local_layout
                .as_ref()
                .map(|layout| layout.is_stale_for_message(content_fingerprint))
                .unwrap_or(false);
            let global_layout = global_layout_cache::get_layout(&cache_key);
            let global_stale = global_layout
                .as_ref()
                .map(|layout| layout.is_stale_for_message(content_fingerprint))
                .unwrap_or(false);
            // A global entry built from another split's snapshot of this
            // message is not stale; only channel-level staleness evicts it.
            let global_stale_for_channel = global_layout
                .as_ref()
                .is_some_and(|layout| layout.is_stale_for_channel());
            let missing_current_width_layout =
                self.layout_store.runtime.refresh_missing_layouts_for_width
                    && local_layout.is_none()
                    && global_layout.is_none();

            if !local_stale && !global_stale && !missing_current_width_layout {
                continue;
            }
            if local_layout.is_some() && !local_stale && !global_stale_for_channel {
                continue;
            }

            self.layout_store
                .runtime
                .stale_layout_refresh_pass_found_stale = true;

            if let Some(layout) =
                local_layout.filter(|layout| !layout.is_stale_for_message(content_fingerprint))
            {
                global_layout_cache::GLOBAL_LAYOUT_CACHE.remove(&cache_key);
                global_layout_cache::insert_layout(cache_key.clone(), layout);
                cache_repairs += 1;
                continue;
            }

            if let Some(layout) =
                global_layout.filter(|layout| !layout.is_stale_for_message(content_fingerprint))
            {
                if local_stale {
                    self.layout_store.layouts.pop(&cache_key);
                }
                Self::cache_layout_impl(
                    &mut self.layout_store.layouts,
                    &mut self.layout_store.runtime.cached_width_index,
                    cache_key,
                    Arc::clone(&layout),
                );
                if self.update_height_preserving_viewport_anchor(
                    idx,
                    layout.height,
                    viewport_anchor_start,
                    was_at_bottom,
                ) {
                    heights_updated += 1;
                }
                cache_repairs += 1;
                continue;
            }

            let in_visible = idx >= viewport_start && idx < viewport_end;
            if in_visible && sync_visible_rebuilt < STALE_VISIBLE_SYNC_REFRESH_BUDGET {
                if local_stale {
                    self.layout_store.layouts.pop(&cache_key);
                }
                if global_stale_for_channel {
                    global_layout_cache::GLOBAL_LAYOUT_CACHE.remove(&cache_key);
                }

                let mut layout = build_message_layout(
                    msg,
                    &self.channel_name,
                    effective_layout_width,
                    self.show_timestamp,
                    Some(window),
                );
                layout.mark_emote_versions_current(current_emote_version);
                let new_height = layout.height;
                self.cache_layout(cache_key, layout);
                if self.update_height_preserving_viewport_anchor(
                    idx,
                    new_height,
                    viewport_anchor_start,
                    was_at_bottom,
                ) {
                    heights_updated += 1;
                }
                sync_visible_rebuilt += 1;
            } else {
                self.render_perf_frame_stats.async_layout_requests += 1;
                match self.request_async_layout(msg) {
                    LayoutStatus::Ready(layout) => {
                        let new_height = layout.height;
                        self.cache_layout_arc(cache_key, layout);
                        if self.update_height_preserving_viewport_anchor(
                            idx,
                            new_height,
                            viewport_anchor_start,
                            was_at_bottom,
                        ) {
                            heights_updated += 1;
                        }
                        cache_repairs += 1;
                    }
                    _ => async_requested += 1,
                }
            }
        }

        if heights_updated > 0 {
            self.message_model.total_height = self.message_model.height_tree.total_height();
            self.interaction_state.hitbox_manager.invalidate_cache();
            if was_at_bottom {
                self.scroll_to_bottom();
            }
        } else if sync_visible_rebuilt > 0 {
            self.interaction_state.hitbox_manager.invalidate_cache();
        }

        if wrapped {
            if !self
                .layout_store
                .runtime
                .stale_layout_refresh_pass_found_stale
            {
                self.layout_store.runtime.has_stale_layouts = false;
                self.layout_store.runtime.refresh_missing_layouts_for_width = false;
            }
            self.layout_store
                .runtime
                .stale_layout_refresh_pass_found_stale = false;
        }

        crate::log_perf_render!(
            trace,
            channel_id = %self.channel_id,
            visible_len,
            work_items = work_indices.len(),
            cache_repairs,
            sync_visible_rebuilt,
            async_requested,
            heights_updated,
            wrapped,
            stale_remaining = self.layout_store.runtime.has_stale_layouts,
            refresh_missing_width = self.layout_store.runtime.refresh_missing_layouts_for_width,
            "Incremental stale-layout refresh"
        );
    }
}
