//! Message-buffer lifecycle and split activity handling.

use super::message_delta::{MessageDeltaPlan, ids_align, same_count_rotation};
use super::*;

/// Rows on each side of the last viewport whose layouts a split keeps while
/// its tab is hidden. See `CanvasMessageList::trim_layouts_for_hidden_tab`.
const HIDDEN_TAB_LAYOUT_MARGIN_ROWS: usize = 64;
/// Newest prepended history rows to pre-layout first.
///
/// These are the rows immediately above the pre-existing viewport after a
/// history prepend, so resolving them early prevents scrollback spacing from
/// changing as soon as the user scrolls upward.
const HISTORY_PREPEND_ASYNC_PRIME_LIMIT: usize = 192;

/// Row identities around the viewport captured before a message-buffer
/// replacement, so the view can be re-anchored on retained content.
struct RetainedViewportAnchor {
    visible: Vec<(SharedString, f32)>,
    previous_messages: Arc<Vec<Arc<ChatMessage>>>,
    viewport_start: usize,
    viewport_end: usize,
}

impl CanvasMessageList {
    /// Whether this retained split currently belongs to the mounted tab.
    pub fn is_tab_visible(&self) -> bool {
        self.is_tab_visible
    }

    /// Set whether this split belongs to the tab currently mounted in the main window.
    ///
    /// Unlike `is_active_split`, this is true for every sibling split in the visible
    /// tab. Hiding a tab stops its detached animation loop while retaining message,
    /// scroll, and prepared-row state, plus the layouts around the viewport, for an
    /// instant switch back.
    pub fn set_tab_visible(&mut self, visible: bool) -> bool {
        if self.is_tab_visible == visible {
            return false;
        }

        self.is_tab_visible = visible;
        if visible {
            self.revalidate_on_show();
        } else {
            self.mark_last_read();
            self.trim_layouts_for_hidden_tab();
            self.clear_chat_pause_for_hidden_tab();
            self.wake_state.has_visible_animations_last_frame = false;
            if let Some(owner) = self.wake_state.animation_census_owner {
                crate::emote_renderer::remove_owner(owner);
            }
            if self.wake_state.animation_waker_running {
                crate::backend::metrics::record_emote_event(
                    "canvas.animation_waker.hidden_tab_release",
                );
                crate::log_perf_render!(
                    debug,
                    channel_id = %self.channel_id,
                    "Released animation waker for inactive tab split"
                );
                self.release_animation_waker();
            }
        }

        true
    }

    /// Drop this split's layouts except those around the last viewport.
    ///
    /// RAM: a hidden split does no layout work, so everything it cached stays
    /// pinned (several KB per layout) until the tab is shown again,
    /// including layouts of rows the buffer has since evicted. In a busy
    /// channel the eviction log usually overflows while hidden, which clears
    /// the cache on show anyway. Keeping the rows around the viewport
    /// preserves the instant repaint on switch-back; rows further away come
    /// from the shared layout cache or are laid out again on scroll.
    fn trim_layouts_for_hidden_tab(&mut self) {
        let order = &self.message_model.message_order;
        if order.is_empty() || self.layout_store.layouts.is_empty() {
            return;
        }
        let last = order.len() - 1;
        let (start, end) = self
            .viewport_state
            .cached_visible_render_range
            .unwrap_or((last, last));
        let keep_start = start
            .min(last)
            .saturating_sub(HIDDEN_TAB_LAYOUT_MARGIN_ROWS);
        let keep_end = end
            .saturating_add(HIDDEN_TAB_LAYOUT_MARGIN_ROWS + 1)
            .min(order.len());
        let keep: rustc_hash::FxHashSet<&SharedString> =
            order[keep_start..keep_end].iter().collect();
        let dropped: Vec<LayoutCacheKey> = self
            .layout_store
            .layouts
            .keys()
            .filter(|key| !keep.contains(&key.message_id))
            .cloned()
            .collect();
        if dropped.is_empty() {
            return;
        }
        for key in &dropped {
            self.layout_store.layouts.pop(key);
        }
        self.cleanup_width_index();
        crate::log_perf_render!(
            debug,
            channel_id = %self.channel_id,
            dropped = dropped.len(),
            kept = self.layout_store.layouts.len(),
            "Trimmed layouts for hidden tab split"
        );
    }

    /// Show or hide inline moderation buttons for this split (moderation
    /// mode). Rows relayout on the next frame; returns whether the mode
    /// changed.
    pub fn set_mod_mode(&mut self, enabled: bool) -> bool {
        let changed = self.mod_mode != enabled;
        self.mod_mode = enabled;
        changed
    }

    pub fn mod_mode(&self) -> bool {
        self.mod_mode
    }

    /// Remember the newest message as read; called on tab switch and window
    /// deactivation.
    pub fn mark_last_read(&mut self) {
        self.message_state.last_read_id = self.message_model.message_order.last().cloned();
    }

    /// Set whether this split is active.
    ///
    /// The laid-out geometry is RETAINED across split focus changes (and
    /// around the viewport across tab switches, see `set_tab_visible`) so
    /// switching back is a cache hit and paints instantly, instead of
    /// cold-reshaping the visible range on the activation frame. Memory is
    /// reclaimed through the decoded-media TTL instead, so the layout cache
    /// deliberately does NOT shrink for inactive splits. `is_active_split`
    /// still gates background async priming (inactive splits lay out lazily
    /// on show).
    pub fn set_active(&mut self, active: bool) -> bool {
        let changed = self.is_active_split != active;
        self.is_active_split = active;
        changed
    }

    #[inline]
    fn cached_or_estimated_height(&mut self, shared_id: SharedString) -> f32 {
        let cache_key = self.layout_cache_key_from_shared(shared_id);
        self.get_cached_layout(&cache_key)
            .map(|layout| layout.height)
            .unwrap_or_else(|| self.message_model.estimated_row_height())
    }

    /// Rebuild the message index, order, and height tree from `messages`
    /// using cached-or-estimated heights, then restart progressive layout.
    fn rebuild_message_model_from(&mut self, messages: &[Arc<ChatMessage>]) {
        let new_count = messages.len();
        self.message_model.clear_message_index();
        self.message_model.reserve_message_index(new_count);
        for (idx, msg) in messages.iter().enumerate() {
            let shared_id = SharedString::from(msg.id.clone());
            self.message_model.insert_message_index(shared_id, idx);
        }

        self.message_model.message_order.clear();
        self.message_model.height_tree.clear();
        self.message_model.message_order.reserve(new_count);
        self.message_model.height_tree.reserve(new_count);

        for msg in messages.iter() {
            let shared_id = SharedString::from(msg.id.clone());
            let height = self.cached_or_estimated_height(shared_id.clone());
            self.message_model.message_order.push(shared_id);
            self.message_model.height_tree.push(height);
        }

        self.message_model.total_height = self.message_model.height_tree.total_height();
        self.message_state.pending_new_message_count = 0;
        self.message_state.needs_layout_rebuild = false;
        self.layout_store.runtime.restart_width_refresh();
        self.preload_state.last_emote_preload_range = None;
        self.interaction_state.hitbox_manager.invalidate_cache();
    }

    /// Replace the shared snapshot this split shows, presenting any removed
    /// rows under the split's deleted-message mode.
    pub fn set_messages(&mut self, messages: Arc<Vec<Arc<ChatMessage>>>) {
        self.set_source_messages(messages);
    }

    /// Update messages and trigger re-render
    pub(super) fn apply_messages(&mut self, messages: Arc<Vec<Arc<ChatMessage>>>) {
        let new_count = messages.len();
        let old_count = self.message_model.messages.len();

        let delta = MessageDeltaPlan::classify(
            &self.message_model.messages,
            &self.message_model.message_order,
            self.message_model.height_tree.len(),
            &messages,
        );
        let content_changed = delta.content_changed;
        let mut had_new_messages = delta.had_new_messages;
        let was_scrolled_away = !self.is_at_bottom();
        let previous_boundary_index = self
            .message_state
            .new_message_boundary_id
            .as_ref()
            .and_then(|id| self.get_message_index(id.as_ref()));
        let mut first_new_message_id =
            if was_scrolled_away && delta.count_increased && delta.old_order_matches_prefix {
                messages
                    .get(old_count)
                    .map(|message| SharedString::from(message.id.clone()))
            } else {
                None
            };
        let mut retained_viewport_anchor = None;

        if delta.count_increased {
            let added_count = delta.added_count;

            if delta.old_order_matches_prefix {
                for (idx, msg) in messages.iter().enumerate().skip(old_count) {
                    let shared_id = SharedString::from(msg.id.clone());
                    self.message_model.insert_message_index(shared_id, idx);
                }
            } else if delta.is_initial_snapshot_fast_path {
                self.message_model.message_order.clear();
                self.message_model.height_tree.clear();
                self.message_model.clear_message_index();
                self.message_model.message_order.reserve(new_count);
                self.message_model.height_tree.reserve(new_count);
                self.message_model.reserve_message_index(new_count);

                for (idx, msg) in messages.iter().enumerate() {
                    let shared_id = SharedString::from(msg.id.clone());
                    let height = self.cached_or_estimated_height(shared_id.clone());
                    self.message_model.message_order.push(shared_id.clone());
                    self.message_model.insert_message_index(shared_id, idx);
                    self.message_model.height_tree.push(height);
                }

                self.message_model.total_height = self.message_model.height_tree.total_height();
                self.message_state.pending_new_message_count = 0;
                self.message_state.needs_layout_rebuild = false;
                self.layout_store.runtime.restart_width_refresh();
                self.preload_state.last_emote_preload_range = None;
                self.interaction_state.hitbox_manager.invalidate_cache();

                let primed_async_layouts = if self.is_active_split {
                    let mut primed = 0usize;
                    for msg in messages
                        .iter()
                        .rev()
                        .take(HISTORY_PREPEND_ASYNC_PRIME_LIMIT)
                    {
                        self.request_primed_async_layout(msg);
                        primed += 1;
                    }
                    primed
                } else {
                    0
                };

                if !self.is_active_split {
                    crate::log_perf_render!(
                        trace,
                        channel_id = %self.channel_id,
                        initialized = new_count,
                        "Skipped inactive split initial-history async priming"
                    );
                }

                crate::log_perf_render!(
                    debug,
                    channel_id = %self.channel_id,
                    initialized = new_count,
                    estimated_height = self.message_model.estimated_row_height(),
                    primed_async_layouts,
                    "Initialized viewport-preserving history snapshot"
                );
            } else if delta.old_order_matches_suffix {
                let mut prepended_order = Vec::with_capacity(added_count);
                let mut prepended_heights = Vec::with_capacity(added_count);

                for msg in messages.iter().take(added_count) {
                    let shared_id = SharedString::from(msg.id.clone());
                    let height = self.cached_or_estimated_height(shared_id.clone());
                    prepended_order.push(shared_id);
                    prepended_heights.push(height);
                }

                let prepended_total_height = self
                    .message_model
                    .height_tree
                    .prepend_many(&prepended_heights);
                self.scroll_controller.offset += prepended_total_height;
                self.scroll_controller.state.scroll_desired += prepended_total_height;

                let mut new_order = Vec::with_capacity(new_count);
                new_order.extend(prepended_order);
                new_order.extend(self.message_model.message_order.iter().cloned());
                self.message_model.message_order = new_order;

                self.message_model.rebuild_message_index_from_order();

                self.message_model.total_height = self.message_model.height_tree.total_height();
                self.message_state.pending_new_message_count = 0;
                self.preload_state.last_emote_preload_range = None;
                self.interaction_state.hitbox_manager.invalidate_cache();

                let primed_async_layouts = if self.is_active_split {
                    let mut primed = 0usize;
                    for msg in messages
                        .iter()
                        .take(added_count)
                        .rev()
                        .take(HISTORY_PREPEND_ASYNC_PRIME_LIMIT)
                    {
                        self.request_primed_async_layout(msg);
                        primed += 1;
                    }
                    primed
                } else {
                    0
                };

                if !self.is_active_split {
                    crate::log_perf_render!(
                        trace,
                        channel_id = %self.channel_id,
                        prepended = added_count,
                        "Skipped inactive split history-prepend async priming"
                    );
                }

                crate::log_perf_render!(
                    debug,
                    channel_id = %self.channel_id,
                    prepended = added_count,
                    scroll_compensation = prepended_total_height,
                    primed_async_layouts,
                    "Preserved viewport across history prepend"
                );
            } else if let Some(existing_start) = delta.old_order_middle_start {
                let prepended_count = existing_start;
                let existing_end = existing_start + old_count;
                let appended_count = new_count.saturating_sub(existing_end);
                if was_scrolled_away && appended_count > 0 {
                    first_new_message_id = messages
                        .get(existing_end)
                        .map(|message| SharedString::from(message.id.clone()));
                }
                let mut prepended_order = Vec::with_capacity(prepended_count);
                let mut prepended_heights = Vec::with_capacity(prepended_count);

                for msg in messages.iter().take(prepended_count) {
                    let shared_id = SharedString::from(msg.id.clone());
                    let height = self.cached_or_estimated_height(shared_id.clone());
                    prepended_order.push(shared_id);
                    prepended_heights.push(height);
                }

                let prepended_total_height = self
                    .message_model
                    .height_tree
                    .prepend_many(&prepended_heights);
                self.scroll_controller.offset += prepended_total_height;
                self.scroll_controller.state.scroll_desired += prepended_total_height;

                let mut new_order = Vec::with_capacity(new_count);
                new_order.extend(prepended_order);
                new_order.extend(self.message_model.message_order.iter().cloned());
                for msg in messages.iter().skip(existing_end) {
                    new_order.push(SharedString::from(msg.id.clone()));
                }
                self.message_model.message_order = new_order;

                self.message_model.rebuild_message_index_from_order();

                self.message_model.total_height = self.message_model.height_tree.total_height();
                self.message_state.pending_new_message_count = appended_count;
                self.preload_state.last_emote_preload_range = None;
                self.interaction_state.hitbox_manager.invalidate_cache();

                let primed_async_layouts = if self.is_active_split {
                    let mut primed = 0usize;
                    for msg in messages
                        .iter()
                        .take(prepended_count)
                        .rev()
                        .take(HISTORY_PREPEND_ASYNC_PRIME_LIMIT)
                    {
                        self.request_primed_async_layout(msg);
                        primed += 1;
                    }
                    primed
                } else {
                    0
                };

                if !self.is_active_split {
                    crate::log_perf_render!(
                        trace,
                        channel_id = %self.channel_id,
                        prepended = prepended_count,
                        appended = appended_count,
                        "Skipped inactive split history-prepend async priming"
                    );
                }

                crate::log_perf_render!(
                    debug,
                    channel_id = %self.channel_id,
                    prepended = prepended_count,
                    appended = appended_count,
                    scroll_compensation = prepended_total_height,
                    primed_async_layouts,
                    "Preserved viewport across history prepend"
                );
            } else {
                retained_viewport_anchor =
                    self.capture_viewport_anchor_before_message_replacement();
                self.rebuild_message_model_from(&messages);

                crate::log_perf_render!(
                    debug,
                    channel_id = %self.channel_id,
                    new_count,
                    "Progressive downgrade for unknown insertion/reorder shape"
                );
            }
        } else if new_count == old_count && content_changed && new_count > 0 {
            let same_message_order = self.message_model.message_order.len() == new_count
                && ids_align(&self.message_model.message_order, &messages);

            if same_message_order {
                had_new_messages = false;
                self.message_model.rebuild_message_index_from_order();
                self.message_state.pending_new_message_count = 0;
                self.layout_store.runtime.mark_stale();
                self.layout_store.runtime.last_checked_emote_version = 0;
                self.layout_store.runtime.last_checked_channel_emote_version = 0;
                self.interaction_state.hitbox_manager.invalidate_cache();
            } else {
                retained_viewport_anchor =
                    self.capture_viewport_anchor_before_message_replacement();
                self.viewport_state.cached_visible_render_range = None;
                self.viewport_state.cached_viewport_paint_rows = None;
                self.interaction_state.hitbox_manager.invalidate_cache();

                let rotation = (self.message_model.message_order.len() == old_count)
                    .then(|| same_count_rotation(&self.message_model.message_order, &messages))
                    .flatten();
                let looks_like_rotation = rotation.is_some();
                let rotated_count = rotation.unwrap_or(0);
                let incremental_rotation_limit = Self::incremental_rotation_limit(new_count);
                let allow_incremental_rotation = looks_like_rotation
                    && (rotated_count <= incremental_rotation_limit
                        || rotated_count.saturating_mul(2) <= new_count);

                if was_scrolled_away && looks_like_rotation {
                    let new_start = new_count - rotated_count;
                    first_new_message_id = messages
                        .get(new_start)
                        .map(|message| SharedString::from(message.id.clone()));
                }

                if allow_incremental_rotation {
                    self.message_model
                        .remove_rotated_front_indices(rotated_count);
                    for _ in 0..rotated_count {
                        self.message_model.height_tree.remove_first();
                    }
                    let new_start = new_count - rotated_count;
                    let base_idx = self.message_model.message_order.len();
                    for (offset, msg) in messages.iter().skip(new_start).enumerate() {
                        let shared_id = SharedString::from(msg.id.clone());
                        self.message_model.message_order.push(shared_id.clone());
                        self.message_model
                            .insert_message_index(shared_id, base_idx + offset);
                        self.message_state.pending_new_message_count += 1;
                    }
                } else {
                    self.rebuild_message_model_from(&messages);

                    crate::log_perf_render!(
                        debug,
                        channel_id = %self.channel_id,
                        new_count,
                        rotated_count,
                        "Progressive downgrade for large rotation/full replacement"
                    );
                }
            }
        } else if new_count < old_count {
            retained_viewport_anchor = self.capture_viewport_anchor_before_message_replacement();
            self.rebuild_message_model_from(&messages);

            crate::log_perf_render!(
                debug,
                channel_id = %self.channel_id,
                new_count,
                old_count,
                "Progressive downgrade for shrink case"
            );
        }

        let retained_boundary_successor = previous_boundary_index.and_then(|boundary_index| {
            self.message_model
                .messages
                .iter()
                .skip(boundary_index.saturating_add(1))
                .find_map(|message| {
                    self.get_message_index(&message.id)
                        .is_some()
                        .then(|| SharedString::from(message.id.clone()))
                })
        });

        self.message_model.messages = messages;
        self.restore_viewport_anchor_after_message_replacement(retained_viewport_anchor);
        let message_model = &self.message_model;
        self.interaction_state
            .selection
            .remap_message_indices(|message_id| message_model.get_message_index(message_id));
        self.message_state.last_message_count = new_count;

        let existing_boundary_is_retained = self
            .message_state
            .new_message_boundary_id
            .as_ref()
            .is_some_and(|id| self.get_message_index(id.as_ref()).is_some());
        if !was_scrolled_away {
            self.message_state.new_message_boundary_id = None;
        } else if !existing_boundary_is_retained {
            self.message_state.new_message_boundary_id =
                retained_boundary_successor.or_else(|| {
                    first_new_message_id.filter(|id| self.get_message_index(id.as_ref()).is_some())
                });
        }

        const STATUS_PRUNE_HEADROOM: usize = 512;
        let should_prune_statuses = (content_changed && new_count <= old_count)
            || self.async_layout_manager.statuses_len()
                > self.message_model.messages.len() + STATUS_PRUNE_HEADROOM;
        if should_prune_statuses {
            let removed = self
                .async_layout_manager
                .prune_statuses_for_messages(self.message_model.messages.as_slice());
            if removed > 0 {
                crate::log_perf_render!(
                    trace,
                    channel_id = %self.channel_id,
                    removed,
                    remaining = self.message_model.messages.len(),
                    "Pruned stale async layout statuses after message set update"
                );
            }
        }

        if new_count <= old_count && !self.preload_state.pending_embed_fetches.is_empty() {
            let current_msg_ids: rustc_hash::FxHashSet<&str> = self
                .message_model
                .messages
                .iter()
                .map(|m| m.id.as_ref())
                .collect();
            self.preload_state
                .pending_embed_fetches
                .retain(|(msg_id, _)| current_msg_ids.contains(msg_id.as_ref()));
        }

        let message_delta = new_count.abs_diff(old_count) as u64;
        crate::backend::metrics::record_split_update(&self.channel_id, message_delta);

        if had_new_messages {
            self.preload_state.last_emote_preload_range = None;
        }

        if self.scroll_controller.state.auto_scroll
            && had_new_messages
            && !self.is_auto_scroll_paused()
        {
            self.message_state.pending_scroll_to_bottom = true;
        }
    }

    /// Capture the visible retained messages before a buffer replacement that
    /// rebuilds or rotates the model, plus the replaced buffer so the nearest
    /// surviving row on either side of the viewport can be found. Normal
    /// tail-follow deliberately has no anchor: its existing bottom-scroll path
    /// remains responsible for pinning to the latest message.
    fn capture_viewport_anchor_before_message_replacement(&self) -> Option<RetainedViewportAnchor> {
        let normal_bottom_follow = self.scroll_controller.state.auto_scroll
            && !self.is_auto_scroll_paused()
            && self.is_at_bottom();
        if normal_bottom_follow || self.message_model.height_tree.is_empty() {
            return None;
        }

        let order = &self.message_model.message_order;
        let (start, end) = self.get_viewport_range();
        let end = end.max(start + 1).min(order.len());
        let visible: Vec<(SharedString, f32)> = (start..end)
            .map_while(|index| {
                let message_id = order.get(index)?.clone();
                Some((message_id, self.message_model.height_tree.y_position(index)))
            })
            .collect();
        if visible.is_empty() {
            return None;
        }
        Some(RetainedViewportAnchor {
            visible,
            previous_messages: Arc::clone(&self.message_model.messages),
            viewport_start: start,
            viewport_end: end,
        })
    }

    /// Translate all coordinate-based scroll state by the first surviving
    /// visible message's layout delta. This keeps the same content at the same
    /// screen position when the bounded buffer evicts rows from its front, or a
    /// rebuild removes or inserts rows, while the user is reading history or
    /// auto-scroll is paused. When every visible row left the buffer the view
    /// moves to the nearest surviving neighbour: the first later row at the
    /// top, else the last earlier row at the bottom. Front eviction leaves no
    /// earlier rows, so the view lands on the oldest retained message.
    fn restore_viewport_anchor_after_message_replacement(
        &mut self,
        viewport_anchor: Option<RetainedViewportAnchor>,
    ) {
        let Some(anchor) = viewport_anchor else {
            return;
        };
        let surviving = anchor.visible.iter().find_map(|(message_id, previous_y)| {
            Some((self.get_message_index(message_id.as_ref())?, *previous_y))
        });
        let Some((anchor_index, previous_y)) = surviving else {
            let tree = &self.message_model.height_tree;
            let previous = anchor.previous_messages.as_slice();
            let following = previous
                .get(anchor.viewport_end..)
                .unwrap_or_default()
                .iter()
                .find_map(|message| self.get_message_index(message.id.as_ref()));
            let preceding = || {
                previous[..anchor.viewport_start.min(previous.len())]
                    .iter()
                    .rev()
                    .find_map(|message| self.get_message_index(message.id.as_ref()))
            };
            let target = match following {
                Some(index) => tree.y_position(index),
                None => preceding()
                    .map(|index| {
                        tree.y_position(index) + tree.height_at(index).unwrap_or(0.0)
                            - self.viewport_height
                    })
                    .unwrap_or(0.0),
            };
            let max_scroll = (tree.total_height() - self.viewport_height).max(0.0);
            let target = target.clamp(0.0, max_scroll);
            self.scroll_controller.offset = target;
            self.scroll_controller.state.scroll_desired = target;
            self.scroll_controller.state.prev_scroll_offset = target;
            self.scroll_controller.state.scroll_animating = false;
            self.viewport_state.cached_visible_render_range = None;
            self.interaction_state.hitbox_manager.invalidate_cache();
            self.render_perf_frame_stats.height_anchor_adjustments += 1;
            return;
        };

        let offset_delta = self.message_model.height_tree.y_position(anchor_index) - previous_y;
        if offset_delta.abs() <= 0.01 {
            return;
        }

        self.scroll_controller.offset = (self.scroll_controller.offset + offset_delta).max(0.0);
        self.scroll_controller.state.scroll_desired =
            (self.scroll_controller.state.scroll_desired + offset_delta).max(0.0);
        self.scroll_controller.state.prev_scroll_offset =
            (self.scroll_controller.state.prev_scroll_offset + offset_delta).max(0.0);
        self.render_perf_frame_stats.height_anchor_adjustments += 1;
    }

    /// O(1) message lookup by ID using pre-built index
    #[inline]
    pub(in crate::components::chat::canvas_renderer) fn get_message(
        &self,
        msg_id: &str,
    ) -> Option<&ChatMessage> {
        let idx = self.message_model.get_message_index(msg_id)?;
        self.message_model.messages.get(idx).map(Arc::as_ref)
    }

    /// O(1) message index lookup by ID
    #[inline]
    pub(in crate::components::chat::canvas_renderer) fn get_message_index(
        &self,
        msg_id: &str,
    ) -> Option<usize> {
        self.message_model.get_message_index(msg_id)
    }

    /// Scroll-map position of the stable new-message boundary, if the user is
    /// currently reading away from live. This is O(1) through message_id -> row
    /// index and intentionally does not scan or mutate the retained buffer.
    pub fn new_message_boundary_position(&self) -> Option<f32> {
        let boundary_id = self.message_state.new_message_boundary_id.as_ref()?;
        let index = self.get_message_index(boundary_id.as_ref())?;
        let total = self.message_model.message_order.len();
        (total > 0).then(|| index as f32 / total as f32)
    }
}
