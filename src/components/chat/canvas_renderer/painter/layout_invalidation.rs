//! Viewport size and split-local layout invalidation helpers.

use super::*;

impl CanvasMessageList {
    /// Apply a new viewport height under the scroll-anchor contract.
    ///
    /// The composer surface changes height for reasons the user did not ask for
    /// (a reply bar opening, a draft wrapping onto a second line), so the frame
    /// that shrinks or grows the canvas must not move what the reader is
    /// looking at:
    ///
    /// - Bottom-following canvases repin to the bottom in the same layout
    ///   cycle, through the pending flag the layout pipeline consumes.
    /// - Scrolled-back canvases keep the *identity* of their top visible
    ///   message plus the fraction of that row already scrolled past. A raw
    ///   offset only survives while nothing else moves; an id-based anchor also
    ///   survives a history page or a synthetic row landing in the same cycle.
    pub fn set_viewport_height(&mut self, height: f32) {
        let height_diff = height - self.viewport_height;
        if height_diff.abs() > 1.0 {
            let was_at_bottom = self.is_at_bottom();
            let scroll_anchor = if was_at_bottom || self.scroll_controller.state.scroll_animating {
                None
            } else {
                self.capture_viewport_scroll_anchor()
            };
            self.viewport_height = height;

            self.interaction_state
                .hitbox_manager
                .set_viewport_height(height);

            if was_at_bottom {
                self.message_state.pending_scroll_ratio = None;
                self.message_state.pending_scroll_to_bottom = true;
            } else if let Some(anchor) = scroll_anchor
                && self.restore_viewport_scroll_anchor(anchor)
            {
                self.message_state.pending_scroll_ratio = None;
            }
        }
    }

    #[track_caller]
    #[inline]
    pub(super) fn note_relayout_cause(&mut self, kind: &'static str) {
        if relayout_trace::enabled() {
            self.layout_store.runtime.relayout_cause = Some(RelayoutCause::here(kind));
        }
    }

    /// Invalidate all cached layouts (e.g., when emote scale changes)
    /// Call this when settings that affect layout change
    /// Layouts are cleared atomically in update_layouts() to avoid visible blank state
    #[track_caller]
    pub fn invalidate_local_layouts(&mut self) {
        let was_at_bottom = self.is_at_bottom();
        self.note_relayout_cause("invalidate_local_layouts");

        self.interaction_state.hitbox_manager.invalidate_cache();
        self.message_state.needs_layout_rebuild = true;
        self.layout_store.runtime.has_stale_layouts = true;
        self.layout_store.runtime.last_checked_emote_version = 0;
        self.layout_store.runtime.last_checked_channel_emote_version = 0;
        self.cleanup_width_index();

        if was_at_bottom {
            self.message_state.pending_scroll_to_bottom = true;
        }

        crate::log_ui!(
            debug,
            channel_id = %self.channel_id,
            "Marked local layouts for rebuild"
        );
    }

    /// Evict every cached layout for one user's decoration update, but repaint
    /// this split immediately only when one of that user's rows is visible.
    ///
    /// 7TV cosmetics resolve during shaping rather than changing the message
    /// snapshot. Merely setting `needs_layout_rebuild` leaves the old unpainted
    /// layout reusable from the local/global caches, which creates a plain-text
    /// gap between older and newly arriving painted rows. Removing every width
    /// variant makes offscreen rows heal when revisited without forcing an
    /// unrelated visible split to rebuild now.
    #[track_caller]
    pub fn invalidate_user_decoration_layouts(
        &mut self,
        user_login: &str,
        user_id: Option<&str>,
    ) -> bool {
        let login = user_login
            .trim()
            .trim_start_matches('@')
            .to_ascii_lowercase();
        let user_id = user_id.map(str::trim).filter(|id| !id.is_empty());

        let matching_message_ids: std::collections::HashSet<Arc<str>> = self
            .message_model
            .messages
            .iter()
            .filter(|message| {
                user_id.is_some_and(|target_user_id| {
                    message
                        .user_id
                        .as_deref()
                        .is_some_and(|message_user_id| message_user_id == target_user_id)
                }) || (!login.is_empty() && message.sender.eq_ignore_ascii_case(&login))
            })
            .map(|message| Arc::clone(&message.id))
            .collect();

        if matching_message_ids.is_empty() {
            return false;
        }

        let visible_matches = if !self.viewport_state.visible_render_rows.is_empty() {
            self.viewport_state
                .visible_render_rows
                .iter()
                .any(|row| self.message_index_matches_user(row.message_index, &login, user_id))
        } else {
            let (start, end) = self.get_visible_range();
            (start..end).any(|idx| self.message_index_matches_user(idx, &login, user_id))
        };

        let removed_global =
            global_layout_cache::remove_layouts_for_message_ids(&matching_message_ids);
        self.async_layout_manager
            .forget_messages(&matching_message_ids);
        let local_keys: Vec<LayoutCacheKey> = self
            .layout_store
            .layouts
            .keys()
            .filter(|key| matching_message_ids.contains(key.message_id.as_ref()))
            .cloned()
            .collect();
        for key in &local_keys {
            self.layout_store.layouts.pop(key);
        }
        self.cleanup_width_index();

        crate::log_perf_render!(
            debug,
            channel_id = %self.channel_id,
            messages = matching_message_ids.len(),
            removed_global,
            removed_local = local_keys.len(),
            visible = visible_matches,
            "Invalidated resolved user decoration layouts"
        );

        if !visible_matches {
            return false;
        }

        self.invalidate_local_layouts();
        self.note_relayout_cause("user_decorations");
        true
    }

    /// Whether a badge-decoration change affects this split. `ChatState`
    /// already recorded one channel eviction for the change; this split, like
    /// every other split of the channel, drops its layouts when it replays it.
    pub fn invalidate_badge_decorations(&self) -> bool {
        !self.message_model.messages.is_empty()
    }

    /// Replay message-scoped evictions from other code paths against this
    /// split's local cache, dropping only the affected layouts. Returns whether
    /// this split lost any layout, in which case its retained visible rows
    /// must not be reused.
    pub(super) fn apply_layout_evictions(&mut self) -> bool {
        let evictions = match global_layout_cache::layout_evictions_since(
            self.layout_store.runtime.last_layout_eviction_seq,
        ) {
            global_layout_cache::LayoutEvictions::Replay { evictions, latest } => {
                self.layout_store.runtime.last_layout_eviction_seq = latest;
                evictions
            }
            global_layout_cache::LayoutEvictions::Overflow { latest } => {
                self.layout_store.runtime.last_layout_eviction_seq = latest;
                self.layout_store.layouts.clear();
                self.layout_store.runtime.cached_width_index.clear();
                self.layout_store.runtime.restart_width_refresh();
                self.interaction_state.hitbox_manager.invalidate_cache();
                self.viewport_state.cached_visible_render_range = None;
                self.note_relayout_cause("layout_evictions_overflow");
                return true;
            }
        };

        let mut keys = Vec::new();
        for eviction in &evictions {
            match eviction {
                global_layout_cache::LayoutEviction::Message(message_id) => {
                    let message_id = SharedString::from(Arc::clone(message_id));
                    if let Some(widths) = self
                        .layout_store
                        .runtime
                        .cached_width_index
                        .get(&message_id)
                    {
                        keys.extend(widths.iter().map(|&(quantized_width, show_timestamp)| {
                            LayoutCacheKey {
                                message_id: message_id.clone(),
                                quantized_width,
                                show_timestamp,
                            }
                        }));
                    }
                }
                global_layout_cache::LayoutEviction::LinksTo(url) => {
                    let layouts = &self.layout_store.layouts;
                    keys.extend(
                        layouts
                            .keys()
                            .filter(|key| {
                                layouts.peek(key).is_some_and(|layout| layout.links_to(url))
                            })
                            .cloned(),
                    );
                }
                global_layout_cache::LayoutEviction::Channel(_) => {}
            }
        }

        if evictions.iter().any(|eviction| {
            matches!(eviction, global_layout_cache::LayoutEviction::Channel(channel)
                if channel.as_ref() == self.channel_name)
        }) {
            for key in &keys {
                global_layout_cache::GLOBAL_LAYOUT_CACHE.remove(key);
            }
            self.layout_store.layouts.clear();
            self.layout_store.runtime.cached_width_index.clear();
            self.invalidate_local_layouts();
            self.viewport_state.cached_visible_render_range = None;
            self.note_relayout_cause("channel_layout_eviction");
            return true;
        }

        if keys.is_empty() {
            return false;
        }

        for key in &keys {
            self.layout_store.layouts.pop(key);
            global_layout_cache::GLOBAL_LAYOUT_CACHE.remove(key);
        }
        self.cleanup_width_index();
        self.interaction_state.hitbox_manager.invalidate_cache();
        self.viewport_state.cached_visible_render_range = None;
        self.note_relayout_cause("layout_evictions");
        crate::log_perf_render!(
            debug,
            channel_id = %self.channel_id,
            evictions = evictions.len(),
            removed_local = keys.len(),
            "Applied message-scoped layout evictions"
        );
        true
    }

    pub(super) fn message_index_matches_user(
        &self,
        idx: usize,
        login: &str,
        user_id: Option<&str>,
    ) -> bool {
        let Some(message) = self.message_model.messages.get(idx) else {
            return false;
        };

        if let Some(target_user_id) = user_id
            && message
                .user_id
                .as_deref()
                .is_some_and(|message_user_id| message_user_id == target_user_id)
        {
            return true;
        }

        !login.is_empty() && message.sender.eq_ignore_ascii_case(login)
    }
}
