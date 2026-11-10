//! Split-local wake-loop control for chat viewport rendering.

use super::*;

/// Image-load waker ticks (50 ms each) between re-requests of unserved visible
/// images while their load state does not change.
const IMAGE_LOAD_REREQUEST_TICKS: u16 = 10;

impl CanvasMessageList {
    #[inline]
    pub(super) fn release_animation_waker(&mut self) {
        self.wake_state.animation_waker_generation =
            self.wake_state.animation_waker_generation.wrapping_add(1);
        self.wake_state.animation_waker_running = false;
        self.wake_state.animation_only_frame_pending = false;
        self.wake_state.last_visible_animation_frame_signature = None;
        self.wake_state.animation_waker_due = None;
    }

    /// Track the platform presentation state of the window hosting this canvas.
    ///
    /// Registered lazily on the first render because the canvas is constructed
    /// without a `Window`.
    pub(super) fn ensure_window_visibility_observer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.wake_state.window_activation_subscription.is_none() {
            self.wake_state.window_activation_subscription =
                Some(cx.observe_window_activation(window, |this, window, cx| {
                    if window.is_window_active() || !this.is_tab_visible {
                        return;
                    }
                    this.mark_last_read();
                    // A hover pause belongs to an attended pointer; once focus
                    // moves elsewhere chat must not stay frozen behind it.
                    let pause_ended = this.clear_chat_hover_pause();
                    if pause_ended || settings::get_show_last_read_indicator() {
                        cx.notify();
                    }
                }));
        }
        if self.wake_state.window_visibility_subscription.is_some() {
            return;
        }
        self.wake_state.window_hidden = !window.is_visible();
        self.wake_state.window_visibility_subscription = Some(cx.observe_window_visibility(
            window,
            |this, visibility, _window, cx| {
                let hidden = !visibility.is_visible();
                if this.wake_state.window_hidden == hidden {
                    return;
                }
                this.wake_state.window_hidden = hidden;
                if hidden {
                    this.release_animation_waker();
                    if let Some(owner) = this.wake_state.animation_census_owner {
                        crate::emote_renderer::remove_owner(owner);
                    }
                } else {
                    crate::backend::metrics::record_ui_notify_source("canvas.window_shown");
                    cx.notify();
                }
            },
        ));
    }

    #[inline]
    pub(super) fn release_async_layout_waker(&mut self) {
        self.wake_state.async_layout_waker_running = false;
    }

    #[inline]
    pub(super) fn release_image_load_waker(&mut self) {
        self.wake_state.image_load_waker_running = false;
    }

    #[inline]
    pub(super) fn release_width_stabilization_waker(&mut self) {
        self.wake_state.width_stabilization_waker_running = false;
    }

    #[inline]
    pub(super) fn sync_global_tooltip_overlay_from_state(&mut self) -> bool {
        if self.interaction_state.tooltip.should_show()
            && let (Some(owner_window), Some(kind)) = (
                self.interaction_state.tooltip.owner_window,
                self.interaction_state.tooltip.hovered.clone(),
            )
        {
            show_canvas_hitbox_tooltip(
                owner_window,
                self.interaction_state.tooltip_overlay_source_id.clone(),
                kind,
                self.interaction_state.tooltip.position,
            );
            return true;
        }

        if let Some(owner_window) = self.interaction_state.tooltip.owner_window {
            clear_global_tooltip_if_source_in_window(
                owner_window,
                &self.interaction_state.tooltip_overlay_source_id,
            );
        } else {
            clear_global_tooltip_if_source(&self.interaction_state.tooltip_overlay_source_id);
        }
        false
    }

    pub(super) fn schedule_tooltip_delay_wake(&mut self, cx: &mut Context<Self>) {
        let Some(delay) = self.interaction_state.tooltip.remaining_delay() else {
            return;
        };
        if delay.is_zero() {
            return;
        }

        self.wake_state.tooltip_delay_wake_generation = self
            .wake_state
            .tooltip_delay_wake_generation
            .wrapping_add(1);
        let generation = self.wake_state.tooltip_delay_wake_generation;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            let _ = this.update(cx, |this, cx| {
                if this.wake_state.tooltip_delay_wake_generation != generation {
                    return;
                }

                let became_visible = this.interaction_state.tooltip.poll_delay_visibility();
                let overlay_visible = this.sync_global_tooltip_overlay_from_state();
                if became_visible || overlay_visible {
                    crate::backend::metrics::record_ui_notify_source("canvas.tooltip_delay");
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Repaint at `deadline`, when the loader next accepts a retry of a
    /// visible failed image, so an idle view still retries it. The loader's
    /// retry budget bounds the chain: an exhausted URL has no deadline.
    pub(super) fn schedule_failed_image_retry_wake(
        &mut self,
        deadline: std::time::Instant,
        cx: &mut Context<Self>,
    ) {
        if self
            .wake_state
            .failed_image_retry_wake_at
            .is_some_and(|scheduled| scheduled <= deadline)
        {
            return;
        }
        self.wake_state.failed_image_retry_wake_at = Some(deadline);

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(deadline.saturating_duration_since(std::time::Instant::now()))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.wake_state.failed_image_retry_wake_at == Some(deadline) {
                    this.wake_state.failed_image_retry_wake_at = None;
                }
                crate::backend::metrics::record_ui_notify_source("canvas.failed_image_retry");
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn schedule_search_focus_clear(&mut self, cx: &mut Context<Self>) {
        self.search_focus_generation = self.search_focus_generation.wrapping_add(1);
        let generation = self.search_focus_generation;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(
                    SEARCH_RESULT_FOCUS_DURATION_MS,
                ))
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.search_focus_generation == generation
                    && this.search_focus_message_id.is_some()
                {
                    this.search_focus_message_id = None;
                    crate::backend::metrics::record_ui_notify_source("canvas.search_focus_clear");
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Ensure a paced animation wake loop is running while visible animations exist.
    ///
    /// The loop wakes when this split's own earliest visible animation changes
    /// frame (`first_change_ms` seeds the first wake), so a split showing only
    /// slow animations is not checked at the cadence of the fastest animation
    /// in another split.
    pub(super) fn ensure_animation_waker(
        &mut self,
        first_change_ms: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.wake_state.animation_waker_running {
            return;
        }
        if !self.is_tab_visible || self.wake_state.window_hidden {
            return;
        }
        if !self.wake_state.has_visible_animations_last_frame {
            return;
        }
        self.wake_state.animation_waker_generation =
            self.wake_state.animation_waker_generation.wrapping_add(1);
        let generation = self.wake_state.animation_waker_generation;
        self.wake_state.animation_waker_running = true;

        /// Outcome of one waker pass, decided while holding the entity.
        enum WakerStep {
            /// Waker released or no longer needed; end the loop.
            Stop,
            /// Normal tick happened; wait this long for the next one.
            Tick(std::time::Duration),
            /// Window is not presenting (minimized/hidden); keep the waker
            /// alive but back off and pause the shared animation clock.
            Stalled,
        }
        /// Poll cadence while the window cannot present frames.
        const STALLED_POLL_MS: u64 = 500;

        let mut delay = self.schedule_animation_wake(first_change_ms);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(delay).await;

                let Ok(step) = this.update_in(cx, |this, window, cx| {
                    if this.wake_state.animation_waker_generation != generation {
                        return WakerStep::Stop;
                    }

                    if !this.wake_state.animation_waker_running {
                        return WakerStep::Stop;
                    }

                    if !this.is_tab_visible {
                        this.release_animation_waker();
                        return WakerStep::Stop;
                    }

                    if !crate::emote_renderer::animation_enabled() {
                        this.release_animation_waker();
                        return WakerStep::Stop;
                    }

                    if !this.wake_state.has_visible_animations_last_frame {
                        this.release_animation_waker();
                        return WakerStep::Stop;
                    }

                    let presented = window.frame_stage_statistics().frames_presented;
                    if presented != this.wake_state.last_observed_present_count {
                        this.wake_state.last_observed_present_count = presented;
                        crate::fps_profiler::record_frame_presented();
                    }
                    if crate::fps_profiler::canvas_paint_stalled() {
                        this.wake_state.animation_waker_due = None;
                        // The window is shown, so the stall may only mean every
                        // visible animation held its frame for over a second
                        // (unchanged ticks do not paint). One repaint ends that
                        // stall; a window that really cannot present ignores it.
                        if !this.wake_state.window_hidden
                            && !this.wake_state.stall_repaint_requested
                        {
                            this.wake_state.stall_repaint_requested = true;
                            crate::backend::metrics::record_ui_notify_source(
                                "canvas.animation_stall_nudge",
                            );
                            cx.notify();
                        }
                        return WakerStep::Stalled;
                    }
                    this.wake_state.stall_repaint_requested = false;

                    crate::emote_renderer::tick_animation();
                    this.render_perf_agg_stats.animation_waker_checks_total += 1;

                    let frame = this.visible_animation_frame_signature();
                    let next_delay =
                        this.schedule_animation_wake(frame.and_then(|frame| frame.next_change_ms));
                    if let Some(frame) = frame {
                        if this.wake_state.last_visible_animation_frame_signature
                            == Some(frame.signature)
                        {
                            crate::backend::metrics::record_emote_event(
                                "canvas.animation_frame_unchanged_skipped",
                            );
                            this.render_perf_agg_stats
                                .animation_frame_unchanged_skips_total += 1;
                            return WakerStep::Tick(next_delay);
                        }
                        this.wake_state.last_visible_animation_frame_signature =
                            Some(frame.signature);
                    } else {
                        this.wake_state.last_visible_animation_frame_signature = None;
                    }

                    let outcome = this.patch_visible_animation_frames(window);
                    this.render_perf_agg_stats
                        .record_animation_patch_outcome(outcome);
                    if outcome == AnimationPatchOutcome::Patched {
                        // Patched frames replace renders, which are what
                        // otherwise release evicted frames from the atlas.
                        crate::utils::drain_render_image_drops_coalesced(window, cx);
                        crate::fps_profiler::sample_frame_stages(window);
                        return WakerStep::Tick(next_delay);
                    }
                    this.wake_state.animation_only_frame_pending = true;
                    crate::backend::metrics::record_ui_notify_source("canvas.animation_waker");
                    cx.notify();
                    WakerStep::Tick(next_delay)
                }) else {
                    break;
                };

                delay = match step {
                    WakerStep::Tick(next_delay) => next_delay,
                    WakerStep::Stalled => std::time::Duration::from_millis(STALLED_POLL_MS),
                    WakerStep::Stop => break,
                };
            }
        })
        .detach();
    }

    /// Show the current frame of every animated image in the presented frame
    /// without repainting this split.
    ///
    /// Only possible when the viewport canvas painted all visible animated
    /// content as slots: no animated name paint, no animated image still on its
    /// static stand-in, and no animated row painted as GPUI elements. Any other
    /// outcome means the split must repaint; slots already patched by then are
    /// harmless, since the repaint shows the same frames.
    fn patch_visible_animation_frames(&self, window: &mut Window) -> AnimationPatchOutcome {
        if !animation_patching_enabled() {
            return AnimationPatchOutcome::Disabled;
        }
        if self.viewport_state.fallback_rows_animate {
            return AnimationPatchOutcome::FallbackRows;
        }
        let painted = self.viewport_state.painted_animation_slots.borrow();
        if painted.unslotted != 0 {
            return AnimationPatchOutcome::Unslotted(painted.unslotted);
        }
        if painted.slots.is_empty() {
            return AnimationPatchOutcome::NoSlots;
        }
        for (key, source) in &painted.slots {
            let failure = match source {
                AnimationSlotSource::Image(url) => {
                    match image_cache::get_current_animated_render_image(url.as_ref()) {
                        None => Some(PatchFailure::NoFrame),
                        Some(image) => (!window.patch_animated_image(*key, &image, 0))
                            .then_some(PatchFailure::Rejected),
                    }
                }
                AnimationSlotSource::NamePaint {
                    spec,
                    shaped_line,
                    font_size,
                    line_height,
                    geometry,
                } => match super::super::seventv_paint::overlay_for_line(
                    spec,
                    shaped_line,
                    *font_size,
                    *line_height,
                    window,
                ) {
                    None => Some(PatchFailure::NoComposite),
                    Some(overlay) => match overlay.patch_image() {
                        None => Some(PatchFailure::NoFrame),
                        Some(_) if overlay.geometry() != *geometry => Some(PatchFailure::Geometry),
                        Some(image) => (!window.patch_animated_image(*key, image, 0))
                            .then_some(PatchFailure::Rejected),
                    },
                },
            };
            if let Some(failure) = failure {
                return AnimationPatchOutcome::PatchFailed(failure);
            }
        }
        AnimationPatchOutcome::Patched
    }

    /// Delay until this split's next animation check, recorded as its due time.
    ///
    /// With a known next frame change the wake lands just after it, but no
    /// sooner than the next point of the shared animation cadence (the cap),
    /// and no later than the scheduler's longest tick so a missed change
    /// recovers. The floor is that cadence point rather than a full interval
    /// from now, so timer lateness is absorbed instead of accumulating into
    /// skipped frames; deadlines past it are not rounded, and GPUI folds the
    /// notifies of several splits into its next window frame. Without frame
    /// timing (frames still decoding) the shared animation cadence applies.
    fn schedule_animation_wake(&mut self, next_change_ms: Option<u64>) -> std::time::Duration {
        use crate::managers::dynamic_tick_scheduler::{
            aligned_tick_delay, animation_min_tick_interval_ms,
        };
        use echo_render_core::animation_timing::MAX_TICK_INTERVAL_MS;

        let min_tick = animation_min_tick_interval_ms();
        let delay = match next_change_ms {
            Some(change_ms) => std::time::Duration::from_millis(
                (change_ms + 1).min(MAX_TICK_INTERVAL_MS.max(min_tick)),
            )
            .max(aligned_tick_delay(min_tick)),
            None => aligned_tick_delay(
                crate::emote_renderer::current_animation_interval_ms().max(min_tick),
            ),
        };
        self.wake_state.animation_waker_due = Some(std::time::Instant::now() + delay);
        delay
    }

    /// Restart the animation waker when a render shows an animation that
    /// changes frame before the waker's scheduled check (new rows, scroll), so
    /// the new animation does not wait out a slower split-local deadline.
    pub(super) fn reschedule_animation_waker_if_late(&mut self, next_change_ms: Option<u64>) {
        let (Some(change_ms), Some(due)) = (next_change_ms, self.wake_state.animation_waker_due)
        else {
            return;
        };
        if !self.wake_state.animation_waker_running {
            return;
        }
        let grid = std::time::Duration::from_millis(
            crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms(),
        );
        let change_at = std::time::Instant::now() + std::time::Duration::from_millis(change_ms);
        if due > change_at + grid {
            self.wake_state.animation_waker_generation =
                self.wake_state.animation_waker_generation.wrapping_add(1);
            self.wake_state.animation_waker_running = false;
            self.wake_state.animation_waker_due = None;
        }
    }

    /// Ensure a paced wake loop is running while async layouts are in flight.
    pub(super) fn ensure_async_layout_waker(&mut self, cx: &mut Context<Self>) {
        if self.wake_state.async_layout_waker_running {
            return;
        }
        if !self.wake_state.has_async_layout_in_flight_last_frame {
            return;
        }
        self.wake_state.async_layout_waker_running = true;

        cx.spawn(async move |this, cx| {
            loop {
                let interval_ms =
                    crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(interval_ms))
                    .await;

                let Ok(should_continue) = this.update(cx, |this, cx| {
                    if !this.wake_state.async_layout_waker_running {
                        return false;
                    }

                    if !this.wake_state.has_async_layout_in_flight_last_frame {
                        this.release_async_layout_waker();
                        return false;
                    }

                    let completed = this.poll_async_layouts();

                    let was_in_flight = this.wake_state.has_async_layout_in_flight_last_frame;
                    this.wake_state.has_async_layout_in_flight_last_frame =
                        this.async_layout_manager.has_in_flight();

                    if completed > 0
                        || (was_in_flight && !this.wake_state.has_async_layout_in_flight_last_frame)
                    {
                        crate::backend::metrics::record_ui_notify_source(
                            "canvas.async_layout_waker",
                        );
                        cx.notify();
                    }

                    if !this.wake_state.has_async_layout_in_flight_last_frame {
                        this.release_async_layout_waker();
                        return false;
                    }

                    true
                }) else {
                    break;
                };

                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    /// Ensure a split-local wake loop is running while visible direct-paint images are loading.
    ///
    /// This mirrors GPUI's asset pattern: the view that requested/uses the image is the view that
    /// gets notified when the image generation advances. Routing this through app/root state causes
    /// every split to refresh when one split scrolls into uncached emotes.
    pub(super) fn ensure_image_load_waker(&mut self, cx: &mut Context<Self>) {
        if self.wake_state.image_load_waker_running {
            return;
        }
        if !self.is_tab_visible || self.wake_state.window_hidden {
            return;
        }
        if !self.wake_state.has_visible_image_loads_last_frame {
            return;
        }

        self.wake_state.last_visible_image_load_signature = self.visible_image_load_signature();
        self.wake_state.image_load_unchanged_ticks = 0;
        self.wake_state.image_load_waker_running = true;

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;

                let Ok(should_continue) = this.update(cx, |this, cx| {
                    if !this.wake_state.image_load_waker_running {
                        return false;
                    }

                    // Showing the tab or window rebuilds the rows, and that
                    // render restarts the waker.
                    if !this.wake_state.has_visible_image_loads_last_frame
                        || !this.is_tab_visible
                        || this.wake_state.window_hidden
                    {
                        this.release_image_load_waker();
                        return false;
                    }

                    let signature = this.visible_image_load_signature();
                    let changed = signature != this.wake_state.last_visible_image_load_signature;
                    if changed {
                        this.wake_state.last_visible_image_load_signature = signature;
                        this.wake_state.image_load_unchanged_ticks = 0;
                        if this.viewport_state.visible_render_rows.iter().any(|row| {
                            !row.layout.pending_native_dimension_urls.is_empty()
                                && row.layout.is_stale_for_channel()
                        }) {
                            this.layout_store.runtime.has_stale_layouts = true;
                        }
                        crate::backend::metrics::record_ui_notify_source(
                            "canvas.image_load_waker.visible_url",
                        );
                        cx.notify();
                    } else {
                        this.wake_state.image_load_unchanged_ticks =
                            this.wake_state.image_load_unchanged_ticks.wrapping_add(1);
                    }

                    if signature.is_none() {
                        this.release_image_load_waker();
                        return false;
                    }

                    // The notify only repaints, and a repaint of an unchanged
                    // viewport reuses its rows without the preload scans. A load
                    // that lost its request (e.g. to the IRC disk-only prefetch
                    // holding the URL) would otherwise never be asked for again.
                    if changed
                        || this.wake_state.image_load_unchanged_ticks % IMAGE_LOAD_REREQUEST_TICKS
                            == 0
                    {
                        this.request_unserved_visible_images();
                    }

                    true
                }) else {
                    break;
                };

                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    /// Ensure pending width-bucket changes get a follow-up render after the
    /// stabilization window. Without this, the only follow-up frame can arrive
    /// too early and leave the pending bucket stuck until user input or resize.
    pub(super) fn ensure_width_stabilization_waker(&mut self, cx: &mut Context<Self>) {
        if self.wake_state.width_stabilization_waker_running {
            return;
        }
        let Some(since) = self.layout_store.runtime.pending_layout_quantized_since else {
            return;
        };
        if self
            .layout_store
            .runtime
            .pending_layout_quantized_width
            .is_none()
        {
            return;
        }

        let elapsed_ms = since.elapsed().as_millis();
        let target_delay_ms = if elapsed_ms < WIDTH_BUCKET_STABILIZE_MS {
            WIDTH_BUCKET_STABILIZE_MS - elapsed_ms
        } else if elapsed_ms < WIDTH_BUCKET_FORCE_APPLY_MS {
            WIDTH_BUCKET_FORCE_APPLY_MS - elapsed_ms
        } else {
            1
        };
        let delay = std::time::Duration::from_millis(target_delay_ms.max(1) as u64);

        self.wake_state.width_stabilization_waker_running = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            let _ = this.update(cx, |this, cx| {
                this.release_width_stabilization_waker();
                if this
                    .layout_store
                    .runtime
                    .pending_layout_quantized_width
                    .is_some()
                {
                    crate::backend::metrics::record_ui_notify_source(
                        "canvas.width_stabilization_waker",
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// Whether animation frames may be patched into the presented frame.
/// `ECHO_ANIMATION_PATCH=0` repaints every frame change instead.
fn animation_patching_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("ECHO_ANIMATION_PATCH").map_or(true, |value| value.trim() != "0")
    });
    *ENABLED
}

/// How an animation-waker frame change reached the screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AnimationPatchOutcome {
    /// Patched into the presented frame without a repaint.
    Patched,
    /// Repainted: an animated row is painted as GPUI elements.
    FallbackRows,
    /// Repainted: animated content was painted without a slot (`UNSLOTTED_*` bits).
    Unslotted(u8),
    /// Repainted: the last canvas paint recorded no slots.
    NoSlots,
    /// Repainted: the window could not patch a slot.
    PatchFailed(PatchFailure),
    /// Repainted: patching is turned off (`ECHO_ANIMATION_PATCH=0`).
    Disabled,
}

/// Why a slot could not be patched.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchFailure {
    /// The animated image has no current frame.
    NoFrame,
    /// The name paint could not be composited for the current frame.
    NoComposite,
    /// The name paint composite moved or changed size.
    Geometry,
    /// The window has no patchable sprite for the slot.
    Rejected,
}

impl AnimationPatchOutcome {
    pub(super) const ALL: [Self; 6] = [
        Self::Patched,
        Self::FallbackRows,
        Self::Unslotted(0),
        Self::NoSlots,
        Self::PatchFailed(PatchFailure::Rejected),
        Self::Disabled,
    ];

    /// Index into `RenderPerfAggregateStats::animation_patch_outcomes`.
    pub(super) fn index(self) -> usize {
        match self {
            Self::Patched => 0,
            Self::FallbackRows => 1,
            Self::Unslotted(_) => 2,
            Self::NoSlots => 3,
            Self::PatchFailed(_) => 4,
            Self::Disabled => 5,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Patched => "patched",
            Self::FallbackRows => "fallback_rows",
            Self::Unslotted(_) => "unslotted",
            Self::NoSlots => "no_slots",
            Self::PatchFailed(_) => "patch_failed",
            Self::Disabled => "disabled",
        }
    }
}
