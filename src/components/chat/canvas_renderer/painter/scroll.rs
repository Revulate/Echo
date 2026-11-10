//! Scroll positioning, smooth scrolling, and message navigation helpers.

use super::*;

/// Distance (px) from the content bottom within which the view counts as "at
/// bottom" for re-pinning to the latest message after a GEOMETRY change (async
/// height resolution, width-bucket change, resize). Note new-message auto-scroll
/// keys off the durable `auto_scroll` flag, not this — so this only governs the
/// geometry-change re-pin. Kept small enough that a deliberate ~one-line scroll-up
/// reads as "not at bottom", but large enough to absorb sub-line offset/rounding
/// drift so genuine bottom-following is not lost.
const AT_BOTTOM_THRESHOLD_PX: f32 = 16.0;

enum HoverPauseWakeStep {
    Stop,
    Sleep(std::time::Duration),
}

impl CanvasMessageList {
    /// Scroll to bottom (instant - used for auto-scroll on new messages)
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        self.scroll_controller.offset = max_scroll;
        self.scroll_controller.state.scroll_desired = max_scroll;
        self.scroll_controller.state.scroll_animating = false;
        self.message_state.pending_scroll_to_bottom = false;
        self.message_state.pending_scroll_ratio = None;
        self.message_state.new_message_boundary_id = None;
        self.scroll_controller.state.auto_scroll = true;
    }

    /// Clamp the retained scroll offset to the current content height.
    ///
    /// While a split is hidden it does not render, so
    /// its `scroll_controller.offset` is frozen. When it becomes visible again the
    /// height tree may have been reseeded with estimated row heights and then
    /// resolved to a smaller `total_height`, leaving the frozen offset PAST the end
    /// of the content. `HeightTree::visible_range` does not clamp the offset, so it
    /// collapses to the single trailing row positioned at a strongly-negative
    /// `viewport_y` (painted above the viewport) → the split looks blank. Because the
    /// renderer is poll/notify-driven and the empty range gets cached + replayed by
    /// the stable-reuse fast path with `needs_notify=false`, it then stays blank
    /// until a click/scroll re-clamps the offset.
    ///
    /// Running this at the top of every render frame (and again after `update_layouts`
    /// may have shrunk `total_height`) guarantees the offset can never strand the
    /// visible range past the content. Returns true if it adjusted the offset.
    /// Skips when a scroll-to-bottom / ratio restore is pending (those re-anchor
    /// intentionally) or when there is no content yet.
    pub(super) fn clamp_scroll_offset_to_content(&mut self) -> bool {
        if self.message_model.height_tree.is_empty()
            || self.message_state.pending_scroll_to_bottom
            || self.message_state.pending_scroll_ratio.is_some()
        {
            return false;
        }

        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        if self.scroll_controller.offset > max_scroll + 0.5 {
            self.scroll_controller.offset = max_scroll;
            self.scroll_controller.state.scroll_desired =
                self.scroll_controller.state.scroll_desired.min(max_scroll);
            self.viewport_state.cached_visible_render_range = None;
            return true;
        }
        false
    }

    /// "On show / on activation" hook: when a split becomes the active/visible
    /// view, re-validate its scroll geometry against current content and force
    /// the next frame off the stable-reuse fast path, so activation can never
    /// paint a stale or stranded (blank) frame. Geometry (height tree + layout
    /// cache) is retained; only the cheap per-frame visible-row reuse is
    /// invalidated. Complements the per-frame clamp in `render` and the `switch_tab`
    /// window refresh: this is the within-tab, split-focus-change path.
    pub fn revalidate_on_show(&mut self) {
        self.clamp_scroll_offset_to_content();
        self.viewport_state.cached_visible_render_range = None;
    }

    /// Scroll to top of chat (Ctrl+Home / Home) - with smooth animation
    pub fn scroll_to_top(&mut self) {
        self.scroll_controller.state.prev_scroll_offset = self.scroll_controller.offset;
        self.start_scroll_animation(0.0);
        self.apply_manual_scroll_target_state(0.0);
        self.record_user_scroll();
    }

    /// Jump to a relative pixel position; fraction: 0.0 = top, 1.0 = bottom.
    pub fn scroll_to_fraction(&mut self, fraction: f32, animate: bool) {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        self.scroll_to_offset(fraction * max_scroll, animate);
    }

    /// Put the viewport top at `position` in message-index space (row index
    /// plus the scrolled fraction of that row, over the row count), the scroll
    /// map's index-valued coordinate space.
    pub fn scroll_to_index_position(&mut self, position: f32, animate: bool) {
        let tree = &self.message_model.height_tree;
        let count = self.message_model.message_order.len();
        let target = if count == 0 || tree.is_empty() {
            0.0
        } else {
            let index_position = position.clamp(0.0, 1.0) * count as f32;
            let idx = (index_position.floor() as usize).min(tree.len() - 1);
            let within = (index_position - idx as f32).clamp(0.0, 1.0);
            tree.y_position(idx) + within * tree.height_at(idx).unwrap_or(0.0)
        };
        self.scroll_to_offset(target, animate);
    }

    /// Viewport top and bottom edges in message-index space, as fractions of
    /// the row count.
    pub fn index_space_viewport(&self) -> (f32, f32) {
        let count = self.message_model.message_order.len();
        if count == 0 {
            return (0.0, 1.0);
        }
        let top = self.index_position_at_y(self.scroll_controller.offset);
        let bottom = self.index_position_at_y(self.scroll_controller.offset + self.viewport_height);
        (top / count as f32, bottom / count as f32)
    }

    fn index_position_at_y(&self, y: f32) -> f32 {
        let tree = &self.message_model.height_tree;
        if y >= tree.total_height() {
            return tree.len() as f32;
        }
        let idx = tree.index_at_y(y);
        let row_height = tree.height_at(idx).unwrap_or(0.0);
        let within = if row_height > 0.0 {
            ((y - tree.y_position(idx)) / row_height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        idx as f32 + within
    }

    pub(super) fn scroll_to_offset(&mut self, target: f32, animate: bool) {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let target = target.clamp(0.0, max_scroll);
        self.scroll_controller.state.prev_scroll_offset = self.scroll_controller.offset;
        if animate {
            self.start_scroll_animation(target);
        } else {
            self.scroll_controller.offset = target;
            self.scroll_controller.state.scroll_desired = target;
            self.scroll_controller.state.scroll_animating = false;
            self.viewport_state.cached_visible_render_range = None;
        }
        self.apply_manual_scroll_target_state(target);
        self.record_user_scroll();
    }

    /// Scroll by page (PageUp/PageDown) - with smooth animation
    /// positive = scroll down (PageDown), negative = scroll up (PageUp)
    pub fn scroll_by_page(&mut self, direction: i32) {
        let page_amount = self.viewport_height * 0.9;
        let scroll_delta = page_amount * direction as f32;

        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        self.scroll_controller.state.prev_scroll_offset = self.scroll_controller.offset;

        let base_position = if self.scroll_controller.state.scroll_animating {
            self.scroll_controller.state.scroll_desired
        } else {
            self.scroll_controller.offset
        };
        let target = (base_position + scroll_delta).clamp(0.0, max_scroll);
        self.start_scroll_animation(target);
        self.apply_manual_scroll_target_state(target);

        self.record_user_scroll();
    }

    /// Apply a wheel-style delta through the mouse-wheel path, scaled like one
    /// wheel line of `delta_y` pixels.
    pub fn scroll_by_pixels(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        self.on_scroll(delta_y * interaction::WHEEL_LINE_SCROLL_MULTIPLIER, cx);
    }

    /// Check if scrolled to bottom (within threshold)
    pub fn is_at_bottom(&self) -> bool {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let effective_offset = if self.scroll_controller.state.scroll_animating {
            self.scroll_controller.state.scroll_desired
        } else {
            self.scroll_controller.offset
        };
        (effective_offset - max_scroll).abs() < AT_BOTTOM_THRESHOLD_PX
    }

    /// SCROLL PAUSE: Check if user is actively scrolling (interacted recently)
    /// Returns true if user scrolled within `scroll_pause_duration` (default 150ms)
    /// Used to prevent auto-scroll from interrupting user scroll actions
    #[inline]
    pub fn is_user_scrolling(&self) -> bool {
        self.scroll_controller.state.last_user_scroll_time.elapsed()
            < self.scroll_controller.state.scroll_pause_duration
    }

    #[inline]
    fn is_modifier_pause_active(&self) -> bool {
        self.scroll_controller.state.modifier_pause_active
    }

    #[inline]
    fn is_stored_hover_pause_active(&self) -> bool {
        self.scroll_controller.state.hover_pause_indefinite
            || self.scroll_controller.state.hover_pause_until.is_some()
    }

    #[inline]
    fn is_hover_pause_active(&self) -> bool {
        if !self.scroll_controller.state.mouse_hovering_viewport {
            return false;
        }
        match settings::get_chat_hover_pause_mode() {
            settings::ChatHoverPauseMode::Disabled => false,
            settings::ChatHoverPauseMode::Indefinite => {
                self.scroll_controller.state.hover_pause_indefinite
            }
            _ => self
                .scroll_controller
                .state
                .hover_pause_until
                .is_some_and(|deadline| std::time::Instant::now() < deadline),
        }
    }

    /// Whether an explicit keyboard or configured hover policy is pausing
    /// bottom-follow. The short manual-scroll debounce is deliberately separate
    /// so it does not flash the visible pause badge.
    #[inline]
    pub fn is_intentional_chat_pause_active(&self) -> bool {
        self.is_modifier_pause_active() || self.is_hover_pause_active()
    }

    /// Pause guard for bottom-locked auto-scroll. A selection drag holds the
    /// view still without surfacing the pause badge; release re-pins through
    /// `resume_stranded_bottom_follow`.
    #[inline]
    pub fn is_auto_scroll_paused(&self) -> bool {
        self.is_user_scrolling()
            || self.is_intentional_chat_pause_active()
            || self.interaction_state.selection.is_dragging
    }

    /// Whether the view is pinned to the newest message and not paused.
    #[inline]
    pub fn is_following_live(&self) -> bool {
        self.scroll_controller.state.auto_scroll && !self.is_auto_scroll_paused()
    }

    /// Whether the visible Paused badge should replace the jump-to-live pill.
    ///
    /// Only an explicit hold/hover pause of a bottom-following view shows it;
    /// scrollback reading is represented by the jump pill, and the manual-scroll
    /// debounce and selection drags never surface it.
    #[inline]
    pub(super) fn should_show_pause_indicator(&self) -> bool {
        self.scroll_controller.state.auto_scroll && self.is_intentional_chat_pause_active()
    }

    /// Re-pin a view that still follows live chat but was left short of the
    /// bottom, e.g. a batch that landed inside the manual-scroll debounce after
    /// a wheel back down.
    /// Returns true when it moved the scroll target.
    pub(super) fn resume_stranded_bottom_follow(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.scroll_controller.state.auto_scroll
            || self.message_state.pending_scroll_to_bottom
            || self.message_model.height_tree.is_empty()
            || self.is_intentional_chat_pause_active()
            || self.interaction_state.selection.is_dragging
            || self.is_at_bottom()
        {
            return false;
        }

        if self.is_user_scrolling() {
            self.schedule_follow_resume_wake(cx);
            return false;
        }

        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        self.start_scroll_animation(max_scroll);
        self.message_state.new_message_boundary_id = None;
        true
    }

    fn schedule_follow_resume_wake(&mut self, cx: &mut Context<Self>) {
        if self.scroll_controller.state.follow_resume_wake_pending {
            return;
        }
        self.scroll_controller.state.follow_resume_wake_pending = true;
        let delay = self
            .scroll_controller
            .state
            .scroll_pause_duration
            .saturating_sub(self.scroll_controller.state.last_user_scroll_time.elapsed())
            + std::time::Duration::from_millis(1);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.scroll_controller.state.follow_resume_wake_pending = false;
                crate::backend::metrics::record_ui_notify_source("canvas.follow_resume");
                cx.notify();
            });
        })
        .detach();
    }

    fn reconcile_intentional_pause_transition(&mut self, was_active: bool) -> bool {
        let is_active = self.is_intentional_chat_pause_active();
        if was_active == is_active {
            return false;
        }

        if is_active {
            self.message_state.pending_scroll_to_bottom = false;
        } else if self.scroll_controller.state.auto_scroll && !self.is_user_scrolling() {
            self.message_state.pending_scroll_to_bottom = true;
        }
        true
    }

    /// Synchronize raw window modifier state. The configured modifier is
    /// matched exactly, so Alt does not activate during Ctrl+Alt split actions.
    pub fn set_chat_pause_modifiers(&mut self, held: Modifiers) -> bool {
        let Modifiers {
            control,
            alt,
            shift,
            platform,
            ..
        } = held;
        let pause_active =
            settings::get_chat_pause_modifier().matches(control, alt, shift, platform);
        let state = &self.scroll_controller.state;
        if state.modifier_control == control
            && state.modifier_alt == alt
            && state.modifier_shift == shift
            && state.modifier_platform == platform
            && state.modifier_pause_active == pause_active
        {
            return false;
        }

        let was_active = self.is_intentional_chat_pause_active();
        let state = &mut self.scroll_controller.state;
        state.modifier_control = control;
        state.modifier_alt = alt;
        state.modifier_shift = shift;
        state.modifier_platform = platform;
        state.modifier_pause_active = pause_active;
        self.reconcile_intentional_pause_transition(was_active)
    }

    /// Start or renew the configured hover pause. Finite modes use one
    /// generation-fenced waker per active hover session; pointer movement only
    /// extends its deadline and does not spawn an unbounded task stream.
    pub(super) fn refresh_chat_hover_pause(&mut self, cx: &mut Context<Self>) {
        if !self.scroll_controller.state.mouse_hovering_viewport {
            return;
        }

        let was_active = self.is_intentional_chat_pause_active();
        let had_stored_hover_pause = self.is_stored_hover_pause_active();
        let mode = settings::get_chat_hover_pause_mode();
        let mut start_waker = None;

        match mode {
            settings::ChatHoverPauseMode::Disabled => {
                self.scroll_controller.state.reset_hover_pause(false);
            }
            settings::ChatHoverPauseMode::Indefinite => {
                self.scroll_controller.state.reset_hover_pause(true);
            }
            finite => {
                let duration = std::time::Duration::from_secs(
                    finite
                        .duration_secs()
                        .expect("finite chat hover pause duration"),
                );
                let state = &mut self.scroll_controller.state;
                state.hover_pause_indefinite = false;
                state.hover_pause_until = Some(std::time::Instant::now() + duration);
                if !state.hover_pause_waker_running {
                    state.hover_pause_generation = state.hover_pause_generation.wrapping_add(1);
                    state.hover_pause_waker_running = true;
                    start_waker = Some((state.hover_pause_generation, duration));
                }
            }
        }

        let changed = self.reconcile_intentional_pause_transition(was_active);
        if !changed
            && had_stored_hover_pause
            && mode == settings::ChatHoverPauseMode::Disabled
            && !self.is_modifier_pause_active()
            && self.scroll_controller.state.auto_scroll
            && !self.is_user_scrolling()
        {
            self.message_state.pending_scroll_to_bottom = true;
        }
        if changed {
            crate::backend::metrics::record_ui_notify_source("canvas.chat_pause.hover_transition");
            cx.notify();
        }

        let Some((generation, initial_delay)) = start_waker else {
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            let mut delay = initial_delay;
            loop {
                cx.background_executor().timer(delay).await;
                let step = this
                    .update(cx, |this, cx| {
                        let state = &this.scroll_controller.state;
                        if state.hover_pause_generation != generation
                            || !state.hover_pause_waker_running
                            || !state.mouse_hovering_viewport
                        {
                            return HoverPauseWakeStep::Stop;
                        }
                        let Some(deadline) = state.hover_pause_until else {
                            return HoverPauseWakeStep::Stop;
                        };
                        let now = std::time::Instant::now();
                        if now < deadline {
                            return HoverPauseWakeStep::Sleep(deadline - now);
                        }

                        let auto_scroll = {
                            let state = &mut this.scroll_controller.state;
                            state.hover_pause_until = None;
                            state.hover_pause_waker_running = false;
                            state.auto_scroll
                        };
                        if !this.is_modifier_pause_active()
                            && auto_scroll
                            && !this.is_user_scrolling()
                        {
                            this.message_state.pending_scroll_to_bottom = true;
                        }
                        crate::backend::metrics::record_ui_notify_source(
                            "canvas.chat_pause.hover_expired",
                        );
                        cx.notify();
                        HoverPauseWakeStep::Stop
                    })
                    .unwrap_or(HoverPauseWakeStep::Stop);

                match step {
                    HoverPauseWakeStep::Stop => break,
                    HoverPauseWakeStep::Sleep(next_delay) => delay = next_delay,
                }
            }
        });
        self.scroll_controller.state.hover_pause_task = Some(task);
    }

    /// Clear hover-owned pause state on pointer leave. Modifier pause remains
    /// independent, so leaving chat while Alt is held does not resume it.
    pub(super) fn clear_chat_hover_pause(&mut self) -> bool {
        let was_active = self.is_modifier_pause_active() || self.is_stored_hover_pause_active();
        self.scroll_controller.state.reset_hover_pause(false);
        self.reconcile_intentional_pause_transition(was_active)
    }

    /// A selection drag whose mouse-up went to another window (Alt+Tab) or a
    /// hidden tab must not keep holding bottom-follow.
    pub(super) fn end_lost_selection_drag(&mut self) {
        self.interaction_state.pending_text_click = None;
        self.interaction_state.selection.end_drag();
    }

    /// Retained hidden tabs must not keep pointer geometry or a lost modifier
    /// release as an active pause reason.
    pub(super) fn clear_chat_pause_for_hidden_tab(&mut self) {
        self.end_lost_selection_drag();
        let was_active = self.is_modifier_pause_active() || self.is_stored_hover_pause_active();
        let state = &mut self.scroll_controller.state;
        state.mouse_hovering_viewport = false;
        state.reset_hover_pause(false);
        state.modifier_control = false;
        state.modifier_alt = false;
        state.modifier_shift = false;
        state.modifier_platform = false;
        state.modifier_pause_active = false;
        let _ = self.reconcile_intentional_pause_transition(was_active);
    }

    /// SCROLL PAUSE: Record that user initiated a scroll action
    /// Called from on_scroll(), scroll_by_page(), scroll_to_top(), etc.
    #[inline]
    pub(super) fn record_user_scroll(&mut self) {
        self.scroll_controller.state.last_user_scroll_time = std::time::Instant::now();
    }

    #[inline]
    pub(super) fn apply_manual_scroll_target_state(&mut self, target: f32) {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let at_bottom = max_scroll < 1.0 || (target - max_scroll).abs() < 10.0;

        self.message_state.pending_scroll_to_bottom = false;
        self.message_state.pending_scroll_ratio = None;
        self.scroll_controller.state.auto_scroll = at_bottom;
        if at_bottom {
            self.message_state.new_message_boundary_id = None;
        }
    }

    /// Update scroll animation using frame-rate-independent exponential decay.
    /// Each frame moves a fixed PROPORTION of the remaining distance,
    /// giving natural deceleration that feels identical at 60Hz and 144Hz.
    /// Returns true if animation is still active and needs more frames.
    #[inline]
    pub(super) fn update_scroll_animation(&mut self) -> bool {
        if !self.scroll_controller.state.scroll_animating {
            return false;
        }

        if settings::get_reduce_decorative_motion() {
            self.scroll_controller.offset = self.scroll_controller.state.scroll_desired;
            self.scroll_controller.state.scroll_animating = false;
            return false;
        }

        let duration_ms = self.scroll_controller.state.anim_duration_ms;

        if !self.scroll_controller.state.anim_smooth_enabled || duration_ms == 0 {
            self.scroll_controller.offset = self.scroll_controller.state.scroll_desired;
            self.scroll_controller.state.scroll_animating = false;
            return false;
        }

        let now = std::time::Instant::now();
        let dt = now
            .duration_since(self.scroll_controller.state.scroll_animation_start)
            .as_secs_f32();
        self.scroll_controller.state.scroll_animation_start = now;

        let duration_s = (duration_ms as f32) / 1000.0;
        let smoothness = 3.0 / duration_s;

        let factor = 1.0 - (-smoothness * dt).exp();
        let diff = self.scroll_controller.state.scroll_desired - self.scroll_controller.offset;
        self.scroll_controller.offset += diff * factor;

        if diff.abs() < 0.5 {
            self.scroll_controller.offset = self.scroll_controller.state.scroll_desired;
            self.scroll_controller.state.scroll_animating = false;
            return false;
        }

        true
    }

    /// Start a smooth scroll animation to a target position.
    /// With exponential decay, retargeting is automatic - just update the desired position.
    #[inline]
    pub(super) fn start_scroll_animation(&mut self, target: f32) {
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let clamped_target = target.clamp(0.0, max_scroll);

        let smooth_enabled =
            settings::get_smooth_scroll_enabled() && !settings::get_reduce_decorative_motion();
        let duration_ms = settings::get_scroll_animation_duration_ms();

        if !smooth_enabled || duration_ms == 0 {
            self.scroll_controller.offset = clamped_target;
            self.scroll_controller.state.scroll_desired = clamped_target;
            self.scroll_controller.state.scroll_animating = false;
            return;
        }

        self.scroll_controller.state.scroll_desired = clamped_target;
        if !self.scroll_controller.state.scroll_animating {
            self.scroll_controller.state.scroll_animation_start = std::time::Instant::now();
            self.scroll_controller.state.anim_smooth_enabled = smooth_enabled;
            self.scroll_controller.state.anim_duration_ms = duration_ms;
        }
        self.scroll_controller.state.scroll_animating = true;
    }

    /// Scroll to a specific message by ID with flash highlight animation
    pub fn scroll_to_message(&mut self, message_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(idx) = self.get_message_index(message_id) else {
            return false;
        };
        let target_y = self.message_model.height_tree.y_position(idx);

        let centered_scroll = (target_y - self.viewport_height / 3.0).max(0.0);
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let target = centered_scroll.min(max_scroll);

        self.scroll_controller.state.prev_scroll_offset = self.scroll_controller.offset;
        self.start_scroll_animation(target);

        self.apply_manual_scroll_target_state(target);
        self.record_user_scroll();
        self.search_focus_message_id = Some(SharedString::from(message_id));
        self.schedule_search_focus_clear(cx);

        crate::backend::metrics::record_ui_notify_source("canvas.scroll_to_message");
        cx.notify();
        true
    }
}

impl CanvasMessageList {
    /// Detect scroll direction based on offset change
    pub(super) fn detect_scroll_direction(&self) -> ScrollDirection {
        let delta = self.scroll_controller.offset - self.scroll_controller.state.prev_scroll_offset;
        if delta.abs() < 1.0 {
            ScrollDirection::Static
        } else if delta > 0.0 {
            ScrollDirection::Down
        } else {
            ScrollDirection::Up
        }
    }

    /// Calculate scroll velocity for adaptive preload margins
    /// Returns normalized value: 0.0 = static, 1.0 = very fast scrolling
    pub(super) fn calculate_scroll_velocity(&self) -> f32 {
        let scroll_delta =
            (self.scroll_controller.offset - self.scroll_controller.state.prev_scroll_offset).abs();
        (scroll_delta / 100.0).min(1.0)
    }

    /// Calculate adaptive preload margins based on scroll direction and velocity
    /// Returns (messages_behind, messages_ahead) for preloading
    pub(super) fn get_adaptive_preload_margins(
        &self,
        direction: ScrollDirection,
    ) -> (usize, usize) {
        let velocity = self.calculate_scroll_velocity();
        let scale = 1.0 + velocity * 1.5;

        let (base_behind, base_ahead) = match direction {
            ScrollDirection::Down => (20, 40),
            ScrollDirection::Up => (50, 20),
            ScrollDirection::Static => (30, 30),
        };

        (
            (base_behind as f32 * scale) as usize,
            (base_ahead as f32 * scale) as usize,
        )
    }

    /// Update the EMA-debounced scroll-acceleration signal (2nd derivative of offset).
    ///
    /// Sampled once per scroll tick from the current normalized velocity versus the
    /// velocity captured on the previous tick. Only *positive* acceleration (a fling
    /// ramping up) feeds the signal; deceleration or direction reversal lets it decay.
    /// The EMA both smooths single-frame jitter (so a momentary spike does not balloon
    /// the lookahead window) and clamps the result into `0.0..=1.0`.
    ///
    /// Pure CPU bookkeeping over offsets we already track — no platform/GPU dependency.
    pub(super) fn update_acceleration_signal(&mut self) {
        const ACCEL_EMA_ALPHA: f32 = 0.35;
        const ACCEL_JITTER_FLOOR: f32 = 0.02;

        let velocity = self.calculate_scroll_velocity();
        let prev_velocity = self.scroll_controller.state.prev_velocity;
        self.scroll_controller.state.prev_velocity = velocity;

        let raw_accel = (velocity - prev_velocity).max(0.0);
        let target = if raw_accel < ACCEL_JITTER_FLOOR {
            0.0
        } else {
            raw_accel
        };

        let ema = self.scroll_controller.state.accel_ema;
        self.scroll_controller.state.accel_ema =
            (ema + ACCEL_EMA_ALPHA * (target - ema)).clamp(0.0, 1.0);
    }

    /// Predictive extension applied to the dominant-direction lookahead margin,
    /// driven by the debounced acceleration signal. Returns extra rows to dispatch
    /// ahead of the fling, hard-clamped so a noisy signal cannot over-expand the
    /// window (and waste worker throughput) on jitter.
    pub(super) fn predictive_lookahead_boost(&self) -> usize {
        const MAX_PREDICTIVE_AHEAD: f32 = 30.0;
        (self.scroll_controller.state.accel_ema * MAX_PREDICTIVE_AHEAD) as usize
    }
}
