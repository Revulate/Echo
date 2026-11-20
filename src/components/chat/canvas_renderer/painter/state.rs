//! Focused state groups for `CanvasMessageList`.
//!
//! These buckets keep lifecycle, scroll, preload, layout, viewport-frame, and wake-loop
//! state separate from the core message/layout storage.

use super::*;

fn instant_minus_secs(secs: u64) -> std::time::Instant {
    std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(secs))
        .unwrap_or_else(std::time::Instant::now)
}

pub(super) struct MessageLifecycleState {
    pub(super) last_message_count: usize,
    pub(super) pending_scroll_to_bottom: bool,
    pub(super) pending_scroll_ratio: Option<f32>,
    pub(super) needs_layout_rebuild: bool,
    pub(super) pending_new_message_count: usize,
    /// Stable ID of the first live message received while this split was away
    /// from the bottom. Keeping the ID (rather than a row index) lets history
    /// prepends and bounded-buffer rotations preserve the visual boundary.
    pub(super) new_message_boundary_id: Option<SharedString>,
    /// Newest message when this split's tab was hidden or its window lost
    /// focus, drawn as the "last read" line when that setting is on.
    pub(super) last_read_id: Option<SharedString>,
}

impl MessageLifecycleState {
    pub(super) fn initial() -> Self {
        Self {
            last_message_count: 0,
            pending_scroll_to_bottom: false,
            pending_scroll_ratio: None,
            needs_layout_rebuild: true,
            pending_new_message_count: 0,
            new_message_boundary_id: None,
            last_read_id: None,
        }
    }
}

/// A budgeted round-robin scan over a row-index range (emote preload, embed
/// discovery): the range it last covered, its cursor (an offset into that
/// range), when it last ran, and the first row added since, so a pass can
/// scan just the new rows instead of the whole range.
pub(super) struct RangeScan {
    range: Option<(usize, usize)>,
    new_rows_from: Option<usize>,
    cursor: usize,
    last_scan: std::time::Instant,
}

impl RangeScan {
    pub(super) fn initial() -> Self {
        Self {
            range: None,
            new_rows_from: None,
            cursor: 0,
            last_scan: instant_minus_secs(10),
        }
    }

    #[cfg(test)]
    pub(super) fn range(&self) -> Option<(usize, usize)> {
        self.range
    }

    /// Forget the range, so the next pass scans it afresh.
    pub(super) fn reset(&mut self) {
        self.range = None;
    }

    /// Rows from `from` on were just added.
    pub(super) fn note_new_rows(&mut self, from: usize) {
        self.new_rows_from = Some(self.new_rows_from.map_or(from, |current| current.min(from)));
    }

    pub(super) fn has_new_rows(&self) -> bool {
        self.new_rows_from.is_some()
    }

    /// Keep naming the same rows after the first `dropped` rows left.
    pub(super) fn shift_after_front_drop(&mut self, dropped: usize) {
        self.range = self
            .range
            .and_then(|range| super::message_delta::range_after_front_drop(range, dropped));
        self.new_rows_from = self.new_rows_from.map(|from| from.saturating_sub(dropped));
    }

    /// The rows to scan now for the range `current`. Rows added since the
    /// last pass are scanned at once when they are all that entered the
    /// range; the round robin of `budget` rows runs when the range changed
    /// otherwise (restarting `restart_at` rows into it) or `interval` passed.
    /// Rows in both are listed once.
    pub(super) fn rows_to_scan(
        &mut self,
        current: (usize, usize),
        restart_at: usize,
        interval: std::time::Duration,
        budget: usize,
        now: std::time::Instant,
    ) -> Vec<usize> {
        let previous = self.range;
        let new_rows_from = self.new_rows_from.take();
        let only_new_rows_entered = previous.is_some_and(|previous| {
            super::preload::only_new_rows_entered(previous, current, new_rows_from)
        });
        let new_rows = new_rows_from
            .filter(|_| only_new_rows_entered)
            .map_or(0..0, |from| from.max(current.0)..current.1);
        let range_changed = previous != Some(current) && !only_new_rows_entered;
        self.range = Some(current);
        let mut scan: Vec<usize> = new_rows.clone().take(budget).collect();
        if !range_changed && now.saturating_duration_since(self.last_scan) < interval {
            return scan;
        }
        if range_changed {
            self.cursor = restart_at;
        }
        self.last_scan = now;
        let range_len = current.1.saturating_sub(current.0);
        if range_len > 0 {
            let scan_count = range_len.min(budget);
            let start_offset = self.cursor % range_len;
            self.cursor = (start_offset + scan_count) % range_len;
            scan.extend(
                (0..scan_count)
                    .map(|step| current.0 + ((start_offset + step) % range_len))
                    .filter(|idx| !new_rows.contains(idx)),
            );
        }
        scan
    }
}

pub(super) struct PreloadState {
    pub(super) pending_embed_fetches: rustc_hash::FxHashSet<(Arc<str>, u32)>,
    pub(super) emote_scan: RangeScan,
    pub(super) last_layout_image_preload_range: Option<(usize, usize)>,
    pub(super) last_layout_image_preload_time: std::time::Instant,
    pub(super) last_embed_fetch_range: Option<(usize, usize)>,
    pub(super) embed_fetch_scan_cursor: usize,
    pub(super) last_embed_fetch_time: std::time::Instant,
    pub(super) last_burst_direction: Option<ScrollDirection>,
    /// Last velocity-scaled lookahead window dispatched for async layout.
    /// Guards `preload_layouts_for_visible_range` so the offscreen lookahead is
    /// only re-dispatched when the window actually moves, not every frame.
    pub(super) last_layout_lookahead_range: Option<(usize, usize)>,
    /// Last landing-zone range dispatched while smooth scrolling toward a distant target.
    pub(super) last_layout_landing_range: Option<(usize, usize)>,
}

impl PreloadState {
    pub(super) fn initial() -> Self {
        Self {
            pending_embed_fetches: rustc_hash::FxHashSet::default(),
            emote_scan: RangeScan::initial(),
            last_layout_image_preload_range: None,
            last_layout_image_preload_time: instant_minus_secs(10),
            last_embed_fetch_range: None,
            embed_fetch_scan_cursor: 0,
            last_embed_fetch_time: instant_minus_secs(10),
            last_burst_direction: None,
            last_layout_lookahead_range: None,
            last_layout_landing_range: None,
        }
    }

    /// Rows from `from` on were just added; the next emote preload pass
    /// scans them first.
    pub(super) fn note_new_rows(&mut self, from: usize) {
        self.emote_scan.note_new_rows(from);
    }

    /// Keep the emote preload range naming the same rows after the first
    /// `dropped` rows left.
    pub(super) fn shift_after_front_drop(&mut self, dropped: usize) {
        self.emote_scan.shift_after_front_drop(dropped);
    }
}

pub(in crate::components::chat::canvas_renderer) struct ScrollState {
    pub(super) auto_scroll: bool,
    pub(super) prev_scroll_offset: f32,
    pub(super) last_user_scroll_time: std::time::Instant,
    pub(super) scroll_pause_duration: std::time::Duration,
    pub(super) follow_resume_wake_pending: bool,
    pub(super) mouse_hovering_viewport: bool,
    pub(super) hover_pause_until: Option<std::time::Instant>,
    pub(super) hover_pause_indefinite: bool,
    pub(super) hover_pause_generation: u64,
    pub(super) hover_pause_waker_running: bool,
    pub(super) hover_pause_task: Option<Task<()>>,
    pub(super) modifier_control: bool,
    pub(super) modifier_alt: bool,
    pub(super) modifier_shift: bool,
    pub(super) modifier_platform: bool,
    pub(super) modifier_pause_active: bool,
    pub(super) scroll_desired: f32,
    pub(super) scroll_animation_start: std::time::Instant,
    pub(super) scroll_animating: bool,
    /// Smooth-scroll settings snapshotted once when an animation starts, so each
    /// animated frame reads these instead of re-locking the global SETTINGS mutex.
    /// A scroll animation is short-lived; a mid-animation settings change applies
    /// on the next animation, which is the desired behavior.
    pub(super) anim_smooth_enabled: bool,
    pub(super) anim_duration_ms: u32,
    /// Normalized scroll velocity (0.0..=1.0) sampled on the previous scroll tick.
    /// Used to derive the 2nd derivative of offset (acceleration) for predictive
    /// layout lookahead during fast flings.
    pub(super) prev_velocity: f32,
    /// EMA-debounced acceleration signal (0.0..=1.0). Only positive (ramping-up)
    /// acceleration accumulates; it decays toward 0 when the fling steadies or
    /// reverses. Smoothing rejects single-frame jitter so the lookahead window
    /// is not over-expanded by noise. See `update_acceleration_signal`.
    pub(super) accel_ema: f32,
}

impl ScrollState {
    pub(super) fn initial() -> Self {
        Self {
            auto_scroll: true,
            prev_scroll_offset: 0.0,
            last_user_scroll_time: instant_minus_secs(10),
            scroll_pause_duration: std::time::Duration::from_millis(150),
            follow_resume_wake_pending: false,
            mouse_hovering_viewport: false,
            hover_pause_until: None,
            hover_pause_indefinite: false,
            hover_pause_generation: 0,
            hover_pause_waker_running: false,
            hover_pause_task: None,
            modifier_control: false,
            modifier_alt: false,
            modifier_shift: false,
            modifier_platform: false,
            modifier_pause_active: false,
            scroll_desired: 0.0,
            scroll_animation_start: std::time::Instant::now(),
            scroll_animating: false,
            anim_smooth_enabled: true,
            anim_duration_ms: 0,
            prev_velocity: 0.0,
            accel_ema: 0.0,
        }
    }

    /// Clear hover-pause timing, cancel its waker, and fence stale wakers.
    #[inline]
    pub(super) fn reset_hover_pause(&mut self, indefinite: bool) {
        self.hover_pause_until = None;
        self.hover_pause_indefinite = indefinite;
        self.hover_pause_waker_running = false;
        self.hover_pause_task = None;
        self.hover_pause_generation = self.hover_pause_generation.wrapping_add(1);
    }
}

pub(in crate::components::chat::canvas_renderer) struct ScrollController {
    pub(in crate::components::chat::canvas_renderer) offset: f32,
    pub(in crate::components::chat::canvas_renderer) state: ScrollState,
}

impl ScrollController {
    pub(super) fn initial() -> Self {
        Self {
            offset: 0.0,
            state: ScrollState::initial(),
        }
    }
}

pub(in crate::components::chat::canvas_renderer) struct InteractionState {
    pub(in crate::components::chat::canvas_renderer) selection: SelectionState,
    pub(in crate::components::chat::canvas_renderer) tooltip: TooltipState,
    pub(in crate::components::chat::canvas_renderer) tooltip_overlay_source_id: String,
    pub(in crate::components::chat::canvas_renderer) hitbox_manager: HitboxManager,
    pub(in crate::components::chat::canvas_renderer) element_bounds: Option<Bounds<Pixels>>,
    pub(in crate::components::chat::canvas_renderer) cursor_style: CursorStyle,
    pub(in crate::components::chat::canvas_renderer) pending_text_click: Option<PendingTextClick>,
    pub(super) pending_middle_click: Option<PendingTextClick>,
    pub(in crate::components::chat::canvas_renderer) last_mouse_move_notify: std::time::Instant,
    /// A throttled mouse move already scheduled its trailing notify.
    pub(super) mouse_move_trailing_notify_pending: bool,
    pub(super) selection_granularity: SelectionGranularity,
    /// Message and char range of the word or message a double- or
    /// triple-click selected; a drag that follows extends by whole units.
    pub(super) granular_anchor: Option<(SharedString, std::ops::Range<usize>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionGranularity {
    Char,
    Word,
    Message,
}

impl SelectionGranularity {
    pub(super) fn from_click_count(click_count: usize) -> Self {
        match click_count {
            0 | 1 => Self::Char,
            2 => Self::Word,
            _ => Self::Message,
        }
    }
}

pub(in crate::components::chat::canvas_renderer) struct PendingTextClick {
    pub(super) message_id: String,
    pub(super) kind: HitboxKind,
    pub(super) x: f32,
    pub(super) y: f32,
}

impl InteractionState {
    pub(super) fn initial(tooltip_overlay_source_id: String) -> Self {
        Self {
            selection: SelectionState::new(),
            tooltip: TooltipState::new(),
            tooltip_overlay_source_id,
            hitbox_manager: HitboxManager::new(),
            element_bounds: None,
            cursor_style: CursorStyle::IBeam,
            pending_text_click: None,
            pending_middle_click: None,
            last_mouse_move_notify: instant_minus_secs(10),
            mouse_move_trailing_notify_pending: false,
            selection_granularity: SelectionGranularity::Char,
            granular_anchor: None,
        }
    }
}

pub(super) struct LayoutRuntimeState {
    /// Secondary index: message_id -> the (quantized_width, show_timestamp) variants cached
    /// for it. The `show_timestamp` discriminant lets the fallback-height path find the cached
    /// layout for the *current* timestamp variant even when both variants coexist (history load
    /// or right after a timestamp toggle), instead of missing and returning the 42px estimate.
    pub(super) cached_width_index: FxHashMap<SharedString, SmallVec<[(u32, bool); 4]>>,
    pub(super) last_checked_emote_version: u64,
    pub(super) last_checked_channel_emote_version: u64,
    pub(super) has_stale_layouts: bool,
    pub(super) last_invalidation_version: u64,
    pub(super) last_layout_eviction_seq: u64,
    pub(super) last_layout_settings_version_seen: u64,
    /// `f32` bits of the window scale factor the stored layouts were built for.
    pub(super) last_layout_scale_bits: u32,
    /// Moderation-button mode the stored layouts were built for.
    pub(super) last_layout_mod_buttons: bool,
    pub(super) resize_in_progress: bool,
    pub(super) active_layout_quantized_width: u32,
    pub(super) pending_layout_quantized_width: Option<u32>,
    /// When the pending bucket last changed; the stabilize delay waits on this.
    pub(super) pending_layout_quantized_since: Option<std::time::Instant>,
    /// When the current run of pending buckets began. Unlike
    /// `pending_layout_quantized_since` it survives bucket changes, so a
    /// continuous resize still reflows at the force-apply cadence instead of
    /// holding every change until the pointer stops.
    pub(super) pending_layout_quantized_run_since: Option<std::time::Instant>,
    pub(super) stale_layout_refresh_cursor: usize,
    pub(super) stale_layout_refresh_pass_found_stale: bool,
    pub(super) refresh_missing_layouts_for_width: bool,
    pub(super) relayout_cause: Option<RelayoutCause>,
}

impl LayoutRuntimeState {
    pub(super) fn initial(container_width: f32, layout_scale_factor: f32) -> Self {
        Self {
            cached_width_index: FxHashMap::default(),
            last_checked_emote_version: 0,
            last_checked_channel_emote_version: 0,
            has_stale_layouts: false,
            last_invalidation_version: global_layout_cache::get_invalidation_version(),
            last_layout_eviction_seq: global_layout_cache::layout_eviction_seq(),
            last_layout_settings_version_seen: settings::get_layout_settings_version(),
            last_layout_scale_bits: layout_scale_factor.to_bits(),
            last_layout_mod_buttons: false,
            resize_in_progress: false,
            active_layout_quantized_width: quantize_width(container_width),
            pending_layout_quantized_width: None,
            pending_layout_quantized_since: None,
            pending_layout_quantized_run_since: None,
            stale_layout_refresh_cursor: 0,
            stale_layout_refresh_pass_found_stale: false,
            refresh_missing_layouts_for_width: false,
            relayout_cause: None,
        }
    }

    #[inline]
    pub(super) fn clear_pending_width_bucket(&mut self) {
        self.pending_layout_quantized_width = None;
        self.pending_layout_quantized_since = None;
        self.pending_layout_quantized_run_since = None;
    }

    /// Keep the stale-layout scan on the row it would scan next after the
    /// first `dropped` rows left, so the pass neither skips the rows that
    /// moved behind it nor ends early.
    #[inline]
    pub(super) fn shift_stale_scan_after_front_drop(&mut self, dropped: usize) {
        self.stale_layout_refresh_cursor = self.stale_layout_refresh_cursor.saturating_sub(dropped);
    }

    /// Restart the progressive stale-layout scan from the first row.
    #[inline]
    pub(super) fn reset_stale_scan(&mut self) {
        self.stale_layout_refresh_cursor = 0;
        self.stale_layout_refresh_pass_found_stale = false;
    }

    /// Mark layouts stale and restart the progressive stale-layout scan.
    #[inline]
    pub(super) fn mark_stale(&mut self) {
        self.has_stale_layouts = true;
        self.reset_stale_scan();
    }

    /// Mark layouts stale and restart the progressive width refresh pass.
    #[inline]
    pub(super) fn restart_width_refresh(&mut self) {
        self.mark_stale();
        self.refresh_missing_layouts_for_width = true;
    }

    /// Drop pending stale and width refresh work; a full rebuild supersedes it.
    #[inline]
    pub(super) fn clear_stale_refresh(&mut self) {
        self.has_stale_layouts = false;
        self.refresh_missing_layouts_for_width = false;
        self.reset_stale_scan();
    }
}

pub(super) struct ViewportFrameState {
    pub(super) render_element_buffer: Vec<AnyElement>,
    pub(super) visible_render_rows: Vec<VisibleRenderRow>,
    pub(super) cached_viewport_paint_rows: Option<Arc<Vec<ViewportPaintRow>>>,
    pub(super) spare_viewport_paint_rows: Vec<ViewportPaintRow>,
    pub(super) cached_viewport_paint_scroll_offset: f32,
    /// `window.scale_factor()` in effect when `cached_viewport_paint_rows` was
    /// prepared. 7TV name-paint overlays are composited at the device scale at
    /// prepare time (see `seventv_paint`), so a cross-frame reuse of those rows
    /// at a different scale would paint a stale-scale overlay. Reuse (snapshot
    /// or per-row) is only eligible when the current scale matches this.
    pub(super) cached_viewport_paint_rows_scale: f32,
    pub(super) cached_visible_render_range: Option<(usize, usize)>,
    pub(super) cached_visible_render_scroll_offset: f32,
    pub(super) cached_visible_render_quantized_width: u32,
    pub(super) cached_visible_render_message_count: usize,
    /// A visible row still carries a windowless (async) layout that the
    /// rebuild path upgrades a few rows per frame, so frames must not be reused.
    pub(super) visible_rows_have_fallback_metrics: bool,
    /// Scratch map reused across scroll-translation frames to avoid per-frame allocation.
    /// Taken via mem::take before drain, restored (cleared) afterward.
    pub(super) scratch_retained_rows: FxHashMap<usize, VisibleRenderRow>,
    pub(super) relayout_geometry: Option<Box<VisibleGeometryTrace>>,
    /// Animated images the viewport element last painted as patchable slots.
    /// Shared with the element, which refills it whenever it paints.
    pub(super) painted_animation_slots: Rc<RefCell<PaintedAnimationSlots>>,
    /// A visible row painted as GPUI elements (not the viewport canvas) shows
    /// animated content, which only a repaint can advance.
    pub(super) fallback_rows_animate: bool,
    /// Sorted image URL hashes last published as this canvas's visible set
    /// (`image_cache::publish_chat_visible_media`).
    pub(super) published_media: Vec<u64>,
    /// The visible row layouts `published_media` was built from. Held, so a
    /// pointer can never be reused by a different layout while it is
    /// compared; an identical list means the URL set is unchanged.
    pub(super) published_media_layouts: Vec<Arc<CanvasMessageLayout>>,
    /// Reused buffer for building the next visible set.
    pub(super) media_scratch: Vec<u64>,
}

/// An animated image showed its static stand-in; the bit is shifted by its
/// `image_cache::AnimatedCacheState` tag (not loaded, loading, ready, failed).
pub(super) const UNSLOTTED_IMAGE_STATE_SHIFT: u8 = 0;
/// An animated name paint was painted without its composite.
pub(super) const UNSLOTTED_NAME_PAINT: u8 = 1 << 4;

/// What an animation slot shows, so the waker can find its current frame.
pub(super) enum AnimationSlotSource {
    /// An animated image, by URL.
    Image(SharedString),
    /// The frame-dependent sprite of an animated 7TV name paint.
    NamePaint {
        spec: Arc<super::super::seventv_paint::SevenTvLinePaint>,
        shaped_line: Rc<ShapedLine>,
        font_size: f32,
        line_height: f32,
        /// Sprite placement when painted; a frame with another placement
        /// needs a repaint.
        geometry: [f32; 4],
    },
}

/// Animated content in the viewport canvas's last paint.
///
/// Each slot is an animated image painted with `Window::paint_animated_image`,
/// so the animation waker can show its next frame with
/// `Window::patch_animated_image` instead of repainting the split.
#[derive(Default)]
pub(super) struct PaintedAnimationSlots {
    pub(super) slots: Vec<(u64, AnimationSlotSource)>,
    /// `UNSLOTTED_*` bits for animated content painted without a slot.
    pub(super) unslotted: u8,
}

impl ViewportFrameState {
    pub(super) fn initial() -> Self {
        Self {
            render_element_buffer: Vec::with_capacity(256),
            visible_render_rows: Vec::with_capacity(256),
            cached_viewport_paint_rows: None,
            spare_viewport_paint_rows: Vec::with_capacity(256),
            cached_viewport_paint_scroll_offset: 0.0,
            cached_viewport_paint_rows_scale: 0.0,
            cached_visible_render_range: None,
            cached_visible_render_scroll_offset: 0.0,
            cached_visible_render_quantized_width: 0,
            cached_visible_render_message_count: 0,
            visible_rows_have_fallback_metrics: false,
            scratch_retained_rows: FxHashMap::default(),
            relayout_geometry: None,
            painted_animation_slots: Rc::default(),
            fallback_rows_animate: false,
            published_media: Vec::new(),
            published_media_layouts: Vec::new(),
            media_scratch: Vec::new(),
        }
    }
}

#[derive(Default)]
pub(super) struct WakeState {
    pub(super) animation_waker_running: bool,
    /// Cancellation epoch for detached animation tasks. A retained view can be
    /// hidden and shown again before its previous timer wakes, so a shared bool
    /// alone cannot distinguish the old loop from its replacement.
    pub(super) animation_waker_generation: u64,
    pub(super) animation_only_frame_pending: bool,
    pub(super) async_layout_waker_running: bool,
    pub(super) image_load_waker_running: bool,
    pub(super) width_stabilization_waker_running: bool,
    pub(super) has_visible_animations_last_frame: bool,
    /// Key of this split's entry in the shared animated-emote census.
    pub(super) animation_census_owner: Option<u64>,
    pub(super) has_visible_image_loads_last_frame: bool,
    /// Deadline of the scheduled wake for a visible failed image's next retry.
    pub(super) failed_image_retry_wake_at: Option<std::time::Instant>,
    pub(super) last_visible_animation_frame_signature: Option<u64>,
    /// The window's present count when the animation waker last looked.
    pub(super) last_observed_present_count: u64,
    /// When the running animation waker next checks this split's frames.
    pub(super) animation_waker_due: Option<std::time::Instant>,
    pub(super) last_visible_image_load_signature: Option<u64>,
    /// Image-load waker ticks since the visible load signature last changed.
    pub(super) image_load_unchanged_ticks: u16,
    pub(super) has_async_layout_in_flight_last_frame: bool,
    /// Bumped whenever the hover target changes or is dropped, so a hover-link
    /// metadata request started for an earlier target is discarded.
    pub(super) hover_generation: u64,
    /// Cancellation epoch and liveness of the drag-selection edge-scroll loop.
    pub(super) selection_edge_scroll_generation: u64,
    pub(super) selection_edge_scroll_running: bool,
    /// Whether the platform is presenting this canvas's window. A hidden window
    /// cannot present frames, so animation wake loops stop until it is shown.
    pub(super) window_hidden: bool,
    /// Whether the animation waker already nudged a repaint since the canvas
    /// last counted as stalled.
    pub(super) stall_repaint_requested: bool,
    pub(super) window_visibility_subscription: Option<gpui::Subscription>,
    pub(super) window_activation_subscription: Option<gpui::Subscription>,
    /// Renders in a row that requested a settle frame for themselves.
    pub(super) consecutive_settle_frames: u16,
}

/// Settle frames a canvas requests for itself in a row before it stops. Each
/// settle frame refreshes or rebuilds a bounded number of visible rows, so a
/// full viewport converges well within this; the cap only ends a loop whose
/// condition fails to clear.
pub(super) const MAX_CONSECUTIVE_SETTLE_FRAMES: u16 = 120;

impl WakeState {
    /// Whether this render should request a settle frame for itself, given
    /// that it still needs one. A render that needs none, or whose next frame
    /// is already coming (`next_frame_covered`: a scroll-animation step or the
    /// async-layout waker), resets the count, so the cap only counts settle
    /// frames that keep the canvas rendering on their own.
    pub(super) fn take_settle_frame_request(
        &mut self,
        needs_settle: bool,
        next_frame_covered: bool,
    ) -> bool {
        if !needs_settle || next_frame_covered {
            self.consecutive_settle_frames = 0;
            return false;
        }
        if self.consecutive_settle_frames >= MAX_CONSECUTIVE_SETTLE_FRAMES {
            return false;
        }
        self.consecutive_settle_frames += 1;
        if self.consecutive_settle_frames == MAX_CONSECUTIVE_SETTLE_FRAMES {
            crate::log_ui!(
                warn,
                "Canvas stopped requesting settle frames after {} in a row",
                MAX_CONSECUTIVE_SETTLE_FRAMES
            );
        }
        true
    }

    /// Invalidate work started for the previous hover target.
    #[inline]
    pub(super) fn bump_hover_generation(&mut self) {
        self.hover_generation = self.hover_generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_CONSECUTIVE_SETTLE_FRAMES, WakeState};
    use core::prelude::v1::test;

    #[test]
    fn settle_frame_requests_stop_at_the_cap_until_a_render_settles() {
        let mut state = WakeState::default();
        for _ in 0..MAX_CONSECUTIVE_SETTLE_FRAMES {
            assert!(state.take_settle_frame_request(true, false));
        }
        assert!(!state.take_settle_frame_request(true, false));
        assert!(!state.take_settle_frame_request(true, false));

        assert!(!state.take_settle_frame_request(false, false));
        assert!(state.take_settle_frame_request(true, false));
    }

    #[test]
    fn a_settled_render_requests_no_frame_and_resets_the_count() {
        let mut state = WakeState::default();
        assert!(state.take_settle_frame_request(true, false));
        assert!(state.take_settle_frame_request(true, false));
        assert!(!state.take_settle_frame_request(false, false));
        assert_eq!(state.consecutive_settle_frames, 0);
    }

    #[test]
    fn frames_a_scroll_animation_requested_do_not_count_toward_the_cap() {
        let mut state = WakeState::default();
        for _ in 0..3 * MAX_CONSECUTIVE_SETTLE_FRAMES {
            assert!(!state.take_settle_frame_request(true, true));
        }
        assert_eq!(state.consecutive_settle_frames, 0);
        // The first frame after the animation ends still gets its settle frame.
        assert!(state.take_settle_frame_request(true, false));
    }
}
