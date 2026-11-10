//! Layout update coordinator for message order, height tree, and scroll restoration.

use super::layout_resolution::LayoutHeightMode;
use super::*;

impl CanvasMessageList {
    pub(super) fn update_layouts(&mut self, window: &mut Window) {
        let _layout_context = self.enter_layout_context();
        let mut force_layout_refresh = false;
        let mut order_tree_cleared_for_rebuild = false;
        let mut rebuild_scroll_anchor = None;
        let sync_layout_budget_deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(SYNC_LAYOUT_BUDGET_MS);
        let mut sync_layout_budget_exhausted = false;
        let mut allow_sync_layout = || {
            if sync_layout_budget_exhausted {
                return false;
            }
            if std::time::Instant::now() < sync_layout_budget_deadline {
                true
            } else {
                sync_layout_budget_exhausted = true;
                false
            }
        };

        let current_layout_settings_version = settings::get_layout_settings_version();
        if current_layout_settings_version
            != self.layout_store.runtime.last_layout_settings_version_seen
        {
            rebuild_scroll_anchor = self.capture_viewport_scroll_anchor();
            self.layout_store.runtime.last_layout_settings_version_seen =
                current_layout_settings_version;

            force_layout_refresh = true;
            self.interaction_state.hitbox_manager.invalidate_cache();
            self.note_relayout_cause("settings_version");

            self.message_state.needs_layout_rebuild = true;
            self.layout_store.runtime.has_stale_layouts = true;
            self.layout_store.runtime.last_checked_emote_version = 0;
            self.layout_store.runtime.last_checked_channel_emote_version = 0;
        }

        let scale_bits = self.layout_scale_factor.to_bits();
        let mod_buttons_changed =
            self.mod_buttons_active != self.layout_store.runtime.last_layout_mod_buttons;
        if scale_bits != self.layout_store.runtime.last_layout_scale_bits || mod_buttons_changed {
            self.layout_store.runtime.last_layout_scale_bits = scale_bits;
            self.layout_store.runtime.last_layout_mod_buttons = self.mod_buttons_active;
            if rebuild_scroll_anchor.is_none() {
                rebuild_scroll_anchor = self.capture_viewport_scroll_anchor();
            }
            force_layout_refresh = true;
            self.interaction_state.hitbox_manager.invalidate_cache();
            self.note_relayout_cause(if mod_buttons_changed {
                "mod_mode"
            } else {
                "scale_factor"
            });
            self.layout_store.layouts.clear();
            self.layout_store.runtime.cached_width_index.clear();
            self.message_state.needs_layout_rebuild = true;
            self.layout_store.runtime.has_stale_layouts = true;
            self.layout_store.runtime.last_checked_emote_version = 0;
            self.layout_store.runtime.last_checked_channel_emote_version = 0;
        }

        self.maybe_apply_stabilized_width_bucket_change();

        let msg_count = self.message_model.messages.len();
        let current_quantized_width = self.layout_quantized_width_for_cache();
        let effective_layout_width = self.layout_effective_width_for_builds();

        let eviction_seq = global_layout_cache::layout_eviction_seq();
        let global_version = global_layout_cache::get_invalidation_version();
        if msg_count == 0 {
            if global_version != self.layout_store.runtime.last_invalidation_version
                || eviction_seq != self.layout_store.runtime.last_layout_eviction_seq
            {
                self.layout_store.layouts.clear();
                self.layout_store.runtime.cached_width_index.clear();
            }
            self.layout_store.runtime.last_invalidation_version = global_version;
            self.layout_store.runtime.last_layout_eviction_seq = eviction_seq;
            self.note_message_order_reset("update_layouts.empty_messages");
            self.message_model.message_order.clear();
            self.message_model.height_tree.clear();
            self.message_model.total_height = 0.0;
            return;
        }

        if global_version != self.layout_store.runtime.last_invalidation_version {
            self.layout_store.runtime.last_invalidation_version = global_version;
            self.layout_store.runtime.last_layout_eviction_seq = eviction_seq;
            self.note_relayout_cause("global_invalidation");
            self.layout_store.layouts.clear();
            self.layout_store.runtime.cached_width_index.clear();
            if force_layout_refresh {
                self.message_state.needs_layout_rebuild = true;
            } else {
                self.layout_store.runtime.refresh_missing_layouts_for_width = true;
                self.layout_store.runtime.stale_layout_refresh_cursor = 0;
                self.layout_store
                    .runtime
                    .stale_layout_refresh_pass_found_stale = false;
                self.interaction_state.hitbox_manager.invalidate_cache();
            }
            self.layout_store.runtime.has_stale_layouts = true;
            self.layout_store.runtime.last_checked_emote_version = 0;
            self.layout_store.runtime.last_checked_channel_emote_version = 0;
        } else if eviction_seq != self.layout_store.runtime.last_layout_eviction_seq {
            self.apply_layout_evictions();
        }

        if self.message_state.needs_layout_rebuild {
            let order_matches_messages = self.message_model.message_order.len() == msg_count
                && self.message_model.height_tree.len() == msg_count
                && self
                    .message_model
                    .message_order
                    .iter()
                    .zip(self.message_model.messages.iter())
                    .all(|(msg_id, msg)| msg_id.as_ref() == msg.id.as_ref());

            if order_matches_messages && !force_layout_refresh {
                self.message_state.needs_layout_rebuild = false;
                self.layout_store.runtime.restart_width_refresh();
                self.interaction_state.hitbox_manager.invalidate_cache();
                crate::log_ui!(
                    debug,
                    channel_id = %self.channel_id,
                    msg_count,
                    "Converted queued layout rebuild to progressive refresh"
                );
            } else {
                if rebuild_scroll_anchor.is_none() {
                    rebuild_scroll_anchor = self.capture_viewport_scroll_anchor();
                }
                self.message_state.needs_layout_rebuild = false;
                self.note_message_order_reset("update_layouts.needs_layout_rebuild");
                self.message_model.message_order.clear();
                self.message_model.height_tree.clear();
                self.message_state.pending_new_message_count = 0;
                order_tree_cleared_for_rebuild = true;
            }
        }

        if self.message_state.pending_new_message_count > 0 {
            let current_emote_version = crate::backend::slots::resolver::get_emote_version();
            let current_channel_emote_version =
                crate::state::VersionRegistry::channel_emote_version(&self.channel_name);
            let new_start = self
                .message_model
                .message_order
                .len()
                .saturating_sub(self.message_state.pending_new_message_count);
            let messages = self.message_model.messages.clone();

            for idx in new_start..self.message_model.message_order.len() {
                if let Some(msg) = messages.get(idx) {
                    let Some(shared_id) = self.message_model.message_order.get(idx).cloned() else {
                        continue;
                    };
                    let cache_key = LayoutCacheKey::from_shared_quantized(
                        shared_id,
                        current_quantized_width,
                        self.show_timestamp,
                    );
                    let height = self.resolve_layout_height(
                        msg,
                        cache_key,
                        LayoutHeightMode::Incremental,
                        effective_layout_width,
                        current_emote_version,
                        current_channel_emote_version,
                        window,
                        &mut allow_sync_layout,
                    );
                    self.message_model.height_tree.push(height);
                }
            }
            self.message_state.pending_new_message_count = 0;

            self.message_model.total_height = self.message_model.height_tree.total_height();

            if self.message_state.pending_scroll_to_bottom {
                self.scroll_to_bottom();
                self.message_state.pending_scroll_to_bottom = false;
            }

            return;
        }

        let current_len = self.message_model.message_order.len();

        let current_emote_version = crate::backend::slots::resolver::get_emote_version();
        let current_channel_emote_version =
            crate::state::VersionRegistry::channel_emote_version(&self.channel_name);

        let version_changed = current_channel_emote_version
            != self.layout_store.runtime.last_checked_channel_emote_version;

        let has_potentially_stale_layouts = if version_changed {
            crate::log_emotes!(
                debug,
                "Channel emote version changed for '{}' ({} -> {}, global observed at {}), preserving existing layouts",
                self.channel_name,
                self.layout_store.runtime.last_checked_channel_emote_version,
                current_channel_emote_version,
                current_emote_version
            );
            self.layout_store.runtime.last_checked_emote_version = current_emote_version;
            self.layout_store.runtime.last_checked_channel_emote_version =
                current_channel_emote_version;
            self.layout_store.runtime.has_stale_layouts = true;
            self.layout_store.runtime.stale_layout_refresh_cursor = 0;
            self.layout_store
                .runtime
                .stale_layout_refresh_pass_found_stale = false;
            true
        } else {
            self.layout_store.runtime.has_stale_layouts
        };

        if current_len > 0 && msg_count > current_len {
            let first_matches = self
                .message_model
                .messages
                .first()
                .map(|m| {
                    self.message_model.message_order.first().map(|s| s.as_ref())
                        == Some(m.id.as_ref())
                })
                .unwrap_or(false);

            if first_matches {
                let new_count = msg_count - current_len;

                self.message_model.message_order.reserve(new_count);
                self.message_model.height_tree.reserve(new_count);

                let messages = self.message_model.messages.clone();
                for msg in messages.iter().skip(current_len) {
                    let shared_id = SharedString::from(msg.id.clone());
                    self.message_model.message_order.push(shared_id.clone());

                    let cache_key = LayoutCacheKey::from_shared_quantized(
                        shared_id,
                        current_quantized_width,
                        self.show_timestamp,
                    );

                    let height = self.resolve_layout_height(
                        msg,
                        cache_key,
                        LayoutHeightMode::Incremental,
                        effective_layout_width,
                        current_emote_version,
                        current_channel_emote_version,
                        window,
                        &mut allow_sync_layout,
                    );
                    self.message_model.height_tree.push(height);
                }

                self.message_model.total_height = self.message_model.height_tree.total_height();

                self.apply_pending_scroll_target();
                return;
            }
        }

        let order_mismatch = self.message_model.message_order.len() != msg_count;
        let height_mismatch = self.message_model.height_tree.len() != msg_count;

        if has_potentially_stale_layouts
            && !order_mismatch
            && !height_mismatch
            && !force_layout_refresh
        {
            self.refresh_stale_layouts_visible_first(
                window,
                current_emote_version,
                effective_layout_width,
                current_quantized_width,
            );
        }

        if order_mismatch || height_mismatch || force_layout_refresh {
            self.layout_store.runtime.has_stale_layouts = false;
            self.layout_store.runtime.refresh_missing_layouts_for_width = false;
            self.layout_store.runtime.stale_layout_refresh_cursor = 0;
            self.layout_store
                .runtime
                .stale_layout_refresh_pass_found_stale = false;
            crate::log_ui!(debug,
                channel_id = %self.channel_id,
                msg_count = msg_count,
                order_len = self.message_model.message_order.len(),
                height_len = self.message_model.height_tree.len(),
                order_mismatch = order_mismatch,
                height_mismatch = height_mismatch,
                "Full layout rebuild"
            );

            let mut rebuild_reason_flags = 0u8;
            if order_mismatch {
                rebuild_reason_flags |= REBUILD_REASON_ORDER_MISMATCH;
            }
            if height_mismatch {
                rebuild_reason_flags |= REBUILD_REASON_HEIGHT_MISMATCH;
            }
            if has_potentially_stale_layouts {
                rebuild_reason_flags |= REBUILD_REASON_STALE_LAYOUTS;
            }
            if force_layout_refresh {
                rebuild_reason_flags |= REBUILD_REASON_FORCE_REFRESH;
            }
            self.note_full_layout_rebuild(rebuild_reason_flags);

            let preserved_visible_range = if self.message_model.message_order.len() == msg_count
                && self.message_model.height_tree.len() == msg_count
            {
                Some(self.get_visible_range())
            } else {
                None
            };

            if !order_tree_cleared_for_rebuild {
                self.note_message_order_reset("update_layouts.full_layout_rebuild");
                self.message_model.message_order.clear();
                self.message_model.height_tree.clear();
            }

            let messages = self.message_model.messages.clone();
            let prime_tail_for_bottom = self.message_state.pending_scroll_to_bottom
                || self.scroll_controller.state.auto_scroll;
            let prime_range = Self::full_rebuild_prime_range(
                msg_count,
                prime_tail_for_bottom,
                preserved_visible_range,
            );
            for (idx, msg) in messages.iter().enumerate() {
                let should_prime_layout = Self::should_prime_full_rebuild_layout(idx, prime_range);
                let shared_id = SharedString::from(msg.id.clone());
                self.message_model.message_order.push(shared_id.clone());

                let cache_key = LayoutCacheKey::from_shared_quantized(
                    shared_id,
                    current_quantized_width,
                    self.show_timestamp,
                );

                crate::log_perf_render!(
                    trace,
                    channel_id = %self.channel_id,
                    msg_id = %msg.id,
                    container_width = self.container_width,
                    quantized = cache_key.quantized_width,
                    "Looking up layout"
                );

                let content_fingerprint = message_content_fingerprint(msg);
                let local_stale = self
                    .layout_store
                    .layouts
                    .peek(&cache_key)
                    .map(|layout| layout.is_stale_for_message(content_fingerprint))
                    .unwrap_or(false);

                let global_layout = global_layout_cache::get_layout(&cache_key);
                let global_stale = global_layout
                    .as_ref()
                    .map(|layout| layout.is_stale_for_message(content_fingerprint))
                    .unwrap_or(false);

                let should_rebuild = local_stale || global_stale;

                if should_rebuild && !force_layout_refresh {
                    crate::log_emotes!(
                        info,
                        "Rebuilding stale layout for msg {} (local_stale={}, global_stale={})",
                        msg.id,
                        local_stale,
                        global_stale
                    );
                    if local_stale {
                        self.layout_store.layouts.pop(&cache_key);
                    }
                    if global_layout.is_some_and(|layout| layout.is_stale_for_channel()) {
                        global_layout_cache::GLOBAL_LAYOUT_CACHE.remove(&cache_key);
                    }
                }

                let height = self.resolve_layout_height(
                    msg,
                    cache_key,
                    LayoutHeightMode::FullRebuild {
                        force_layout_refresh,
                        should_prime_layout,
                    },
                    effective_layout_width,
                    current_emote_version,
                    current_channel_emote_version,
                    window,
                    &mut allow_sync_layout,
                );
                self.message_model.height_tree.push(height);
            }

            self.message_model.total_height = self.message_model.height_tree.total_height();
            crate::log_ui!(debug,
                channel_id = %self.channel_id,
                total_height = self.message_model.total_height,
                layout_count = self.layout_store.layouts.len(),
                "Layout rebuild complete"
            );
        }

        let restored_identity_anchor =
            rebuild_scroll_anchor.is_some_and(|anchor| self.restore_viewport_scroll_anchor(anchor));
        if restored_identity_anchor {
            self.message_state.pending_scroll_ratio = None;
            self.message_state.pending_scroll_to_bottom = false;
        } else {
            self.apply_pending_scroll_target();
        }
    }

    /// Apply a queued scroll ratio, else a queued scroll-to-bottom.
    fn apply_pending_scroll_target(&mut self) {
        if let Some(ratio) = self.message_state.pending_scroll_ratio.take() {
            let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
            self.scroll_controller.offset = (ratio * max_scroll).max(0.0).min(max_scroll);
            self.message_state.pending_scroll_to_bottom = false;
        } else if self.message_state.pending_scroll_to_bottom {
            self.scroll_to_bottom();
            self.message_state.pending_scroll_to_bottom = false;
        }
    }
}
