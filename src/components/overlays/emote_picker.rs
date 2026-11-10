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

use super::{clear_global_tooltip_if_source_in_window, show_text_tooltip, traits::Openable};
use crate::backend::Emote;
use crate::backend::lock_utils::MutexExt;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::components::ui::input::{Input, InputEvent, InputState, apply_standard_input_chrome};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::state::SplitId;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering as AtomicOrdering,
};
use std::time::{Duration, Instant};

pub use super::emote_picker_core::{
    EMOTE_PICKER_ROW_HEIGHT, EmotePickerCore, EmoteTab, LoadingState, calculate_emote_dimensions,
    pack_virtual_rows, picker_display_url, picker_native_animated_enabled, picker_page_emote_limit,
    picker_tooltip_text, provider_color,
};
use super::emote_picker_core::{PackedGridNavigation, emote_selection_identity_key};
use super::emote_picker_prefetch::{
    PICKER_ANIMATED_PREFETCH_BATCH_LIMIT, PICKER_INITIAL_PREFETCH_ROWS, PickerPrefetchPlanKind,
    build_prefetch_plan, execute_prefetch,
};
use super::emote_picker_shared::{
    PickerButtonExt, PickerFocusOrder, VirtualRow, pack_visible_range, picker_accessible_label,
    picker_cell_id, picker_control_row, publish_visible_emote_rows, render_load_more_warning,
};
use super::global_tooltip::clear_global_tooltip_if_source_prefix;

/// Maximum picker width in pixels.
pub const EMOTE_PICKER_MAX_WIDTH: f32 = 620.0;
pub const EMOTE_PICKER_HEIGHT: f32 = 480.0;
/// Minimum picker width in pixels for very small windows.
const PICKER_MIN_WIDTH: f32 = 360.0;
/// Approximate horizontal space not available to emote cells (padding + scrollbar).
const ROW_WIDTH_OVERHEAD: f32 = 24.0;
/// Minimum width used for row-packing safety.
const MIN_ROW_WIDTH: f32 = 120.0;
/// Single source key for global emote-picker cell tooltips.
const EMOTE_PICKER_TOOLTIP_SOURCE: &str = "emote-picker-cell";

/// Row height for virtualization (must be uniform)
const ROW_HEIGHT: f32 = EMOTE_PICKER_ROW_HEIGHT;

/// Search debounce delay in milliseconds
const SEARCH_DEBOUNCE_MS: u64 = 150;
/// Fast poll interval while picker metadata is actively changing.
const UPDATE_CHECK_INTERVAL_MIN_MS: u64 = 120;
/// Slow poll interval once picker metadata is stable.
const UPDATE_CHECK_INTERVAL_MAX_MS: u64 = 1200;
/// Keep policy/viewport changes responsive even when every visible animation has
/// unusually long frames. This matches the dynamic scheduler's safety ceiling.
const PICKER_ANIMATION_WAKE_MAX_MS: u64 = 100;

use std::sync::Mutex;

/// Counter for notifications - batched to avoid flooding
static PENDING_NOTIFICATIONS: AtomicU32 = AtomicU32::new(0);

/// Record that an image finished loading.
pub fn notify_image_loaded() {
    PENDING_NOTIFICATIONS.fetch_add(1, AtomicOrdering::Relaxed);
    crate::managers::frame_rate_controller::record_image_ready();
}

/// Record that a refill window landed for an already-registered animation.
///
/// Overlays still repaint, but steady playback is not activity: it must not
/// reset the idle clock that `notify_image_loaded` resets.
pub fn notify_animation_refilled() {
    PENDING_NOTIFICATIONS.fetch_add(1, AtomicOrdering::Relaxed);
}

/// Apply pending notifications and return if there are updates
pub(super) fn apply_pending_notifications() -> bool {
    let pending = PENDING_NOTIFICATIONS.swap(0, AtomicOrdering::SeqCst);
    pending > 0
}

/// Drain globally pending image-load notifications for overlay refreshes.
///
/// Chat canvas refreshes are split-local via visible URL signatures; callers
/// must not translate this global overlay signal into a root/app notify.
pub fn drain_image_load_notifications() -> bool {
    apply_pending_notifications()
}

/// Static state for click events from uniform_list (which can't use cx.listener).
///
/// Own the exact rendered emote instead of retaining a mutable flat-list index;
/// searches, provider publications, and category changes can all reorder that list
/// before the next picker render consumes the click.
static PENDING_FAVORITE_CLICK: Mutex<Option<Arc<Emote>>> = Mutex::new(None);
static PENDING_HEADER_CLICK: Mutex<Option<String>> = Mutex::new(None);
static PENDING_LOAD_MORE_CLICK: AtomicBool = AtomicBool::new(false);

/// Track the visible row range for viewport-based prefetching atomically.
///
/// A single packed value prevents a reader from observing a start/end pair
/// from two different `UniformList` callbacks.
static LAST_VISIBLE_RANGE: AtomicU64 = AtomicU64::new(0);
const NO_HOVERED_EMOTE: usize = usize::MAX;
static HOVERED_EMOTE_INDEX: AtomicUsize = AtomicUsize::new(NO_HOVERED_EMOTE);

/// Signal the visible row range (called from uniform_list closure)
fn signal_visible_range(start: usize, end: usize) {
    LAST_VISIBLE_RANGE.store(pack_visible_range(start, end), AtomicOrdering::Relaxed);
}

/// Get current visible range for prefetching
fn get_visible_range() -> (usize, usize) {
    let packed = LAST_VISIBLE_RANGE.load(AtomicOrdering::Relaxed);
    (((packed >> 32) as u32) as usize, (packed as u32) as usize)
}

fn reset_visible_range() {
    LAST_VISIBLE_RANGE.store(0, AtomicOrdering::Relaxed);
    HOVERED_EMOTE_INDEX.store(NO_HOVERED_EMOTE, AtomicOrdering::Relaxed);
    image_cache::clear_picker_visible_urls(image_cache::PickerViewport::Inline);
}

fn hovered_emote_index() -> Option<usize> {
    let index = HOVERED_EMOTE_INDEX.load(AtomicOrdering::Relaxed);
    (index != NO_HOVERED_EMOTE).then_some(index)
}

/// Update the cell whose animation is admitted in Hover mode.
///
/// A conditional clear prevents a delayed leave event from the previous cell
/// from stopping the animation of the newly hovered cell.
fn signal_hovered_emote(index: usize, is_animated: bool, hovered: bool) -> bool {
    if hovered {
        let next = if is_animated { index } else { NO_HOVERED_EMOTE };
        return HOVERED_EMOTE_INDEX.swap(next, AtomicOrdering::Relaxed) != next;
    }

    is_animated
        && HOVERED_EMOTE_INDEX
            .compare_exchange(
                index,
                NO_HOVERED_EMOTE,
                AtomicOrdering::Relaxed,
                AtomicOrdering::Relaxed,
            )
            .is_ok()
}

fn signal_favorite_click(emote: Arc<Emote>) {
    *PENDING_FAVORITE_CLICK.lock_or_recover() = Some(emote);
}

/// Signal that a header was clicked (called from uniform_list closure)
fn signal_header_click(header_id: String) {
    *PENDING_HEADER_CLICK.lock_or_recover() = Some(header_id);
}

fn signal_load_more_click() {
    PENDING_LOAD_MORE_CLICK.store(true, AtomicOrdering::SeqCst);
}

fn take_pending_favorite_click() -> Option<Arc<Emote>> {
    PENDING_FAVORITE_CLICK.lock_or_recover().take()
}

/// Check and consume pending header click
fn take_pending_header_click() -> Option<String> {
    PENDING_HEADER_CLICK.lock_or_recover().take()
}

fn take_pending_load_more_click() -> bool {
    PENDING_LOAD_MORE_CLICK.swap(false, AtomicOrdering::SeqCst)
}

fn clear_pending_picker_actions() {
    *PENDING_FAVORITE_CLICK.lock_or_recover() = None;
    *PENDING_HEADER_CLICK.lock_or_recover() = None;
    PENDING_LOAD_MORE_CLICK.store(false, AtomicOrdering::SeqCst);
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
    core: EmotePickerCore,
    on_select: Option<Arc<dyn Fn(Emote) + Send + Sync>>,
    selected_emote_index: usize,
    selected_emote_identity: Option<String>,
    /// Search input state (lazily initialized on first render)
    search_input: Option<Entity<InputState>>,
    /// Focus target for keyboard grid navigation after leaving search editing.
    grid_focus: Option<FocusHandle>,
    focus_order: PickerFocusOrder,
    search_enter_down: bool,
    search_enter_keep_open: bool,
    open_state_notified: bool,
    pending_grid_focus: bool,
    pending_picker_focus: bool,
    composer_bounds: Option<Bounds<Pixels>>,
    /// Invalidates delayed search applications after newer input/context changes.
    search_revision: u64,
    /// Subscriptions to keep alive
    _subscriptions: Vec<Subscription>,
    /// Virtualized rows for efficient rendering
    virtual_rows: Arc<Vec<VirtualRow>>,
    packed_navigation: PackedGridNavigation,
    /// Scroll handle for the uniform list
    scroll_handle: UniformListScrollHandle,
    /// Last prefetch time for throttling
    last_prefetch_time: Instant,
    /// Scale of the window the picker last rendered in, for prefetches
    /// started outside its render.
    window_scale: f32,
    /// Last refresh time (for throttling)
    last_refresh: Instant,
    /// Render version (incremented to force uniform_list refresh)
    render_version: u32,
    /// Last prefetched row range to avoid redundant prefetching
    last_prefetch_range: Option<(PickerPrefetchPlanKind, usize, usize, usize, usize)>,
    /// Last exact viewport published to cache residency protection.
    last_published_visible_range: Option<(u32, usize, usize, u64)>,
    /// Last time we polled for emote metadata updates.
    last_update_check: Instant,
    /// Adaptive poll interval for metadata checks.
    update_check_interval: Duration,
    /// Current picker width used for render/layout.
    picker_width: f32,
    /// Width hint used by row packing.
    row_width_hint: f32,
    /// Whether the picker-owned animation wake loop is running.
    animation_waker_running: bool,
    /// Invalidates a detached wake loop when the picker closes and reopens.
    animation_waker_generation: u64,
    /// Frames the visible cells displayed at the last wake-driven repaint.
    last_animation_frame_signature: Option<u64>,
    /// Saved policy controlling which picker cells actively advance animation.
    animation_mode: EmotePickerAnimationMode,
    /// Number of emotes currently allowed into the rendered grid.
    visible_emote_limit: usize,
    /// Number of emotes represented in the rendered grid.
    visible_emote_count: usize,
    /// Number of emotes available in the current tab/search result.
    total_available_emotes: usize,
    /// Whether the load-more resource warning is open.
    show_load_more_warning: bool,
    /// Local pending value for the "do not show again" toggle.
    hide_load_more_warning_next_time: bool,
    /// Whether heavyweight state has already been released for the current close.
    closed_state_cleared: bool,
    /// Selection routing is independent of mutable picker state until the app consumes it.
    pending_selections: VecDeque<PendingPickerEmote>,
    /// One-shot target for returning focus after an inline picker close.
    focus_return_split_id: Option<SplitId>,
}

impl EmotePicker {
    /// Create a new emote picker. Search input is lazily initialized on first render.
    pub fn new(state: EmotePickerState) -> Self {
        let mut core = EmotePickerCore::new(state.channel.clone());
        core.load_emotes();

        let mut picker = Self {
            state,
            core,
            on_select: None,
            selected_emote_index: 0,
            selected_emote_identity: None,
            search_input: None,
            grid_focus: None,
            focus_order: PickerFocusOrder::default(),
            search_enter_down: false,
            search_enter_keep_open: false,
            open_state_notified: false,
            pending_grid_focus: false,
            pending_picker_focus: false,
            composer_bounds: None,
            search_revision: 0,
            _subscriptions: Vec::new(),
            virtual_rows: Arc::new(Vec::new()),
            packed_navigation: PackedGridNavigation::default(),
            scroll_handle: UniformListScrollHandle::new(),
            last_prefetch_time: Instant::now(),
            window_scale: 0.0,
            last_refresh: Instant::now(),
            render_version: 0,
            last_prefetch_range: None,
            last_published_visible_range: None,
            last_update_check: Instant::now(),
            update_check_interval: Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS),
            picker_width: EMOTE_PICKER_MAX_WIDTH,
            row_width_hint: EMOTE_PICKER_MAX_WIDTH - ROW_WIDTH_OVERHEAD,
            animation_waker_running: false,
            animation_waker_generation: 0,
            last_animation_frame_signature: None,
            animation_mode: crate::backend::settings::get_emote_picker_animation_mode(),
            visible_emote_limit: picker_page_emote_limit(),
            visible_emote_count: 0,
            total_available_emotes: 0,
            show_load_more_warning: false,
            hide_load_more_warning_next_time:
                crate::backend::settings::get_hide_emote_picker_load_more_warning(),
            closed_state_cleared: false,
            pending_selections: VecDeque::new(),
            focus_return_split_id: None,
        };

        picker.rebuild_virtual_rows();

        if !picker.core.flat_emotes.is_empty() {
            picker.prefetch_emote_images();
        }
        picker
    }

    fn reset_visible_emote_limit(&mut self) {
        self.visible_emote_limit = picker_page_emote_limit();
        self.show_load_more_warning = false;
        self.hide_load_more_warning_next_time =
            crate::backend::settings::get_hide_emote_picker_load_more_warning();
    }

    fn request_load_more(&mut self, cx: &mut Context<Self>) {
        if crate::backend::settings::get_hide_emote_picker_load_more_warning() {
            self.confirm_load_more(cx);
            return;
        }
        self.hide_load_more_warning_next_time = false;
        self.show_load_more_warning = true;
        cx.notify();
    }

    fn confirm_load_more(&mut self, cx: &mut Context<Self>) {
        if self.hide_load_more_warning_next_time {
            crate::backend::settings::set_hide_emote_picker_load_more_warning(true);
        }
        self.visible_emote_limit = self.total_available_emotes.max(picker_page_emote_limit());
        self.show_load_more_warning = false;
        self.pending_grid_focus = true;
        self.rebuild_virtual_rows();
        self.prefetch_emote_images();
        cx.notify();
    }

    fn cancel_load_more(&mut self, cx: &mut Context<Self>) {
        self.show_load_more_warning = false;
        self.pending_grid_focus = true;
        cx.notify();
    }

    fn toggle_hide_load_more_warning(&mut self, cx: &mut Context<Self>) {
        self.hide_load_more_warning_next_time = !self.hide_load_more_warning_next_time;
        cx.notify();
    }

    fn clear_closed_picker_state(&mut self) {
        self.animation_waker_generation = self.animation_waker_generation.wrapping_add(1);
        self.animation_waker_running = false;
        self.last_animation_frame_signature = None;
        reset_visible_range();
        self.core.clear_heavy_caches();
        self.virtual_rows = Arc::new(Vec::new());
        self.packed_navigation = PackedGridNavigation::default();
        self.search_input = None;
        self.grid_focus = None;
        self.focus_order = PickerFocusOrder::default();
        self._subscriptions.clear();
        self.last_prefetch_range = None;
        self.selected_emote_index = 0;
        self.selected_emote_identity = None;
        self.visible_emote_count = 0;
        self.total_available_emotes = 0;
        self.show_load_more_warning = false;
        self.render_version = self.render_version.wrapping_add(1);
    }

    fn invalidate_search_tasks(&mut self) {
        self.search_revision = self.search_revision.wrapping_add(1);
    }

    fn cleanup_after_close(&mut self) {
        self.open_state_notified = false;
        self.search_enter_down = false;
        self.pending_grid_focus = false;
        self.pending_picker_focus = false;
        self.invalidate_search_tasks();
        clear_pending_picker_actions();
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
        self.core.load_emotes();
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        if !self.core.flat_emotes.is_empty() {
            self.prefetch_emote_images();
        }
    }

    fn sync_selected_identity(&mut self) {
        self.selected_emote_identity = self
            .core
            .flat_emotes
            .get(self.selected_emote_index)
            .filter(|_| self.selected_emote_index < self.visible_emote_count)
            .map(|emote| emote_selection_identity_key(emote));
    }

    fn scroll_selection_into_view(&self) {
        if let Some(row_index) = self
            .packed_navigation
            .virtual_row_for_index(self.selected_emote_index)
        {
            self.scroll_handle
                .scroll_to_item(row_index, ScrollStrategy::Nearest);
        }
    }

    fn reset_selection_to_first(&mut self) {
        self.selected_emote_index = 0;
        self.sync_selected_identity();
        self.scroll_selection_into_view();
    }

    fn apply_selection_move(&mut self, next: usize, cx: &mut Context<Self>) {
        if next != self.selected_emote_index {
            self.selected_emote_index = next;
            self.sync_selected_identity();
            self.scroll_selection_into_view();
            cx.notify();
        }
    }

    fn move_selection_vertical(&mut self, direction: isize, cx: &mut Context<Self>) {
        if let Some(next) = self
            .packed_navigation
            .vertical_neighbor(self.selected_emote_index, direction)
        {
            self.apply_selection_move(next, cx);
        }
    }

    fn move_selection_horizontal(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.visible_emote_count == 0 {
            return;
        }
        let next = self
            .selected_emote_index
            .saturating_add_signed(direction)
            .min(self.visible_emote_count - 1);
        self.apply_selection_move(next, cx);
    }

    /// Rebuild the virtualized row list from grid_items
    fn rebuild_virtual_rows(&mut self) {
        clear_global_tooltip_if_source_prefix(EMOTE_PICKER_TOOLTIP_SOURCE);
        reset_visible_range();
        self.last_prefetch_range = None;
        self.last_published_visible_range = None;
        let started_at = Instant::now();
        let previous_index = self.selected_emote_index;
        let selected_identity = self.selected_emote_identity.clone();
        let (rows, shown, total_available, packed_navigation) = pack_virtual_rows(
            &self.core.grid_items,
            &self.core.flat_emotes,
            self.visible_emote_limit,
            self.row_width_hint,
            |emote| self.core.is_favorite(emote),
            VirtualRow::Header,
            |emotes, base_index| VirtualRow::EmoteRow { emotes, base_index },
            |shown, total| VirtualRow::LoadMore { shown, total },
        );
        crate::backend::metrics::record_emote_stage(
            "picker.inline.rebuild_virtual_rows",
            started_at.elapsed(),
            rows.len(),
        );
        self.visible_emote_count = shown;
        self.total_available_emotes = total_available;
        if self.visible_emote_count == 0 {
            self.selected_emote_index = 0;
            self.selected_emote_identity = None;
        } else {
            self.selected_emote_index = selected_identity
                .as_deref()
                .and_then(|identity| {
                    self.core
                        .flat_emotes
                        .iter()
                        .take(self.visible_emote_count)
                        .position(|emote| emote_selection_identity_key(emote) == identity)
                })
                .unwrap_or_else(|| previous_index.min(self.visible_emote_count - 1));
        }
        self.virtual_rows = Arc::new(rows);
        self.packed_navigation = packed_navigation;
        self.sync_selected_identity();
        self.scroll_selection_into_view();
        self.render_version = self.render_version.wrapping_add(1);
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
        if (next_row_width - self.row_width_hint).abs() >= 8.0 {
            self.row_width_hint = next_row_width;
            self.rebuild_virtual_rows();
            self.prefetch_emote_images();
        }
    }

    fn schedule_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.invalidate_search_tasks();
        let revision = self.search_revision;
        let channel = self.state.channel.clone();

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;

            let _ = this.update(cx, move |picker, cx| {
                if picker.search_revision != revision
                    || !picker.state.is_open
                    || picker.state.channel != channel
                {
                    return;
                }

                if picker.core.search_query != query {
                    picker.core.set_search(query);
                    picker.reset_visible_emote_limit();
                    picker.rebuild_virtual_rows();
                    picker.reset_selection_to_first();
                }
                crate::backend::metrics::record_ui_notify_source("picker.search_debounce_apply");
                cx.notify();
            });
        })
        .detach();
    }

    fn flush_search_for_navigation(&mut self, cx: &mut Context<Self>) {
        let query = self
            .search_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.core.search_query.clone());
        self.invalidate_search_tasks();
        if self.core.search_query != query {
            self.core.set_search(query);
            self.reset_visible_emote_limit();
            self.rebuild_virtual_rows();
            self.reset_selection_to_first();
            crate::backend::metrics::record_ui_notify_source("picker.search_navigation_flush");
            cx.notify();
        }
    }

    /// Initialize the search input if not already done. Called during render when we have window access.
    fn ensure_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_input.is_none() {
            let search_input =
                cx.new(|cx| InputState::new(window, cx).with_placeholder("Search emotes…"));

            let subscription = cx.subscribe(
                &search_input,
                |this, _entity, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        if let Some(ref input) = this.search_input {
                            let text = input.read(cx).value().to_string();
                            this.schedule_search(text, cx);
                        }
                    }
                    InputEvent::PressEnter => {
                        this.flush_search_for_navigation(cx);
                        this.confirm_selection(this.search_enter_keep_open, cx);
                    }
                    InputEvent::Blur => this.search_enter_down = false,
                    InputEvent::Focus => {}
                },
            );

            let focus_handle = search_input.read(cx).focus_handle().clone();
            self.search_input = Some(search_input);
            self._subscriptions.push(subscription);
            window.focus(&focus_handle, cx);
        }
    }

    /// Prefetch emote images for visible viewport + buffer
    /// Uses unified cache for both static and animated emotes
    fn prefetch_emote_images(&mut self) {
        self.prefetch_visible_range(
            0,
            PICKER_INITIAL_PREFETCH_ROWS,
            0,
            PICKER_INITIAL_PREFETCH_ROWS,
            PickerPrefetchPlanKind::Initial,
        );
    }

    fn publish_visible_emotes(&mut self, start_row: usize, end_row: usize) {
        let start_row = start_row.min(self.virtual_rows.len());
        let end_row = end_row.min(self.virtual_rows.len());
        let animation_state_revision = image_cache::animated_cache_state_revision();
        let key = (
            self.render_version,
            start_row,
            end_row,
            animation_state_revision,
        );
        if self.last_published_visible_range == Some(key) {
            return;
        }

        let published = publish_visible_emote_rows(
            self.virtual_rows.as_slice(),
            start_row,
            end_row,
            self.animation_mode,
            image_cache::PickerViewport::Inline,
        );
        self.last_published_visible_range = published.then_some(key);
    }

    /// Prefetch emote images for a specific row range
    /// This is called on scroll and initial load for viewport-based loading
    fn prefetch_visible_range(
        &mut self,
        start_row: usize,
        end_row: usize,
        animated_start_row: usize,
        animated_end_row: usize,
        kind: PickerPrefetchPlanKind,
    ) {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(self.window_scale);
        let started_at = Instant::now();
        let end_row = end_row.min(self.virtual_rows.len());
        let animated_start_row = animated_start_row.min(self.virtual_rows.len());
        let animated_end_row = animated_end_row.min(self.virtual_rows.len());
        let range_key = (
            kind,
            start_row,
            end_row,
            animated_start_row,
            animated_end_row,
        );
        let same_range = Some(range_key) == self.last_prefetch_range;

        let now = Instant::now();
        if same_range && now.duration_since(self.last_prefetch_time) < Duration::from_millis(700) {
            return;
        }

        self.last_prefetch_range = Some(range_key);
        self.last_prefetch_time = now;

        crate::log_emotes!(
            debug,
            "prefetch_{}_range: rows {}..{} of {} total",
            kind.label(),
            start_row,
            end_row,
            self.virtual_rows.len()
        );

        let mut visible_emotes: Vec<Arc<Emote>> = Vec::with_capacity(200);

        for row_idx in start_row..end_row {
            if let Some(VirtualRow::EmoteRow { emotes, .. }) = self.virtual_rows.get(row_idx) {
                for (emote, _) in emotes {
                    visible_emotes.push(Arc::clone(emote));
                }
            }
        }

        let background_animation_mode = if self.animation_mode == EmotePickerAnimationMode::Always {
            EmotePickerAnimationMode::Never
        } else {
            self.animation_mode
        };
        let mut plan = build_prefetch_plan(visible_emotes.iter(), background_animation_mode);
        if self.animation_mode == EmotePickerAnimationMode::Always {
            let exact_visible_emotes = self.virtual_rows[animated_start_row..animated_end_row]
                .iter()
                .filter_map(|row| match row {
                    VirtualRow::EmoteRow { emotes, .. } => Some(emotes.iter()),
                    VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                })
                .flatten()
                .map(|(emote, _)| emote);
            plan.animated_requests =
                build_prefetch_plan(exact_visible_emotes, self.animation_mode).animated_requests;
        }
        let static_count = plan.static_requests.len();
        let poster_count = plan.poster_requests.len();
        let animated_count = plan.animated_requests.len();
        let plan_metric = match kind {
            PickerPrefetchPlanKind::Initial => "picker.inline.prefetch.initial_plan",
            PickerPrefetchPlanKind::Viewport => "picker.inline.prefetch.viewport_plan",
        };

        if static_count == 0 && poster_count == 0 && animated_count == 0 {
            crate::backend::metrics::record_emote_stage(plan_metric, started_at.elapsed(), 0);
            crate::log_emotes!(
                debug,
                "prefetch_visible_range: nothing to prefetch (all cached)"
            );
            return;
        }

        crate::log_emotes!(
            debug,
            "Prefetching {} emotes: static={}, posters={}, animated={}",
            kind.label(),
            static_count,
            poster_count,
            animated_count
        );
        crate::backend::metrics::record_emote_stage(
            plan_metric,
            started_at.elapsed(),
            static_count + poster_count + animated_count,
        );

        execute_prefetch(plan, PICKER_ANIMATED_PREFETCH_BATCH_LIMIT);
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
            self.invalidate_search_tasks();
            clear_pending_picker_actions();
            clear_global_tooltip_if_source_prefix(EMOTE_PICKER_TOOLTIP_SOURCE);
        }

        if self.state.channel.is_empty() {
            return;
        }

        if channel_changed {
            self.core.set_channel(self.state.channel.clone());
            self.reset_visible_emote_limit();
            self.rebuild_virtual_rows();
            self.reset_selection_to_first();
            self.prefetch_emote_images();
        } else if !was_open && will_open && self.core.categories.is_empty() {
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
        let updated = self.core.check_for_emote_updates();
        if updated {
            crate::log_emotes!(
                debug,
                "Emote picker detected update: flat_emotes={}, grid_items={}",
                self.core.flat_emotes.len(),
                self.core.grid_items.len()
            );
            self.rebuild_virtual_rows();
            self.prefetch_emote_images();
        }
        updated
    }

    fn select_tab(&mut self, tab: EmoteTab, cx: &mut Context<Self>) {
        self.core.select_tab(tab);
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        cx.notify();
    }

    fn toggle_category_collapsed(&mut self, category_id: &str, cx: &mut Context<Self>) {
        self.core.toggle_category(category_id);
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        if let Some(row) = self
            .virtual_rows
            .iter()
            .position(|row| matches!(row, VirtualRow::Header(header) if header.id == category_id))
        {
            self.scroll_handle
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Clear the search input and reset search state
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref search_input) = self.search_input {
            search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.core.set_search(String::new());
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.invalidate_search_tasks();
        if let Some(input) = &self.search_input {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn confirm_selection(&mut self, keep_open: bool, cx: &mut Context<Self>) {
        if self.selected_emote_index >= self.visible_emote_count {
            return;
        }
        if let Some(emote) = self
            .core
            .flat_emotes
            .get(self.selected_emote_index)
            .cloned()
        {
            self.confirm_emote(emote, keep_open, cx);
        }
    }

    fn confirm_emote(&mut self, emote: Arc<Emote>, keep_open: bool, cx: &mut Context<Self>) {
        self.core.record_usage(&emote);
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

    fn toggle_favorite_at_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.visible_emote_count {
            return;
        }
        if let Some(emote) = self.core.flat_emotes.get(index).cloned() {
            self.toggle_favorite(emote, cx);
        }
    }

    fn toggle_favorite(&mut self, emote: Arc<Emote>, cx: &mut Context<Self>) {
        self.core.toggle_favorite(&emote);
        self.rebuild_virtual_rows();
        cx.notify();
    }

    fn request_popout(&mut self, cx: &mut Context<Self>) {
        let channel = self.state.channel.clone();
        if !channel.is_empty() {
            self.core.publish_catalog_handoff();
            cx.emit(EmotePickerEvent::PopoutRequested {
                channel,
                split_id: self.state.split_id,
            });
        }
        self.close(false, cx);
    }

    fn handle_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_load_more_warning {
            self.cancel_load_more(cx);
        } else if self
            .search_input
            .as_ref()
            .is_some_and(|input| !input.read(cx).value().is_empty())
            || !self.core.search_query.is_empty()
        {
            self.clear_search(window, cx);
        } else {
            self.close(true, cx);
        }
    }

    fn handle_grid_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "enter" {
            self.search_enter_down = true;
        }
        let search_focused = self
            .search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window));
        let grid_focused = self
            .grid_focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window));
        let control = event.keystroke.modifiers.control;
        let key = event.keystroke.key.as_str();
        if self.show_load_more_warning && !matches!(key, "tab" | "escape") {
            return;
        }
        let handled = match key {
            "up" | "down" if grid_focused || search_focused => {
                self.flush_search_for_navigation(cx);
                self.move_selection_vertical(if key == "up" { -1 } else { 1 }, cx);
                true
            }
            "left" | "right" if grid_focused => {
                self.move_selection_horizontal(if key == "left" { -1 } else { 1 }, cx);
                true
            }
            "home" | "end" if grid_focused => {
                let next = if control {
                    (self.visible_emote_count > 0).then_some(if key == "end" {
                        self.visible_emote_count.saturating_sub(1)
                    } else {
                        0
                    })
                } else {
                    self.packed_navigation
                        .row_edge(self.selected_emote_index, key == "end")
                };
                if let Some(next) = next {
                    self.apply_selection_move(next, cx);
                }
                true
            }
            "pageup" | "pagedown" if grid_focused => {
                let (start, end) = get_visible_range();
                if let Some(next) = self.packed_navigation.page_neighbor(
                    self.selected_emote_index,
                    if key == "pageup" { -1 } else { 1 },
                    end.saturating_sub(start).max(1),
                ) {
                    self.apply_selection_move(next, cx);
                }
                true
            }
            "tab" if control => {
                let tabs = EmoteTab::inline_tabs();
                let current = tabs
                    .iter()
                    .position(|&tab| tab == self.core.active_tab)
                    .unwrap_or(0);
                let next = if event.keystroke.modifiers.shift {
                    current.checked_sub(1).unwrap_or(tabs.len() - 1)
                } else {
                    (current + 1) % tabs.len()
                };
                self.select_tab(tabs[next], cx);
                true
            }
            "tab" => {
                self.flush_search_for_navigation(cx);
                self.focus_order
                    .cycle(event.keystroke.modifiers.shift, window, cx);
                true
            }
            "enter" | "space" if grid_focused => {
                if !event.is_held {
                    self.flush_search_for_navigation(cx);
                    self.confirm_selection(event.keystroke.modifiers.shift, cx);
                }
                true
            }
            "escape" => {
                self.handle_escape(window, cx);
                true
            }
            "f" if control && (grid_focused || search_focused) => {
                self.flush_search_for_navigation(cx);
                self.toggle_favorite_at_index(self.selected_emote_index, cx);
                true
            }
            key if control => key
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| EmoteTab::inline_tabs().get(index).copied())
                .map(|tab| {
                    self.select_tab(tab, cx);
                    true
                })
                .unwrap_or(false),
            _ if grid_focused
                && !control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|text| !text.chars().any(char::is_control))
                    && let Some(input) = self.search_input.clone()
                {
                    let focus = input.read(cx).focus_handle().clone();
                    window.focus(&focus, cx);
                    input.update(cx, |input, cx| input.insert(text, window, cx));
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }
}

impl Render for EmotePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        self.window_scale = window.scale_factor();
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
        if animation_mode != self.animation_mode {
            self.animation_mode = animation_mode;
            HOVERED_EMOTE_INDEX.store(NO_HOVERED_EMOTE, AtomicOrdering::Relaxed);
            self.last_prefetch_range = None;
            self.last_published_visible_range = None;
        }

        if !self.open_state_notified {
            self.open_state_notified = true;
            // Opening is routed through app state; publish it once to the escape-stack observer.
            cx.notify();
        }
        self.ensure_search_input(window, cx);
        self.focus_order.begin();
        if let Some(input) = &self.search_input {
            self.focus_order
                .include(input.read(cx).focus_handle().clone());
        }
        if self.grid_focus.is_none() {
            self.grid_focus = Some(cx.focus_handle());
        }
        self.update_layout_hints(window);
        if self.pending_grid_focus {
            self.pending_grid_focus = false;
            if let Some(focus) = &self.grid_focus {
                window.focus(focus, cx);
            }
        }

        let now = Instant::now();
        if now.duration_since(self.last_update_check) >= self.update_check_interval {
            self.last_update_check = now;
            if self.check_for_emote_updates() {
                self.update_check_interval = Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS);
            } else {
                let next_ms = (self.update_check_interval.as_millis() as u64 * 2)
                    .min(UPDATE_CHECK_INTERVAL_MAX_MS);
                self.update_check_interval = Duration::from_millis(next_ms);
            }
        }

        if now.duration_since(self.last_refresh) >= Duration::from_millis(100)
            && apply_pending_notifications()
        {
            self.last_refresh = now;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }

        if let Some(emote) = take_pending_favorite_click() {
            self.toggle_favorite(emote, cx);
        }
        if let Some(header_id) = take_pending_header_click() {
            self.toggle_category_collapsed(&header_id, cx);
        }
        if take_pending_load_more_click() {
            self.request_load_more(cx);
        }
        if !self.state.is_open {
            return div().into_any_element();
        }

        {
            let (start, end) = get_visible_range();
            self.publish_visible_emotes(start, end);
            if end > start {
                const BUFFER_ROWS: usize = 6;
                let buffered_start = start.saturating_sub(BUFFER_ROWS);
                let buffered_end = (end + BUFFER_ROWS).min(self.virtual_rows.len());
                self.prefetch_visible_range(
                    buffered_start,
                    buffered_end,
                    start,
                    end,
                    PickerPrefetchPlanKind::Viewport,
                );
            }
        }
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
            .occlude()
            .on_mouse_down_out(cx.listener(|_, event: &MouseDownEvent, _, cx| {
                cx.emit(EmotePickerEvent::PointerPressedOutside(event.position));
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::input::Escape, window, cx| {
                    if this
                        .search_input
                        .as_ref()
                        .is_some_and(|input| input.read(cx).marked_range().is_some())
                    {
                        return;
                    }
                    this.handle_escape(window, cx);
                    cx.stop_propagation();
                },
            ))
            .capture_action(cx.listener(
                |this, action: &crate::components::ui::input::Enter, _, cx| {
                    if this.search_enter_down {
                        cx.stop_propagation();
                    } else {
                        this.search_enter_down = true;
                        this.search_enter_keep_open = action.secondary;
                    }
                },
            ))
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::FocusNext, window, cx| {
                    this.flush_search_for_navigation(cx);
                    this.focus_order.cycle(false, window, cx);
                    cx.stop_propagation();
                },
            ))
            .capture_action(cx.listener(
                |this, _: &crate::components::ui::FocusPrev, window, cx| {
                    this.flush_search_for_navigation(cx);
                    this.focus_order.cycle(true, window, cx);
                    cx.stop_propagation();
                },
            ))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                if event.keystroke.key == "enter" {
                    this.search_enter_down = false;
                }
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_grid_key(event, window, cx);
            }))
            .child(self.render_header(window, cx))
            .child(self.render_tab_bar(cx))
            .child(self.render_grid(cx))
            .when(self.show_load_more_warning, |s| {
                s.child(render_load_more_warning(
                    "emote-picker-load-more-warning",
                    self.hide_load_more_warning_next_time,
                    Self::cancel_load_more,
                    Self::toggle_hide_load_more_warning,
                    Self::confirm_load_more,
                    &mut self.focus_order,
                    cx,
                ))
            })
            .into_any_element();
        if self.show_load_more_warning {
            self.focus_order.ensure_focused(window, cx);
        }
        if self.pending_picker_focus {
            self.pending_picker_focus = false;
            self.focus_order.ensure_focused(window, cx);
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

        let hovered_index = match self.animation_mode {
            EmotePickerAnimationMode::Never => return None,
            EmotePickerAnimationMode::Always => None,
            EmotePickerAnimationMode::Hover => Some(hovered_emote_index()?),
        };

        let (mut start, mut end) = get_visible_range();
        if end <= start {
            start = 0;
            end = PICKER_INITIAL_PREFETCH_ROWS.min(self.virtual_rows.len());
        }
        start = start.min(self.virtual_rows.len());
        end = end.min(self.virtual_rows.len());
        Some((&self.virtual_rows[start..end], hovered_index))
    }

    /// Hash the animation frames the visible cells would display right now.
    ///
    /// `None` means the frame set cannot be proven stable — unresolved metadata
    /// or a non-resident frame — and must fall through to an unconditional wake
    /// so the render pass keeps driving decode/refill.
    fn visible_animation_frame_signature(&self) -> Option<u64> {
        let (rows, hovered_index) = self.visible_animation_rows()?;

        let compact_animations =
            super::emote_picker_prefetch::picker_uses_compact_animation_variant(
                rows.iter()
                    .filter_map(|row| match row {
                        VirtualRow::EmoteRow { emotes, .. } => Some(emotes),
                        VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                    })
                    .flat_map(|emotes| emotes.iter())
                    .filter(|(emote, _)| emote.is_animated)
                    .count(),
                self.animation_mode,
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
                let active_url = super::emote_picker_prefetch::picker_active_animation_url(
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
        let compact_animations =
            super::emote_picker_prefetch::picker_uses_compact_animation_variant(
                rows.iter()
                    .filter_map(|row| match row {
                        VirtualRow::EmoteRow { emotes, .. } => Some(emotes),
                        VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                    })
                    .flat_map(|emotes| emotes.iter())
                    .filter(|(emote, _)| emote.is_animated)
                    .count(),
                self.animation_mode,
            );
        let frame_interval = |emote: &Arc<Emote>| {
            let primary_url = picker_display_url(emote);
            let active_url = super::emote_picker_prefetch::picker_active_animation_url(
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
        let has_search = if let Some(ref search_input) = self.search_input {
            !search_input.read(cx).value().is_empty() || !self.core.search_query.is_empty()
        } else {
            !self.core.search_query.is_empty()
        };

        let search_child: AnyElement = if let Some(ref search_input) = self.search_input {
            let is_focused = search_input.read(cx).focus_handle().is_focused(window);
            let focus_handle = search_input.read(cx).focus_handle().clone();
            apply_standard_input_chrome(
                div()
                    .id("emote-picker-search-shell")
                    .flex_1()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        window.focus(&focus_handle, cx);
                    }),
                is_focused,
            )
            .child(div().text_sm().text_color(colors::text_muted()).child("⌕"))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(colors::text_primary())
                    .child(
                        Input::new(search_input)
                            .accessibility_label("Search emotes")
                            .accessibility_search(true)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
            .into_any_element()
        } else {
            apply_standard_input_chrome(
                div()
                    .id("emote-picker-search-shell-fallback")
                    .flex_1()
                    .h(px(40.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1(),
                false,
            )
            .child(div().text_sm().text_color(colors::text_muted()).child("⌕"))
            .child(
                div()
                    .text_sm()
                    .text_color(colors::text_muted())
                    .child("Search emotes…"),
            )
            .into_any_element()
        };

        let clear_focus = has_search.then(|| self.focus_order.register("clear-search", cx));
        let popout_focus = self.focus_order.register("popout", cx);
        let close_focus = self.focus_order.register("close", cx);
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
            .when(has_search, |el| {
                el.child(
                    div()
                        .id("emote-picker-clear-search-btn")
                        .track_focus(clear_focus.as_ref().expect("clear search focus"))
                        .role(Role::Button)
                        .aria_label("Clear search")
                        .on_hover(crate::components::overlays::text_tooltip_on_hover(
                            "emote-picker-clear-search-btn".to_string(),
                            "Clear search".to_string(),
                        ))
                        .w(px(28.0))
                        .h(px(28.0))
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_hover()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_picker_activate(cx.listener(|this, _, window, cx| {
                            this.clear_search(window, cx);
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .child(crate::icons::CLOSE_GLYPH),
                        ),
                )
            })
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

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tabs = EmoteTab::inline_tabs();
        let tab_focuses: Vec<_> = tabs
            .iter()
            .map(|tab| {
                self.focus_order
                    .register(format!("tab-{}", *tab as usize), cx)
            })
            .collect();

        div()
            .id("tab-bar")
            .role(Role::TabList)
            .aria_label("Emote categories")
            .aria_orientation(Orientation::Horizontal)
            .w_full()
            .h(px(40.0))
            .bg(colors::bg_surface())
            .border_b_1()
            .border_color(colors::border())
            .overflow_x_scroll()
            .overflow_y_hidden()
            .child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .children(tabs.iter().zip(tab_focuses.iter()).map(|(&tab, focus)| {
                        let is_selected = tab == self.core.active_tab;

                        div()
                            .id(("tab", tab as usize))
                            .role(Role::Tab)
                            .aria_label(tab.label())
                            .aria_selected(is_selected)
                            .track_focus(focus)
                            .px_3()
                            .py_1()
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .flex_shrink_0()
                            .when(is_selected, |s| {
                                s.bg(colors::bg_hover())
                                    .border_b_2()
                                    .border_color(colors::accent())
                            })
                            .hover(|s| s.bg(colors::bg_hover()))
                            .on_picker_activate(cx.listener(move |this, _, _window, cx| {
                                this.select_tab(tab, cx);
                            }))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if is_selected {
                                        colors::accent_text()
                                    } else {
                                        colors::text_muted()
                                    })
                                    .child(tab.icon()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .text_color(if is_selected {
                                        colors::text_primary()
                                    } else {
                                        colors::text_secondary()
                                    })
                                    .when(is_selected, |s| s.font_weight(FontWeight::SEMIBOLD))
                                    .child(tab.label()),
                            )
                    })),
            )
    }

    fn grid_container(&self) -> Stateful<Div> {
        div()
            .id("emote-grid")
            .role(Role::Grid)
            .aria_label(format!("Emotes, {}", self.core.active_tab.label()))
            .aria_row_count(self.virtual_rows.len())
            .track_focus(
                self.grid_focus
                    .as_ref()
                    .expect("emote picker grid focus initialized"),
            )
    }

    fn render_grid(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let selected_idx = self.selected_emote_index;
        if let Some(focus) = self.grid_focus.clone() {
            self.focus_order.include(focus);
        }

        let loading_state = self.core.get_loading_state();
        if loading_state == LoadingState::Loading {
            return self
                .grid_container()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_2xl()
                        .text_color(colors::accent_text())
                        .child("⏳")
                        .map(|s| {
                            if cx.reduce_motion() {
                                return s.into_any_element();
                            }
                            s.with_animation(
                                "spinner",
                                Animation::new(std::time::Duration::from_secs(1))
                                    .repeat()
                                    .with_easing(pulsating_between(0.6, 1.0)),
                                |s, delta| s.opacity(delta),
                            )
                            .into_any_element()
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child("Loading emotes…"),
                )
                .into_any_element();
        }

        if self.virtual_rows.is_empty() {
            let message = self.core.empty_state_message();

            return self
                .grid_container()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(div().text_xl().text_color(colors::text_muted()).child("😶"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors::text_muted())
                                .text_center()
                                .child(message),
                        ),
                )
                .into_any_element();
        }

        let virtual_rows = Arc::clone(&self.virtual_rows);
        let row_count = virtual_rows.len();
        let version = self.render_version;
        let animation_mode = self.animation_mode;
        let picker = cx.entity().downgrade();
        let channel = self.state.channel.clone();
        let row_focuses: Vec<_> = virtual_rows
            .iter()
            .enumerate()
            .map(|(row, item)| {
                let key = match item {
                    VirtualRow::Header(header) => format!("header-{}", header.id),
                    VirtualRow::LoadMore { .. } => "load-more".to_string(),
                    VirtualRow::EmoteRow { .. } => return None,
                };
                Some(
                    self.focus_order
                        .register_row(key, row, &self.scroll_handle, cx),
                )
            })
            .collect();

        self.grid_container()
            .flex_1()
            .p_2()
            .child(
                uniform_list(
                    ("emote-list", version),
                    row_count,
                    move |range: Range<usize>, window: &mut Window, _cx: &mut App| {
                        let _scale_scope =
                            crate::components::chat::canvas_renderer::layout_scale::enter(
                                window.scale_factor(),
                            );
                        if get_visible_range() != (range.start, range.end) {
                            publish_visible_emote_rows(
                                virtual_rows.as_slice(),
                                range.start,
                                range.end,
                                animation_mode,
                                image_cache::PickerViewport::Inline,
                            );
                        }
                        signal_visible_range(range.start, range.end);
                        let compact_animations =
                            super::emote_picker_prefetch::picker_uses_compact_animation_variant(
                                range
                                    .clone()
                                    .filter_map(|row_idx| virtual_rows.get(row_idx))
                                    .filter_map(|row| match row {
                                        VirtualRow::EmoteRow { emotes, .. } => Some(emotes),
                                        VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => None,
                                    })
                                    .flat_map(|emotes| emotes.iter())
                                    .filter(|(emote, _)| emote.is_animated)
                                    .count(),
                                animation_mode,
                            );
                        range
                            .map(|row_idx| {
                                let row = &virtual_rows[row_idx];
                                Self::render_virtual_row(
                                    row,
                                    row_idx,
                                    selected_idx,
                                    animation_mode,
                                    compact_animations,
                                    &picker,
                                    row_focuses[row_idx].as_ref(),
                                    version,
                                    &channel,
                                    window.is_a11y_active(),
                                )
                            })
                            .collect()
                    },
                )
                .flex_1()
                .h_full()
                .track_scroll(&self.scroll_handle),
            )
            .into_any_element()
    }

    /// Render a single virtualized row (header or emote row)
    #[allow(clippy::too_many_arguments)]
    fn render_virtual_row(
        row: &VirtualRow,
        row_idx: usize,
        selected_idx: usize,
        animation_mode: EmotePickerAnimationMode,
        compact_animations: bool,
        picker: &WeakEntity<Self>,
        row_focus: Option<&FocusHandle>,
        version: u32,
        channel: &str,
        a11y_active: bool,
    ) -> AnyElement {
        match row {
            VirtualRow::Header(header) => {
                let header_id = header.id.clone();
                let expand_id = header.id.clone();
                let collapse_id = header.id.clone();
                let collapsed = header.is_collapsed;
                let control = div()
                    .id(("header", row_idx))
                    .role(Role::Button)
                    .aria_label(header.name.clone())
                    .aria_expanded(!header.is_collapsed)
                    .on_a11y_action(AccessibleAction::Expand, move |_, window, _| {
                        if collapsed {
                            signal_header_click(expand_id.clone());
                            window.refresh();
                        }
                    })
                    .on_a11y_action(AccessibleAction::Collapse, move |_, window, _| {
                        if !collapsed {
                            signal_header_click(collapse_id.clone());
                            window.refresh();
                        }
                    })
                    .track_focus(row_focus.expect("category focus"))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .when(header.is_special, |s| s.bg(colors::bg_plate()))
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_picker_activate(move |_, window, _| {
                        signal_header_click(header_id.clone());
                        window.refresh();
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(if header.is_collapsed { "▶" } else { "▼" }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if let Some(ref p) = header.provider {
                                provider_color(Some(*p))
                            } else if header.is_special {
                                colors::accent()
                            } else {
                                colors::text_secondary()
                            })
                            .child(header.icon.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .child(header.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(header.count_str.clone()),
                    )
                    ;
                picker_control_row("emote-header-row", row_idx, control)
            }
            VirtualRow::LoadMore { shown, total } => {
                let remaining = total.saturating_sub(*shown);
                let control = div()
                    .id(("emote-load-more", row_idx))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(format!("Showing {} of {}", shown, total)),
                    )
                    .child(
                        div()
                            .id("emote-picker-load-more")
                            .role(Role::Button)
                            .aria_label("Load more emotes")
                            .track_focus(row_focus.expect("load more focus"))
                            .px_3()
                            .py_1()
                            .rounded(Radii::sm())
                            .border_1()
                            .border_color(colors::border())
                            .bg(colors::bg_surface())
                            .hover(|s| s.bg(colors::bg_hover()))
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .on_picker_activate(move |_, window, _| {
                                signal_load_more_click();
                                window.refresh();
                            })
                            .child(if remaining > 0 {
                                format!("Load more ({remaining})")
                            } else {
                                "Load more".to_string()
                            }),
                    )
                    ;
                picker_control_row("emote-load-more-row", row_idx, control)
            }
            VirtualRow::EmoteRow { emotes, base_index } => {
                div()
                    .id(("emote-row", row_idx))
                    .role(Role::Row)
                    .aria_row_index(row_idx + 1)
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(emotes.iter().enumerate().map(|(i, (emote, is_favorite))| {
                        let emote_idx = base_index + i;
                        let is_selected = emote_idx == selected_idx;
                        let emote_url = picker_display_url(emote).to_string();
                        let fallback_animation_url =
                            super::emote_picker_prefetch::picker_animation_fallback_url(
                                emote,
                                &emote_url,
                            )
                            .map(str::to_string);
                        let active_animation_url =
                            super::emote_picker_prefetch::picker_active_animation_url(
                                emote,
                                &emote_url,
                                compact_animations,
                            )
                            .to_string();
                        let active_media_identity =
                            crate::backend::types::media_source_id_for_url(
                                emote,
                                &active_animation_url,
                            );
                        let (cell_width, cell_height, img_width, img_height) =
                            calculate_emote_dimensions(emote);
                        let is_animated = emote.is_animated;
                        let should_animate = is_animated
                            && animation_mode
                                .animates_cell(hovered_emote_index() == Some(emote_idx));
                        let is_fav = *is_favorite;
                        let media_identity =
                            crate::backend::types::media_source_id_for_url(emote, &emote_url);
                        let tooltip_text = picker_tooltip_text(emote);
                        let tooltip_source = format!("{EMOTE_PICKER_TOOLTIP_SOURCE}:{version}:{emote_idx}");
                        let selection_emote = Arc::clone(emote);
                        let favorite_emote = Arc::clone(emote);
                        let accessible_emote = Arc::clone(emote);
                        let accessible_picker = picker.clone();
                        let selection_picker = picker.clone();
                        let focus_emote = Arc::clone(emote);
                        let focus_picker = picker.clone();

                        div()
                            .id(("emote", picker_cell_id(emote)))
                            .role(Role::GridCell)
                            .when(a11y_active, |cell| cell.aria_label(picker_accessible_label(emote, channel)))
                            .aria_column_index(i + 1)
                            .aria_selected(is_selected)
                            .when(is_selected, |cell| cell.aria_active_descendant())
                            .on_a11y_action(AccessibleAction::Click, move |_, _, app| {
                                accessible_picker.update(app, |picker, cx| {
                                    if picker.state.is_open {
                                        picker.confirm_emote(Arc::clone(&accessible_emote), false, cx);
                                    }
                                }).ok();
                            })
                            .on_a11y_action(AccessibleAction::Focus, move |_, window, app| {
                                let focus = focus_picker.update(app, |picker, cx| {
                                    let index = picker.core.flat_emotes.iter().take(picker.visible_emote_count)
                                        .position(|emote| Arc::ptr_eq(emote, &focus_emote))?;
                                    picker.apply_selection_move(index, cx);
                                    picker.grid_focus.clone()
                                }).ok().flatten();
                                if let Some(focus) = focus { window.focus(&focus, app); }
                            })
                            .w(px(cell_width))
                            .h(px(cell_height))
                            .rounded(Radii::sm())
                            .cursor_pointer()
                            .relative()
                            .when(is_selected, |s| {
                                s.bg(colors::bg_hover())
                                    .border_1()
                                    .border_color(colors::accent())
                            })
                            .hover(|s| s.bg(colors::bg_hover()))
                            .on_activate_without_tab_stop(move |event, _, app| {
                                selection_picker.update(app, |picker, cx| {
                                    if picker.state.is_open {
                                        picker.confirm_emote(Arc::clone(&selection_emote), event.modifiers().shift, cx);
                                    }
                                }).ok();
                            })
                            .on_right_click(move |_, _, _| {
                                signal_favorite_click(Arc::clone(&favorite_emote));
                            })
                            .on_hover({
                                let tooltip_text = tooltip_text.clone();
                                let picker = picker.clone();
                                move |hovered, window, app| {
                                    if animation_mode == EmotePickerAnimationMode::Hover
                                        && signal_hovered_emote(
                                            emote_idx,
                                            is_animated,
                                            *hovered,
                                        )
                                    {
                                        picker
                                            .update(app, |_, cx| {
                                                crate::backend::metrics::record_ui_notify_source(
                                                    "picker.hover_animation",
                                                );
                                                cx.notify();
                                            })
                                            .ok();
                                    }
                                    if *hovered {
                                        window.refresh();
                                        show_text_tooltip(
                                            window.window_handle(),
                                            tooltip_source.clone(),
                                            tooltip_text.clone(),
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &tooltip_source,
                                        );
                                    }
                                }
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .child({
                                let placeholder = || {
                                    div()
                                        .w(px(img_width))
                                        .h(px(img_height))
                                        .rounded(Radii::sm())
                                        .bg(colors::bg_hover())
                                        .text_xs()
                                        .text_color(colors::text_muted())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .overflow_hidden()
                                        .child(emote.name.to_string())
                                        .into_any_element()
                                };

                                if should_animate && picker_native_animated_enabled() {
                                    let stable_instance_id = {
                                        use std::hash::{Hash, Hasher};
                                        let mut hasher =
                                            std::collections::hash_map::DefaultHasher::new();
                                        emote_idx.hash(&mut hasher);
                                        emote_url.hash(&mut hasher);
                                        hasher.finish()
                                    };
                                    img(emote_url.clone())
                                        .id(("animated-emote", stable_instance_id))
                                        .w(px(img_width))
                                        .h(px(img_height))
                                        .object_fit(ObjectFit::Contain)
                                        .into_any_element()
                                } else if should_animate {
                                    match super::emote_picker_prefetch::picker_current_animated_frame(
                                        &emote_url,
                                        fallback_animation_url.as_deref(),
                                        &active_animation_url,
                                    ) {
                                        Some((render_image, _, _, _)) => img(render_image)
                                            .w(px(img_width))
                                            .h(px(img_height))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                        _ => {
                                            if animation_mode == EmotePickerAnimationMode::Hover {
                                                image_cache::trigger_animated_decode_media_with_source(
                                                    &active_animation_url,
                                                    active_media_identity.clone(),
                                                    image_cache::PreloadSource::PickerCell,
                                                );
                                            }
                                            match image_cache::get_animated_poster_render_image(
                                                &active_animation_url,
                                            )
                                            .or_else(|| {
                                                image_cache::get_stable_animated_render_image(
                                                    &active_animation_url,
                                                )
                                            })
                                            .or_else(|| {
                                                image_cache::get_cached_render_image(
                                                    &active_animation_url,
                                                )
                                            })
                                            .or_else(|| {
                                                image_cache::get_animated_poster_render_image(
                                                    &emote_url,
                                                )
                                            })
                                            .or_else(|| {
                                                image_cache::get_stable_animated_render_image(
                                                    &emote_url,
                                                )
                                            })
                                            .or_else(|| {
                                                image_cache::get_cached_render_image(&emote_url)
                                            }) {
                                                Some(render_image) => img(render_image)
                                                    .w(px(img_width))
                                                    .h(px(img_height))
                                                    .object_fit(ObjectFit::Contain)
                                                    .into_any_element(),
                                                _ => placeholder(),
                                            }
                                        }
                                    }
                                } else if is_animated {
                                    match image_cache::get_animated_poster_render_image(&emote_url)
                                        .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
                                    {
                                        Some(render_image) => img(render_image)
                                            .w(px(img_width))
                                            .h(px(img_height))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                        _ => {
                                            if !image_cache::is_downloading(&emote_url) {
                                                image_cache::trigger_animated_poster_preload_media_single_with_source(
                                                    &emote_url,
                                                    media_identity.clone(),
                                                    image_cache::PreloadSource::PickerCell,
                                                );
                                            }
                                            match image_cache::get_animated_poster_render_image(
                                                &emote_url,
                                            )
                                            .or_else(|| {
                                                image_cache::get_stable_animated_render_image(
                                                    &emote_url,
                                                )
                                            }) {
                                                Some(render_image) => img(render_image)
                                                    .w(px(img_width))
                                                    .h(px(img_height))
                                                    .object_fit(ObjectFit::Contain)
                                                    .into_any_element(),
                                                _ => placeholder(),
                                            }
                                        }
                                    }
                                } else {
                                    match image_cache::get_cached_render_image(&emote_url) {
                                        Some(render_image) => img(render_image)
                                            .w(px(img_width))
                                            .h(px(img_height))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                        _ => {
                                            if !image_cache::is_downloading(&emote_url) {
                                                image_cache::trigger_preload_media_single_with_source(
                                                    &emote_url,
                                                    media_identity.clone(),
                                                    image_cache::PreloadSource::PickerCell,
                                                );
                                            }
                                            placeholder()
                                        }
                                    }
                                }
                            })
                            .when(is_fav, |s| {
                                s.child(
                                    div()
                                        .absolute()
                                        .top(px(1.0))
                                        .right(px(1.0))
                                        .text_xs()
                                        .text_color(colors::warning())
                                        .child("♥"),
                                )
                            })
                    }))
                    .into_any_element()
            }
        }
    }
}
