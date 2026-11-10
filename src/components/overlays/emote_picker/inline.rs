//! Emote picker overlay for Echo GPUI
//!
//! Provides a grid-based emote picker with:
//! - Tab-based navigation (Favorites, Channel, Global)
//! - Favorites system with add/remove and persistence
//! - Recent/frequent emotes tracking with special sections
//! - Collapsible category headers with provider grouping
//! - Real search input with debouncing
//! - Keyboard navigation
//! - Emote tooltips
//! - Popout window support

use crate::backend::Emote;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::components::overlays::traits::{Openable, OverlayPointerIsolation};
use crate::state::SplitId;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::core::PackedGridNavigation;
pub use super::core::{
    EmotePickerCore, EmoteTab, picker_display_url, picker_native_animated_enabled,
};
use super::grid::{
    self, MIN_ROW_WIDTH, PICKER_ANIMATION_WAKE_MAX_MS, PickerGrid, PickerHost, PickerMetricNames,
    PickerShell, PrefetchReport, ROW_WIDTH_OVERHEAD, SEARCH_DEBOUNCE_MS, SearchStyle, TabBarStyle,
};
use super::prefetch::PICKER_INITIAL_PREFETCH_ROWS;
use super::rows::{self, GridElementIds, GridStyle, HoverRepaint, PlaceholderStyle};
use super::shared::{PickerButtonExt, PickerFocusOrder, VirtualRow, render_load_more_warning};
use super::signals::INLINE_SIGNALS;
use crate::components::overlays::global_tooltip::clear_global_tooltip_if_source_prefix;
use crate::components::overlays::image_ready::ImageReadyConsumer;

/// Maximum picker width in pixels.
pub const EMOTE_PICKER_MAX_WIDTH: f32 = 620.0;
pub const EMOTE_PICKER_HEIGHT: f32 = 480.0;
/// Minimum picker width in pixels for very small windows.
const PICKER_MIN_WIDTH: f32 = 360.0;
/// Single source key for global emote-picker cell tooltips.
const EMOTE_PICKER_TOOLTIP_SOURCE: &str = "emote-picker-cell";

static INLINE_HOST: PickerHost = PickerHost {
    signals: &INLINE_SIGNALS,
    tooltip_source: EMOTE_PICKER_TOOLTIP_SOURCE,
    metrics: PickerMetricNames {
        rebuild_rows: "picker.inline.rebuild_virtual_rows",
        initial_prefetch_plan: "picker.inline.prefetch.initial_plan",
        viewport_prefetch_plan: "picker.inline.prefetch.viewport_plan",
        search_navigation_flush: "picker.search_navigation_flush",
    },
    initial_prefetch_animations: true,
    eager_prefetch: false,
    log_rebuild_perf: false,
    log_prefetch: log_inline_prefetch,
    tabs: EmoteTab::inline_tabs,
    search: SearchStyle {
        shell_id: "emote-picker-search-shell",
        fallback_id: "emote-picker-search-shell-fallback",
        clear_id: "emote-picker-clear-search-btn",
        clear_focus_key: "clear-search",
    },
    image_ready: ImageReadyConsumer::InlinePicker,
    repaint_on_image_ready: false,
    tab_bar: TabBarStyle {
        bar_id: "tab-bar",
        tab_id: "tab",
        height: 40.0,
        compact: false,
    },
    style: GridStyle {
        ids: GridElementIds {
            grid: "emote-grid",
            list: "emote-list",
            header: "header",
            header_row: "emote-header-row",
            load_more: "emote-load-more",
            load_more_button: "emote-picker-load-more",
            load_more_row: "emote-load-more-row",
            emote_row: "emote-row",
            emote_cell: "emote",
            animated_emote: "animated-emote",
        },
        highlight_special_headers: true,
        empty_state_icon: true,
        placeholder: PlaceholderStyle::Clipped,
        hover_repaint: HoverRepaint::NotifyPicker,
        track_ranges_without_emotes: true,
        record_stability: false,
        row_build_timing: None,
    },
};

fn log_inline_prefetch(report: &PrefetchReport) {
    crate::log_emotes!(
        debug,
        "prefetch_{}_range: rows {}..{} of {} total",
        report.kind.label(),
        report.rows.0,
        report.rows.1,
        report.total_rows
    );
    if report.requests() == 0 {
        crate::log_emotes!(
            debug,
            "prefetch_visible_range: nothing to prefetch (all cached)"
        );
    } else {
        crate::log_emotes!(
            debug,
            "Prefetching {} emotes: static={}, posters={}, animated={}",
            report.kind.label(),
            report.static_requests,
            report.poster_requests,
            report.animated_requests
        );
    }
}

impl PickerShell for EmotePicker {
    fn grid(&self) -> &PickerGrid {
        &self.grid
    }

    fn grid_mut(&mut self) -> &mut PickerGrid {
        &mut self.grid
    }

    fn activate_emote(&mut self, emote: Arc<Emote>, keep_open: bool, cx: &mut Context<Self>) {
        if self.state.is_open {
            self.confirm_emote(emote, keep_open, cx);
        }
    }

    fn accessible_channel(&self) -> &str {
        &self.state.channel
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        EmotePicker::clear_search(self, window, cx);
    }

    fn dismiss(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(true, cx);
    }

    fn search_changed(&mut self, text: String, cx: &mut Context<Self>) {
        self.schedule_search(text, cx);
    }

    fn search_enter_keeps_open(&self) -> bool {
        self.search_enter_keep_open
    }
}

/// Emote picker state (for external use)
#[derive(Clone, Debug)]
pub struct EmotePickerState {
    pub is_open: bool,
    pub x: f32,
    pub y: f32,
    pub channel: String,
    /// Track which split the emote picker was opened for.
    pub split_id: SplitId,
    /// Pending emote to be inserted (set when emote is selected)
    pub pending_emote: Option<String>,
}

impl Default for EmotePickerState {
    fn default() -> Self {
        Self {
            is_open: false,
            x: 0.0,
            y: 0.0,
            channel: String::new(),
            split_id: SplitId::new(),
            pending_emote: None,
        }
    }
}

impl Openable for EmotePickerState {
    fn set_open(&mut self, open: bool) {
        if !self.is_open && open {
            image_cache::begin_picker_media_scope();
        }
        if self.is_open && !open {
            image_cache::close_picker_media_scope();
        }
        self.is_open = open;
    }
}

impl EmotePickerState {
    pub fn open(x: f32, y: f32, channel: String, split_id: SplitId) -> Self {
        Self {
            is_open: true,
            x,
            y,
            channel,
            split_id,
            pending_emote: None,
        }
    }

    /// Close the emote picker (convenience method using Openable trait)
    pub fn close(&mut self) {
        Openable::close(self);
    }
}

/// Owned selection payload that survives picker close/reopen until the app consumes it.
#[derive(Clone, Debug)]
pub(crate) struct PendingPickerEmote {
    pub(crate) name: String,
    pub(crate) channel: String,
    pub(crate) split_id: SplitId,
}

pub(crate) enum EmotePickerEvent {
    PendingActions,
    PopoutRequested { channel: String, split_id: SplitId },
    PointerPressedOutside(Point<Pixels>),
}

impl EventEmitter<EmotePickerEvent> for EmotePicker {}

/// Emote picker component
pub struct EmotePicker {
    pub state: EmotePickerState,
    on_select: Option<Arc<dyn Fn(Emote) + Send + Sync>>,
    search_enter_keep_open: bool,
    open_state_notified: bool,
    pending_picker_focus: bool,
    composer_bounds: Option<Bounds<Pixels>>,
    /// Current picker width used for render/layout.
    picker_width: f32,
    /// Whether the picker-owned animation wake loop is running.
    animation_waker_running: bool,
    /// Invalidates a detached wake loop when the picker closes and reopens.
    animation_waker_generation: u64,
    /// Frames the visible cells displayed at the last wake-driven repaint.
    last_animation_frame_signature: Option<u64>,
    /// Whether heavyweight state has already been released for the current close.
    closed_state_cleared: bool,
    /// Selection routing is independent of mutable picker state until the app consumes it.
    pending_selections: VecDeque<PendingPickerEmote>,
    /// One-shot target for returning focus after an inline picker close.
    focus_return_split_id: Option<SplitId>,
    grid: PickerGrid,
}

impl EmotePicker {
    /// Create a new emote picker. Search input is lazily initialized on first render.
    pub fn new(state: EmotePickerState) -> Self {
        let mut core = EmotePickerCore::new(state.channel.clone());
        core.load_emotes();

        let mut picker = Self {
            state,
            on_select: None,
            search_enter_keep_open: false,
            open_state_notified: false,
            pending_picker_focus: false,
            composer_bounds: None,
            picker_width: EMOTE_PICKER_MAX_WIDTH,
            animation_waker_running: false,
            animation_waker_generation: 0,
            last_animation_frame_signature: None,
            closed_state_cleared: false,
            pending_selections: VecDeque::new(),
            focus_return_split_id: None,
            grid: PickerGrid::new(
                &INLINE_HOST,
                core,
                EMOTE_PICKER_MAX_WIDTH - ROW_WIDTH_OVERHEAD,
                crate::backend::settings::get_emote_picker_animation_mode(),
            ),
        };

        picker.grid.rebuild_virtual_rows();

        if !picker.grid.core.flat_emotes.is_empty() {
            picker.grid.prefetch_initial();
        }
        picker
    }

    fn clear_closed_picker_state(&mut self) {
        self.animation_waker_generation = self.animation_waker_generation.wrapping_add(1);
        self.animation_waker_running = false;
        self.last_animation_frame_signature = None;
        INLINE_SIGNALS.reset_visible_range();
        self.grid.core.clear_heavy_caches();
        self.grid.virtual_rows = Arc::new(Vec::new());
        self.grid.packed_navigation = PackedGridNavigation::default();
        self.grid.search_input = None;
        self.grid.grid_focus = None;
        self.grid.focus_order = PickerFocusOrder::default();
        self.grid.subscriptions.clear();
        self.grid.last_prefetch_range = None;
        self.grid.selected_emote_index = 0;
        self.grid.selected_emote_identity = None;
        self.grid.visible_emote_count = 0;
        self.grid.total_available_emotes = 0;
        self.grid.show_load_more_warning = false;
        self.grid.render_version = self.grid.render_version.wrapping_add(1);
    }

    fn cleanup_after_close(&mut self) {
        self.open_state_notified = false;
        self.grid.enter_down = false;
        self.grid.pending_grid_focus = false;
        self.pending_picker_focus = false;
        self.grid.invalidate_search();
        INLINE_SIGNALS.clear_pending_actions();
        clear_global_tooltip_if_source_prefix(EMOTE_PICKER_TOOLTIP_SOURCE);
        if !self.closed_state_cleared {
            self.clear_closed_picker_state();
            self.closed_state_cleared = true;
        }
    }

    /// Close the picker and release state that should not survive while hidden.
    ///
    /// The split/channel/pending-emote routing fields remain available to the app.
    pub fn close(&mut self, return_focus: bool, cx: &mut Context<Self>) {
        if return_focus {
            self.focus_return_split_id = Some(self.state.split_id);
        }
        self.state.close();
        self.cleanup_after_close();
        cx.emit(EmotePickerEvent::PendingActions);
        cx.notify();
    }

    /// Consume a pending request to restore focus to the split that opened the picker.
    pub fn take_focus_return_request(&mut self) -> Option<SplitId> {
        self.focus_return_split_id.take()
    }

    pub fn focus_return_requested(&self) -> bool {
        self.focus_return_split_id.is_some()
    }

    pub(crate) fn has_pending_selection(&self) -> bool {
        !self.pending_selections.is_empty()
    }

    pub(crate) fn take_pending_selection(&mut self) -> Option<PendingPickerEmote> {
        let selection = self.pending_selections.pop_front();
        self.state.pending_emote = self
            .pending_selections
            .front()
            .map(|selection| selection.name.clone());
        selection
    }

    fn reload_open_picker(&mut self) {
        self.grid.core.load_emotes();
        self.grid.reset_visible_emote_limit();
        self.grid.rebuild_virtual_rows();
        if !self.grid.core.flat_emotes.is_empty() {
            self.grid.prefetch_initial();
        }
    }

    fn update_layout_hints(&mut self, window: &Window) {
        let viewport_size = window.viewport_size();
        let viewport_width: f32 = viewport_size.width.into();
        let available_width = (viewport_width - 16.0).max(0.0);
        let min_width = PICKER_MIN_WIDTH.min(available_width);
        let next_picker_width = available_width.min(EMOTE_PICKER_MAX_WIDTH).max(min_width);
        let next_row_width = (next_picker_width - ROW_WIDTH_OVERHEAD).max(MIN_ROW_WIDTH);

        if (next_picker_width - self.picker_width).abs() >= 1.0 {
            self.picker_width = next_picker_width;
        }
        self.grid.set_row_width_hint(next_row_width);
    }

    fn schedule_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.grid.invalidate_search();
        let revision = self.grid.search_revision;
        let channel = self.state.channel.clone();

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;

            let _ = this.update(cx, move |picker, cx| {
                if picker.grid.search_revision != revision
                    || !picker.state.is_open
                    || picker.state.channel != channel
                {
                    return;
                }

                if picker.grid.core.search_query != query {
                    picker.grid.core.set_search(query);
                    picker.grid.reset_visible_emote_limit();
                    picker.grid.rebuild_virtual_rows();
                    picker.grid.reset_selection_to_first();
                }
                crate::backend::metrics::record_ui_notify_source("picker.search_debounce_apply");
                cx.notify();
            });
        })
        .detach();
    }

    pub fn update_state(&mut self, state: EmotePickerState) {
        let channel_changed = self.state.channel != state.channel;
        let was_open = self.state.is_open;
        let will_open = state.is_open;
        if !was_open && will_open {
            image_cache::begin_picker_media_scope();
        } else if was_open && !will_open {
            image_cache::close_picker_media_scope();
        }
        self.state = state;

        if !self.state.is_open {
            self.cleanup_after_close();
            return;
        }
        self.closed_state_cleared = false;

        if channel_changed {
            self.grid.invalidate_search();
            INLINE_SIGNALS.clear_pending_actions();
            clear_global_tooltip_if_source_prefix(EMOTE_PICKER_TOOLTIP_SOURCE);
        }

        if self.state.channel.is_empty() {
            return;
        }

        if channel_changed {
            self.grid.core.set_channel(self.state.channel.clone());
            self.grid.reset_visible_emote_limit();
            self.grid.rebuild_virtual_rows();
            self.grid.reset_selection_to_first();
            self.grid.prefetch_initial();
        } else if !was_open && will_open && self.grid.core.categories.is_empty() {
            self.reload_open_picker();
        }
    }

    pub(crate) fn set_composer_bounds(
        &mut self,
        bounds: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.composer_bounds != bounds {
            self.composer_bounds = bounds;
            cx.notify();
        }
    }

    /// Check if emotes have been updated and refresh if needed
    pub fn check_for_emote_updates(&mut self) -> bool {
        let updated = self.grid.refresh_emote_catalog();
        if updated {
            self.log_catalog_update();
        }
        updated
    }

    fn log_catalog_update(&self) {
        crate::log_emotes!(
            debug,
            "Emote picker detected update: flat_emotes={}, grid_items={}",
            self.grid.core.flat_emotes.len(),
            self.grid.core.grid_items.len()
        );
    }

    /// Clear the search input and reset search state
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref search_input) = self.grid.search_input {
            search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.grid.core.set_search(String::new());
        self.grid.reset_visible_emote_limit();
        self.grid.rebuild_virtual_rows();
        self.grid.reset_selection_to_first();
        self.grid.invalidate_search();
        if let Some(input) = &self.grid.search_input {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn confirm_emote(&mut self, emote: Arc<Emote>, keep_open: bool, cx: &mut Context<Self>) {
        self.grid.core.record_usage(&emote);
        let name = emote.name.to_string();
        self.pending_selections.push_back(PendingPickerEmote {
            name,
            channel: self.state.channel.clone(),
            split_id: self.state.split_id,
        });
        self.state.pending_emote = self
            .pending_selections
            .front()
            .map(|selection| selection.name.clone());

        if let Some(callback) = &self.on_select {
            callback((*emote).clone());
        }
        if keep_open {
            self.focus_return_split_id = None;
            self.pending_picker_focus = true;
            cx.emit(EmotePickerEvent::PendingActions);
            cx.notify();
        } else {
            self.close(true, cx);
        }
    }

    fn request_popout(&mut self, cx: &mut Context<Self>) {
        let channel = self.state.channel.clone();
        if !channel.is_empty() {
            self.grid.core.publish_catalog_handoff();
            cx.emit(EmotePickerEvent::PopoutRequested {
                channel,
                split_id: self.state.split_id,
            });
        }
        self.close(false, cx);
    }
}

impl Render for EmotePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        self.grid.window_scale = window.scale_factor();
        if !self.state.is_open {
            self.cleanup_after_close();
            return div().into_any_element();
        }
        if self.closed_state_cleared {
            image_cache::begin_picker_media_scope();
            self.closed_state_cleared = false;
            self.reload_open_picker();
        }

        let animation_mode = crate::backend::settings::get_emote_picker_animation_mode();
        self.grid.set_animation_mode(animation_mode);

        if !self.open_state_notified {
            self.open_state_notified = true;
            // Opening is routed through app state; publish it once to the escape-stack observer.
            cx.notify();
        }
        grid::ensure_search_input(self, window, cx);
        self.grid.focus_order.begin();
        if let Some(input) = &self.grid.search_input {
            self.grid
                .focus_order
                .include(input.read(cx).focus_handle().clone());
        }
        if self.grid.grid_focus.is_none() {
            self.grid.grid_focus = Some(cx.focus_handle());
        }
        self.update_layout_hints(window);
        if self.grid.pending_grid_focus {
            self.grid.pending_grid_focus = false;
            if let Some(focus) = &self.grid.grid_focus {
                window.focus(focus, cx);
            }
        }

        let now = Instant::now();
        if self.grid.poll_emote_catalog(now) {
            self.log_catalog_update();
        }

        grid::repaint_for_ready_images(self, now, cx);

        if INLINE_SIGNALS.take_pending() {
            grid::apply_pending_row_actions(self, cx);
        }
        if !self.state.is_open {
            return div().into_any_element();
        }

        self.grid.publish_and_prefetch_viewport();
        self.ensure_animation_waker(cx);

        let viewport_size = window.viewport_size();
        let viewport_width: f32 = viewport_size.width.into();
        let viewport_height: f32 = viewport_size.height.into();
        let picker_fit = crate::utils::fit_overlay_to_viewport(
            viewport_size,
            self.picker_width,
            EMOTE_PICKER_HEIGHT,
            8.0,
        );
        let picker_width = picker_fit.width;
        let picker_height = picker_fit.height;
        let (preferred_x, preferred_y) = if let Some(composer) = self.composer_bounds {
            let above = composer.origin.y.as_f32() - picker_height - 8.0;
            let below = composer.bottom().as_f32() + 8.0;
            let fits_above = above >= picker_fit.margin_y;
            let fits_below = below + picker_height <= viewport_height - picker_fit.margin_y;
            let y = if fits_above
                || (!fits_below
                    && composer.origin.y.as_f32() >= viewport_height - composer.bottom().as_f32())
            {
                above
            } else {
                below
            };
            (composer.right().as_f32() - picker_width, y)
        } else {
            (self.state.x, self.state.y)
        };
        let clamped_x = crate::utils::clamp_overlay_axis(
            preferred_x,
            picker_width,
            viewport_width,
            picker_fit.margin_x,
        );
        let clamped_y = crate::utils::clamp_overlay_axis(
            preferred_y,
            picker_height,
            viewport_height,
            picker_fit.margin_y,
        );

        let picker = div()
            .id("emote-picker")
            .absolute()
            .left(px(clamped_x))
            .top(px(clamped_y))
            .w(px(picker_width))
            .h(px(picker_height))
            .bg(colors::bg_elevated())
            .rounded(Radii::lg())
            .border_1()
            .border_color(colors::border())
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .key_context("EmotePicker")
            .on_mouse_down_out(cx.listener(|_, event: &MouseDownEvent, _, cx| {
                cx.emit(EmotePickerEvent::PointerPressedOutside(event.position));
            }))
            .block_overlay_pointer_passthrough()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::input::Escape, window, cx| {
                    grid::capture_escape(this, window, cx);
                },
            ))
            .capture_action(cx.listener(
                |this, action: &crate::components::ui::input::Enter, _, cx| {
                    if this.grid.enter_down {
                        cx.stop_propagation();
                    } else {
                        this.grid.enter_down = true;
                        this.search_enter_keep_open = action.secondary;
                    }
                },
            ))
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::FocusNext, window, cx| {
                    grid::flush_search_for_navigation(this, cx);
                    this.grid.focus_order.cycle(false, window, cx);
                    cx.stop_propagation();
                },
            ))
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::FocusPrev, window, cx| {
                    grid::flush_search_for_navigation(this, cx);
                    this.grid.focus_order.cycle(true, window, cx);
                    cx.stop_propagation();
                },
            ))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                if event.keystroke.key == "enter" {
                    this.grid.enter_down = false;
                }
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                grid::handle_grid_key(this, event, window, cx);
            }))
            .child(self.render_header(window, cx))
            .child(grid::render_tab_bar(self, cx))
            .child(rows::render_grid(self, cx))
            .when(self.grid.show_load_more_warning, |s| {
                s.child(render_load_more_warning(
                    "emote-picker-load-more-warning",
                    self.grid.hide_load_more_warning_next_time,
                    grid::cancel_load_more::<Self>,
                    grid::toggle_hide_load_more_warning::<Self>,
                    grid::confirm_load_more::<Self>,
                    &mut self.grid.focus_order,
                    cx,
                ))
            })
            .into_any_element();
        if self.grid.show_load_more_warning {
            self.grid.focus_order.ensure_focused(window, cx);
        }
        if self.pending_picker_focus {
            self.pending_picker_focus = false;
            self.grid.focus_order.ensure_focused(window, cx);
        }
        picker
    }
}

impl EmotePicker {
    /// Row range and hover filter that decide which cells animate right now.
    ///
    /// Shared by the wake cadence and the frame-signature gate so both read the
    /// exact set of cells the grid is rendering.
    fn visible_animation_rows(&self) -> Option<(&[VirtualRow], Option<usize>)> {
        if !crate::emote_renderer::animation_enabled()
            || picker_native_animated_enabled()
            || !self.state.is_open
        {
            return None;
        }

        let hovered_index = match self.grid.animation_mode {
            EmotePickerAnimationMode::Never => return None,
            EmotePickerAnimationMode::Always => None,
            EmotePickerAnimationMode::Hover => Some(INLINE_SIGNALS.hovered_emote()?),
        };

        let (mut start, mut end) = INLINE_SIGNALS.visible_range();
        if end <= start {
            start = 0;
            end = PICKER_INITIAL_PREFETCH_ROWS.min(self.grid.virtual_rows.len());
        }
        start = start.min(self.grid.virtual_rows.len());
        end = end.min(self.grid.virtual_rows.len());
        Some((&self.grid.virtual_rows[start..end], hovered_index))
    }

    /// Hash the animation frames the visible cells would display right now.
    ///
    /// `None` means the frame set cannot be proven stable — unresolved metadata
    /// or a non-resident frame — and must fall through to an unconditional wake
    /// so the render pass keeps driving decode/refill.
    fn visible_animation_frame_signature(&self) -> Option<u64> {
        let (rows, hovered_index) = self.visible_animation_rows()?;

        let compact_animations = super::prefetch::picker_uses_compact_animation_variant(
            rows.iter()
                .filter_map(|row| match row {
                    VirtualRow::EmoteRow { emotes, .. } => Some(emotes),
                    VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                })
                .flat_map(|emotes| emotes.iter())
                .filter(|(emote, _)| emote.is_animated)
                .count(),
            self.grid.animation_mode,
        );

        use std::hash::{Hash, Hasher};
        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut count = 0usize;

        for row in rows {
            let VirtualRow::EmoteRow { emotes, base_index } = row else {
                continue;
            };
            for (offset, (emote, _)) in emotes.iter().enumerate() {
                let emote_idx = base_index + offset;
                if !emote.is_animated || hovered_index.is_some_and(|index| index != emote_idx) {
                    continue;
                }
                let primary_url = picker_display_url(emote);
                let active_url = super::prefetch::picker_active_animation_url(
                    emote,
                    primary_url,
                    compact_animations,
                );
                let metadata = image_cache::get_animation_metadata(active_url)?;
                let frame_idx = metadata.frame_index_at(time_ms);
                if !image_cache::has_animated_frame(active_url, frame_idx) {
                    return None;
                }
                emote_idx.hash(&mut hasher);
                frame_idx.hash(&mut hasher);
                count += 1;
            }
        }

        (count > 0).then(|| hasher.finish())
    }

    fn visible_animation_interval_ms(&self) -> Option<u64> {
        let (rows, hovered_index) = self.visible_animation_rows()?;
        let compact_animations = super::prefetch::picker_uses_compact_animation_variant(
            rows.iter()
                .filter_map(|row| match row {
                    VirtualRow::EmoteRow { emotes, .. } => Some(emotes),
                    VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                })
                .flat_map(|emotes| emotes.iter())
                .filter(|(emote, _)| emote.is_animated)
                .count(),
            self.grid.animation_mode,
        );
        let frame_interval = |emote: &Arc<Emote>| {
            let primary_url = picker_display_url(emote);
            let active_url = super::prefetch::picker_active_animation_url(
                emote,
                primary_url,
                compact_animations,
            );
            emote
                .is_animated
                .then(|| image_cache::get_animation_metadata(active_url))
                .flatten()
                .map(|metadata| u64::from(metadata.min_frame_duration_ms))
                .filter(|duration_ms| *duration_ms > 0)
        };

        let visible_min = rows
            .iter()
            .filter_map(|row| match row {
                VirtualRow::Header(_) => None,
                VirtualRow::EmoteRow { emotes, base_index } => match hovered_index {
                    Some(index) => index
                        .checked_sub(*base_index)
                        .and_then(|offset| emotes.get(offset))
                        .and_then(|(emote, _)| frame_interval(emote)),
                    None => emotes
                        .iter()
                        .filter_map(|(emote, _)| frame_interval(emote))
                        .min(),
                },
                VirtualRow::LoadMore { .. } => None,
            })
            .min()?;

        Some(
            visible_min
                .max(crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms())
                .min(PICKER_ANIMATION_WAKE_MAX_MS),
        )
    }

    fn ensure_animation_waker(&mut self, cx: &mut Context<Self>) {
        if self.animation_waker_running || self.visible_animation_interval_ms().is_none() {
            return;
        }

        self.animation_waker_running = true;
        let generation = self.animation_waker_generation;
        cx.spawn(async move |this, cx| {
            loop {
                let interval_ms = this
                    .update(cx, |picker, _| {
                        if picker.animation_waker_generation != generation {
                            return 0;
                        }
                        if let Some(interval_ms) = picker.visible_animation_interval_ms() {
                            interval_ms
                        } else {
                            picker.animation_waker_running = false;
                            0
                        }
                    })
                    .unwrap_or(0);
                if interval_ms == 0 {
                    break;
                }

                cx.background_executor()
                    .timer(crate::managers::dynamic_tick_scheduler::aligned_tick_delay(
                        interval_ms,
                    ))
                    .await;

                let keep_running = this
                    .update(cx, |picker, cx| {
                        if picker.animation_waker_generation != generation {
                            return false;
                        }
                        if picker.visible_animation_interval_ms().is_none() {
                            picker.animation_waker_running = false;
                            return false;
                        }

                        match picker.visible_animation_frame_signature() {
                            Some(signature)
                                if picker.last_animation_frame_signature == Some(signature) =>
                            {
                                crate::backend::metrics::record_emote_event(
                                    "picker.animation_frame_unchanged_skipped",
                                );
                                return true;
                            }
                            signature => picker.last_animation_frame_signature = signature,
                        }

                        crate::backend::metrics::record_ui_notify_source("picker.animation_waker");
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn render_header(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let search_child = grid::render_search_field(self, window, cx);
        let clear_button = grid::render_clear_search_button(self, cx);
        let popout_focus = self.grid.focus_order.register("popout", cx);
        let close_focus = self.grid.focus_order.register("close", cx);
        div()
            .w_full()
            .px_2()
            .py_2()
            .bg(colors::bg_plate())
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .items_center()
            .gap_2()
            .child(search_child)
            .children(clear_button)
            .child(
                div()
                    .id("popout-btn")
                    .role(Role::Button)
                    .aria_label("Pop out emote picker")
                    .track_focus(&popout_focus)
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_picker_activate(cx.listener(|this, _, _window, cx| {
                        this.request_popout(cx);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child("↗"),
                    ),
            )
            .child(
                div()
                    .id("close-btn")
                    .track_focus(&close_focus)
                    .role(Role::Button)
                    .aria_label("Close emote picker")
                    .on_hover(crate::components::overlays::text_tooltip_on_hover(
                        "emote-picker-close-btn".to_string(),
                        "Close".to_string(),
                    ))
                    .w(px(28.0))
                    .h(px(28.0))
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_picker_activate(cx.listener(|this, _, _window, cx| {
                        this.close(true, cx);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors::text_secondary())
                            .child(crate::icons::CLOSE_GLYPH),
                    ),
            )
    }
}
