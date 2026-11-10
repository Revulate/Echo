//! How a `CanvasMessageList` requests layouts from the shared
//! `AsyncLayoutManager` and adopts the ones that finished.

use super::CanvasMessageList;
use crate::backend::ChatMessage;
use crate::components::chat::canvas_renderer::async_layout::{
    LayoutRequest, LayoutStatus, LayoutVersions,
};
use crate::components::chat::canvas_renderer::layout::{
    LayoutCacheKey, message_content_fingerprint, width_from_quantized,
};
use crate::components::chat::canvas_renderer::{global_layout_cache, layout_mode, layout_scale};
use gpui::SharedString;
use std::sync::Arc;

impl CanvasMessageList {
    /// Adopt the async layouts that finished since the last poll; called by
    /// the async-layout waker. Returns how many completed.
    pub fn poll_async_layouts(&mut self) -> usize {
        let _layout_context = self.enter_layout_context();
        let completed_ids = self.async_layout_manager.poll_completed();
        let count = completed_ids.len();

        let mut heights_updated = 0;
        let was_at_bottom = self.is_at_bottom();
        let (anchor_start_idx, _) = self.get_viewport_range();

        for msg_id in completed_ids {
            let Some(content_fingerprint) =
                self.get_message(&msg_id).map(message_content_fingerprint)
            else {
                continue;
            };
            let versions = LayoutVersions {
                show_timestamp: self.show_timestamp,
                content_fingerprint,
                layout_settings_version: crate::backend::settings::get_layout_settings_version(),
                emote_resolution_version: crate::backend::slots::resolver::get_emote_version(),
                channel_emote_version: crate::state::VersionRegistry::channel_emote_version(
                    &self.channel_name,
                ),
            };
            if let Some((layout, quantized_width)) =
                self.async_layout_manager.get_layout(&msg_id, &versions)
            {
                let cache_key = LayoutCacheKey {
                    message_id: SharedString::from(Arc::clone(&msg_id)),
                    quantized_width,
                    show_timestamp: self.show_timestamp,
                };

                let fresh_existing_layout = self
                    .peek_cached_layout(&cache_key)
                    .filter(|cached| {
                        cached.uses_window_text_metrics
                            && !cached.is_stale_for_message(content_fingerprint)
                    })
                    .or_else(|| {
                        global_layout_cache::get_layout(&cache_key).filter(|cached| {
                            cached.uses_window_text_metrics
                                && !cached.is_stale_for_message(content_fingerprint)
                        })
                    });
                if let Some(existing_layout) = fresh_existing_layout {
                    if let Some(idx) = self.get_message_index(&msg_id) {
                        let new_height = existing_layout.height;
                        if self.update_height_preserving_viewport_anchor(
                            idx,
                            new_height,
                            anchor_start_idx,
                            was_at_bottom,
                        ) {
                            heights_updated += 1;
                        }
                    }
                    continue;
                }

                self.cache_layout_arc(cache_key, Arc::clone(&layout));
                self.interaction_state.hitbox_manager.invalidate_cache();

                if let Some(idx) = self.get_message_index(&msg_id) {
                    let new_height = layout.height;
                    if self.update_height_preserving_viewport_anchor(
                        idx,
                        new_height,
                        anchor_start_idx,
                        was_at_bottom,
                    ) {
                        heights_updated += 1;
                    }
                }
            }
        }

        if heights_updated > 0 {
            self.sync_total_height_from_tree();

            if was_at_bottom {
                self.scroll_to_bottom();
            }

            crate::log_perf_render!(
                trace,
                "Updated heights incrementally: channel={}, completed={}, heights_updated={}",
                self.channel_id,
                count,
                heights_updated
            );
        }

        count
    }

    /// Request async layout computation for a message
    /// Returns the current status (Pending, Computing, or Ready)
    pub fn request_async_layout(&self, msg: &Arc<ChatMessage>) -> LayoutStatus {
        let requested_at_eviction_seq = global_layout_cache::layout_eviction_seq();
        let _layout_context = self.enter_layout_context();
        let q = self.layout_quantized_width_for_cache();
        let available_width = if q == 0 {
            self.container_width
        } else {
            width_from_quantized(q)
        };

        let cache_key = LayoutCacheKey::from_shared_quantized(
            SharedString::from(Arc::clone(&msg.id)),
            q,
            self.show_timestamp,
        );
        let content_fingerprint = message_content_fingerprint(msg);
        if let Some(layout) = global_layout_cache::get_layout(&cache_key)
            && !layout.is_stale_for_message(content_fingerprint)
        {
            return LayoutStatus::Ready(layout);
        }

        let versions = LayoutVersions {
            show_timestamp: self.show_timestamp,
            content_fingerprint,
            layout_settings_version: crate::backend::settings::get_layout_settings_version(),
            emote_resolution_version: crate::backend::slots::resolver::get_emote_version(),
            channel_emote_version: crate::state::VersionRegistry::channel_emote_version(
                &self.channel_name,
            ),
        };
        let existing =
            self.async_layout_manager
                .get_exact_status(&msg.id, q, &self.channel_name, &versions);
        if !matches!(existing, LayoutStatus::Pending | LayoutStatus::Failed) {
            return existing;
        }

        self.async_layout_manager.request_layout(LayoutRequest {
            message_id: Arc::clone(&msg.id),
            message: Arc::clone(msg),
            channel: Arc::from(self.channel_name.as_str()),
            available_width,
            show_timestamp: self.show_timestamp,
            layout_settings_version: versions.layout_settings_version,
            emote_resolution_version: versions.emote_resolution_version,
            channel_emote_version: versions.channel_emote_version,
            scale_factor: layout_scale::current(),
            mod_buttons: layout_mode::mod_buttons(),
            requested_at_eviction_seq,
        })
    }

    /// Get async layout status for a message
    pub fn get_async_layout_status(&self, msg: &ChatMessage) -> LayoutStatus {
        let _layout_context = self.enter_layout_context();
        self.async_layout_manager.get_status(
            &msg.id,
            &LayoutVersions {
                show_timestamp: self.show_timestamp,
                content_fingerprint: message_content_fingerprint(msg),
                layout_settings_version: crate::backend::settings::get_layout_settings_version(),
                emote_resolution_version: crate::backend::slots::resolver::get_emote_version(),
                channel_emote_version: crate::state::VersionRegistry::channel_emote_version(
                    &self.channel_name,
                ),
            },
        )
    }
}
