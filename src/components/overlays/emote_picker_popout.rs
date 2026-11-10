//! Emote picker popout window
//!
//! A standalone OS window containing the emote picker, allowing users to browse
//! emotes while keeping the picker always visible.
//!
//! Features:
//! - Real search input with debouncing
//! - Collapsible category sections
//! - Emote tooltips on hover
//! - Provider filtering tabs

mod benchmark_driver;

use crate::backend::Emote;
use crate::backend::lock_utils::MutexExt;
use crate::backend::settings::EmotePickerAnimationMode;
use crate::cache::loader as image_cache;
use crate::components::ui::input::{
    Enter, Escape, Input, InputEvent, InputState, apply_standard_input_chrome,
};
use crate::components::ui::{ButtonExt, FocusNext, FocusPrev, PressToClick};
use crate::state::SplitId;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use benchmark_driver::BenchPickerSweepState;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::emote_picker_core::{
    EMOTE_PICKER_ROW_HEIGHT, EmotePickerCore, EmoteTab, LoadingState, calculate_emote_dimensions,
    clear_picker_catalog_handoff, pack_virtual_rows, picker_display_url,
    picker_native_animated_enabled, picker_page_emote_limit, picker_tooltip_text, provider_color,
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
use super::emote_picker_stability::{self, PickerCellVisualState};
use super::{
    clear_global_tooltip_if_source_in_window, ensure_global_tooltip_animation_waker,
    render_global_tooltip_overlay, show_text_tooltip,
};

/// Search debounce delay in milliseconds
const SEARCH_DEBOUNCE_MS: u64 = 150;
/// Virtualized row height for popout list.
const ROW_HEIGHT: f32 = EMOTE_PICKER_ROW_HEIGHT;
/// Approximate horizontal space not available to emote cells (padding + scrollbar).
const ROW_WIDTH_OVERHEAD: f32 = 24.0;
/// Minimum row width used for row packing safety.
const MIN_ROW_WIDTH: f32 = 120.0;
/// Poll interval for background emote metadata updates while popout is open.
const UPDATE_CHECK_INTERVAL_MIN_MS: u64 = 120;
const UPDATE_CHECK_INTERVAL_MAX_MS: u64 = 1200;
/// Keep close/reopen and viewport changes responsive even for unusually slow frames.
/// This matches the dynamic scheduler's 100 ms safety ceiling.
const PICKER_ANIMATION_WAKE_MAX_MS: u64 = 100;
const POPOUT_TOOLTIP_SOURCE: &str = "emote-picker-popout-cell";

static PICKER_TIMING_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("ECHO_PICKER_TIMING")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
});
static PICKER_TIMING_ROW_BUILD_NS: AtomicU64 = AtomicU64::new(0);
static PICKER_TIMING_ROW_BUILD_CALLBACKS: AtomicU64 = AtomicU64::new(0);

fn visible_emote_cell_count(rows: &[VirtualRow], start: usize, end: usize) -> usize {
    let start = start.min(rows.len());
    let end = end.min(rows.len());
    rows[start..end]
        .iter()
        .map(|row| match row {
            VirtualRow::EmoteRow { emotes, .. } => emotes.len(),
            VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => 0,
        })
        .sum()
}

fn visible_animated_emote_cell_count(rows: &[VirtualRow], start: usize, end: usize) -> usize {
    let start = start.min(rows.len());
    let end = end.min(rows.len());
    rows[start..end]
        .iter()
        .map(|row| match row {
            VirtualRow::EmoteRow { emotes, .. } => {
                emotes.iter().filter(|(emote, _)| emote.is_animated).count()
            }
            VirtualRow::Header(_) | VirtualRow::LoadMore { .. } => 0,
        })
        .sum()
}

/// Messages sent from popout to main window
#[derive(Clone, Debug)]
pub enum PopoutMessage {
    /// User selected an emote
    EmoteSelected {
        name: String,
        channel: String,
        split_id: Option<SplitId>,
    },
    /// Popout window was closed
    Closed,
}

/// Message queue for cross-window communication
static POPOUT_MESSAGES: LazyLock<Mutex<Vec<PopoutMessage>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(16)));

pub(crate) struct PopoutMessageEvents;

impl EventEmitter<()> for PopoutMessageEvents {}

struct PopoutMessageEventsGlobal {
    events: Entity<PopoutMessageEvents>,
}

impl Global for PopoutMessageEventsGlobal {}

pub(crate) fn popout_message_events(cx: &mut App) -> Entity<PopoutMessageEvents> {
    if let Some(global) = cx.try_global::<PopoutMessageEventsGlobal>() {
        return global.events.clone();
    }

    let events = cx.new(|_| PopoutMessageEvents);
    cx.set_global(PopoutMessageEventsGlobal {
        events: events.clone(),
    });
    events
}

/// Flag indicating if popout is currently open
static POPOUT_OPEN: AtomicBool = AtomicBool::new(false);
static POPOUT_HANDLE: LazyLock<Mutex<Option<AnyWindowHandle>>> = LazyLock::new(|| Mutex::new(None));
static POPOUT_GENERATION: AtomicU64 = AtomicU64::new(0);
static PENDING_POPOUT_CHANNEL: Mutex<Option<(String, Option<SplitId>)>> = Mutex::new(None);
static PENDING_POPOUT_FAVORITE_CLICK: Mutex<Option<Arc<Emote>>> = Mutex::new(None);
static PENDING_POPOUT_HEADER_CLICK: Mutex<Option<String>> = Mutex::new(None);
static PENDING_POPOUT_LOAD_MORE_CLICK: AtomicBool = AtomicBool::new(false);
/// Single dirty flag guarding the pending-signal mutexes above so an idle render
/// never has to lock any of them.
static PENDING_POPOUT_SIGNALS: AtomicBool = AtomicBool::new(false);
/// Advances whenever an application-level publication targets the popout, which
/// is how an animation-policy settings change reaches this window.
static POPOUT_SETTINGS_REVISION: AtomicU64 = AtomicU64::new(0);
/// Track the visible row range atomically so prefetch never reads a torn pair.
static POPOUT_VISIBLE_RANGE: AtomicU64 = AtomicU64::new(0);
const NO_HOVERED_POPOUT_EMOTE: usize = usize::MAX;
static HOVERED_POPOUT_EMOTE_INDEX: AtomicUsize = AtomicUsize::new(NO_HOVERED_POPOUT_EMOTE);

fn signal_popout_visible_range(start: usize, end: usize) {
    POPOUT_VISIBLE_RANGE.store(pack_visible_range(start, end), Ordering::Relaxed);
}

fn get_popout_visible_range() -> (usize, usize) {
    let packed = POPOUT_VISIBLE_RANGE.load(Ordering::Relaxed);
    (((packed >> 32) as u32) as usize, (packed as u32) as usize)
}

fn reset_popout_visible_range() {
    POPOUT_VISIBLE_RANGE.store(0, Ordering::Relaxed);
    HOVERED_POPOUT_EMOTE_INDEX.store(NO_HOVERED_POPOUT_EMOTE, Ordering::Relaxed);
    image_cache::clear_picker_visible_urls(image_cache::PickerViewport::Popout);
}

fn hovered_popout_emote_index() -> Option<usize> {
    let index = HOVERED_POPOUT_EMOTE_INDEX.load(Ordering::Relaxed);
    (index != NO_HOVERED_POPOUT_EMOTE).then_some(index)
}

fn signal_hovered_popout_emote(index: usize, is_animated: bool, hovered: bool) -> bool {
    if hovered {
        let next = if is_animated {
            index
        } else {
            NO_HOVERED_POPOUT_EMOTE
        };
        return HOVERED_POPOUT_EMOTE_INDEX.swap(next, Ordering::Relaxed) != next;
    }

    is_animated
        && HOVERED_POPOUT_EMOTE_INDEX
            .compare_exchange(
                index,
                NO_HOVERED_POPOUT_EMOTE,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
}

fn mark_pending_popout_signals() {
    PENDING_POPOUT_SIGNALS.store(true, Ordering::SeqCst);
}

fn take_pending_popout_signals() -> bool {
    PENDING_POPOUT_SIGNALS.swap(false, Ordering::SeqCst)
}

fn signal_popout_favorite_click(emote: Arc<Emote>) {
    *PENDING_POPOUT_FAVORITE_CLICK.lock_or_recover() = Some(emote);
    mark_pending_popout_signals();
}

fn signal_popout_header_click(header_id: String) {
    *PENDING_POPOUT_HEADER_CLICK.lock_or_recover() = Some(header_id);
    mark_pending_popout_signals();
}

fn signal_popout_load_more_click() {
    PENDING_POPOUT_LOAD_MORE_CLICK.store(true, Ordering::SeqCst);
    mark_pending_popout_signals();
}

fn take_pending_popout_favorite_click() -> Option<Arc<Emote>> {
    PENDING_POPOUT_FAVORITE_CLICK.lock_or_recover().take()
}

fn take_pending_popout_header_click() -> Option<String> {
    PENDING_POPOUT_HEADER_CLICK.lock_or_recover().take()
}

fn take_pending_popout_load_more_click() -> bool {
    PENDING_POPOUT_LOAD_MORE_CLICK.swap(false, Ordering::SeqCst)
}

fn clear_pending_popout_actions() {
    *PENDING_POPOUT_FAVORITE_CLICK.lock_or_recover() = None;
    *PENDING_POPOUT_HEADER_CLICK.lock_or_recover() = None;
    PENDING_POPOUT_LOAD_MORE_CLICK.store(false, Ordering::SeqCst);
    PENDING_POPOUT_SIGNALS.store(false, Ordering::SeqCst);
}

/// Inputs that can change which cells animate or which source they animate from.
type VisibleAnimationKey = (u32, usize, usize, usize, u64, EmotePickerAnimationMode);

/// One resolved visible animation, kept so wake ticks need no URL/cache lookups.
struct VisibleAnimationCell {
    cell_index: usize,
    active_url: String,
    metadata: Arc<crate::cache::frame_layer::AnimationMetadata>,
}

/// Send a message from the popout to the main window
pub fn send_to_main(msg: PopoutMessage) {
    POPOUT_MESSAGES.lock_or_recover().push(msg);
}

/// Drain all pending messages from the popout (non-blocking)
pub fn drain_popout_messages() -> Vec<PopoutMessage> {
    std::mem::take(&mut *POPOUT_MESSAGES.lock_or_recover())
        .into_iter()
        .filter(|message| !matches!(message, PopoutMessage::Closed))
        .collect()
}

/// Mark popout as closed (called by app when window handle becomes invalid)
pub fn mark_popout_closed() {
    emote_picker_stability::cancel_stationary_window();
    reset_popout_visible_range();
    if POPOUT_OPEN.swap(false, Ordering::SeqCst) {
        image_cache::close_picker_media_scope();
    }
    *POPOUT_HANDLE.lock_or_recover() = None;
    *PENDING_POPOUT_CHANNEL.lock_or_recover() = None;
    clear_pending_popout_actions();
    clear_picker_catalog_handoff();
}

/// Close the real picker window during the opt-in hermetic lifecycle benchmark.
/// Ordinary application code must continue to use the window's normal close path.
pub fn benchmark_close_popout(cx: &mut App) -> bool {
    if !crate::benchmark_ui_fixture::lifecycle_churn_enabled() {
        return false;
    }
    let handle = *POPOUT_HANDLE.lock_or_recover();
    let closed = handle
        .and_then(|handle| {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok()
        })
        .is_some();
    mark_popout_closed();
    closed
}

pub fn popout_is_open() -> bool {
    POPOUT_OPEN.load(Ordering::SeqCst)
}

/// Refresh the open picker popout from an application-level publication.
///
/// Copy the window handle out of the mutex before entering GPUI. Window updates can
/// synchronously run application code, so retaining the handle lock across `update`
/// would make close/reopen paths vulnerable to lock inversion.
pub fn refresh_open_emote_picker_popout(cx: &mut App) -> bool {
    POPOUT_SETTINGS_REVISION.fetch_add(1, Ordering::Relaxed);
    if !POPOUT_OPEN.load(Ordering::SeqCst) {
        return false;
    }

    let handle = *POPOUT_HANDLE.lock_or_recover();
    let Some(handle) = handle else {
        return false;
    };

    handle
        .update(cx, |_, window, _cx| {
            window.refresh();
        })
        .is_ok()
}

/// Open the emote picker popout window
pub fn open_emote_popout(channel: String, cx: &mut App) {
    open_emote_popout_for_split(channel, None, cx);
}

/// Open the emote picker popout for a specific composer split when one is known.
pub(crate) fn open_emote_popout_for_split(
    channel: String,
    split_id: Option<SplitId>,
    cx: &mut App,
) {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return;
    }

    if POPOUT_OPEN.load(Ordering::SeqCst) {
        *PENDING_POPOUT_CHANNEL.lock_or_recover() = Some((channel.clone(), split_id));
        mark_pending_popout_signals();
        let handle = *POPOUT_HANDLE.lock_or_recover();
        if let Some(handle) = handle
            && handle
                .update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    window.refresh();
                })
                .is_ok()
        {
            return;
        }

        mark_popout_closed();
    }

    if !POPOUT_OPEN.swap(true, Ordering::SeqCst) {
        image_cache::begin_picker_media_scope();
    }
    let generation = POPOUT_GENERATION
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);

    let fallback_size = crate::backend::settings::get_emote_picker_popout_window_size();
    let fallback_position = crate::backend::settings::get_emote_picker_popout_window_position();
    let (_, target_display_id) = crate::utils::popup_window_placement(
        fallback_position,
        size(px(fallback_size.0), px(fallback_size.1)),
        cx,
    );
    let display_uuid = target_display_id
        .and_then(|display_id| cx.find_display(display_id))
        .and_then(|display| display.uuid().ok())
        .map(|uuid| uuid.to_string());
    let ((width, height), position) = display_uuid
        .as_deref()
        .and_then(crate::backend::settings::get_emote_picker_popout_window_geometry_for_display)
        .unwrap_or((fallback_size, fallback_position));
    let (bounds, display_id) =
        crate::utils::popup_window_placement(position, size(px(width), px(height)), cx);

    let open_result = crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(format!("Emotes - {}", channel).into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Floating,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(px(320.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            *POPOUT_HANDLE.lock_or_recover() = Some(window.window_handle());
            cx.new(|_cx| {
                EmotePickerPopout::new(channel.clone(), split_id, display_uuid.clone(), generation)
            })
        },
    );
    if open_result.is_err() {
        mark_popout_closed();
    }
}

/// State for the emote picker popout
pub struct EmotePickerPopout {
    generation: u64,
    origin_split_id: Option<SplitId>,
    core: EmotePickerCore,
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
    search_input: Option<Entity<InputState>>,
    grid_focus: Option<FocusHandle>,
    focus_order: PickerFocusOrder,
    enter_pressed: bool,
    search_revision: u64,
    ignored_search_input_value: Option<String>,
    _subscriptions: Vec<Subscription>,
    virtual_rows: Arc<Vec<VirtualRow>>,
    packed_navigation: PackedGridNavigation,
    scroll_handle: UniformListScrollHandle,
    selected_emote_index: usize,
    selected_emote_identity: Option<String>,
    last_prefetch_time: Instant,
    /// Scale of the popout window, for prefetches started outside its render.
    window_scale: f32,
    last_prefetch_range: Option<(PickerPrefetchPlanKind, usize, usize, usize, usize)>,
    last_published_visible_range: Option<(u32, usize, usize, u64)>,
    last_update_check: Instant,
    update_check_interval: Duration,
    last_refresh: Instant,
    row_width_hint: f32,
    render_version: u32,
    animation_waker_running: bool,
    /// Invalidates a detached wake loop when the popout closes or its policy changes.
    animation_waker_generation: u64,
    animation_mode: EmotePickerAnimationMode,
    /// Settings revision the cached `animation_mode` was read at.
    settings_revision: u64,
    /// Channel the OS window title currently spells out.
    title_channel: String,
    visible_animation_key: Option<VisibleAnimationKey>,
    /// Missing/failed assets must not rebuild the entire visible metadata set
    /// at the fastest emote's frame rate. Retry without delaying viewport changes.
    visible_animation_retry: Option<(VisibleAnimationKey, Instant)>,
    visible_animation_cells: Vec<VisibleAnimationCell>,
    visible_animation_min_frame_ms: Option<u64>,
    last_animation_frame_signature: Option<u64>,
    visible_emote_limit: usize,
    visible_emote_count: usize,
    total_available_emotes: usize,
    show_load_more_warning: bool,
    hide_load_more_warning_next_time: bool,
    pending_grid_focus: bool,
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    last_persisted_display_uuid: Option<String>,
    last_display_resolution_key: Option<(f32, f32, f32, f32, f32)>,
    bench_sweep: Option<BenchPickerSweepState>,
    timing_last_report: Instant,
    timing_render_count: u64,
    timing_last_present_count: u64,
}

impl Drop for EmotePickerPopout {
    fn drop(&mut self) {
        self.cancel_animation_waker();
        self.invalidate_pending_search();
        if POPOUT_GENERATION.load(Ordering::SeqCst) == self.generation {
            super::global_tooltip::clear_global_tooltip_if_source_prefix(POPOUT_TOOLTIP_SOURCE);
            mark_popout_closed();
            send_to_main(PopoutMessage::Closed);
        }
    }
}

impl EmotePickerPopout {
    pub fn new(
        channel: String,
        origin_split_id: Option<SplitId>,
        display_uuid: Option<String>,
        generation: u64,
    ) -> Self {
        let mut core = EmotePickerCore::new(channel);
        core.load_emotes();
        let fallback_size = crate::backend::settings::get_emote_picker_popout_window_size();
        let fallback_position = crate::backend::settings::get_emote_picker_popout_window_position();
        let display_geometry = display_uuid.as_deref().and_then(
            crate::backend::settings::get_emote_picker_popout_window_geometry_for_display,
        );
        let (last_persisted_size, last_persisted_position) =
            display_geometry.unwrap_or((fallback_size, fallback_position));
        let last_persisted_display_uuid = display_geometry
            .is_some()
            .then(|| display_uuid.clone())
            .flatten();
        let bench_sweep = BenchPickerSweepState::from_env();
        let animation_mode = bench_sweep
            .as_ref()
            .and_then(|sweep| sweep.animation_mode_override)
            .unwrap_or_else(crate::backend::settings::get_emote_picker_animation_mode);

        let mut picker = Self {
            generation,
            origin_split_id,
            core,
            always_on_top: Default::default(),
            search_input: None,
            grid_focus: None,
            focus_order: PickerFocusOrder::default(),
            enter_pressed: false,
            search_revision: 0,
            ignored_search_input_value: None,
            _subscriptions: Vec::new(),
            virtual_rows: Arc::new(Vec::new()),
            packed_navigation: PackedGridNavigation::default(),
            scroll_handle: UniformListScrollHandle::new(),
            selected_emote_index: 0,
            selected_emote_identity: None,
            last_prefetch_time: Instant::now(),
            window_scale: 0.0,
            last_prefetch_range: None,
            last_published_visible_range: None,
            last_update_check: Instant::now(),
            update_check_interval: Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS),
            last_refresh: Instant::now(),
            row_width_hint: 420.0 - ROW_WIDTH_OVERHEAD,
            render_version: 0,
            animation_waker_running: false,
            animation_waker_generation: 0,
            animation_mode,
            settings_revision: POPOUT_SETTINGS_REVISION.load(Ordering::Relaxed),
            title_channel: String::new(),
            visible_animation_key: None,
            visible_animation_retry: None,
            visible_animation_cells: Vec::new(),
            visible_animation_min_frame_ms: None,
            last_animation_frame_signature: None,
            visible_emote_limit: picker_page_emote_limit(),
            visible_emote_count: 0,
            total_available_emotes: 0,
            show_load_more_warning: false,
            hide_load_more_warning_next_time:
                crate::backend::settings::get_hide_emote_picker_load_more_warning(),
            pending_grid_focus: false,
            last_persisted_position,
            last_persisted_size,
            last_persisted_display_uuid,
            last_display_resolution_key: None,
            bench_sweep,
            timing_last_report: Instant::now(),
            timing_render_count: 0,
            timing_last_present_count: 0,
        };
        picker.rebuild_virtual_rows();
        picker.prefetch_emotes();
        picker
    }

    fn apply_pending_channel_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_channel = PENDING_POPOUT_CHANNEL.lock_or_recover().take();

        let Some((channel, split_id)) = next_channel else {
            return;
        };

        self.origin_split_id = split_id;

        if channel == self.core.channel {
            clear_picker_catalog_handoff();
            return;
        }

        clear_pending_popout_actions();
        self.clear_search_input_value(window, cx);
        self.core.set_channel(channel);
        self.core.select_tab(EmoteTab::Channel);
        self.core.set_search(String::new());
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.prefetch_emotes();
        cx.notify();
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
        self.prefetch_emotes();
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

    fn cancel_animation_waker(&mut self) {
        self.animation_waker_generation = self.animation_waker_generation.wrapping_add(1);
        self.animation_waker_running = false;
        self.last_animation_frame_signature = None;
    }

    /// Resolve the visible animated cells when the inputs behind them changed.
    ///
    /// Complete sets are retained by key. Partial sets retry at the hydration
    /// cadence, since metadata registration has no revision of its own; they
    /// must neither stay cached forever nor rescan every cell on each frame.
    fn sync_visible_animation_cells(
        &mut self,
        start: usize,
        end: usize,
        hovered_index: Option<usize>,
    ) {
        let key = (
            self.render_version,
            start,
            end,
            hovered_index.unwrap_or(NO_HOVERED_POPOUT_EMOTE),
            image_cache::animated_cache_state_revision(),
            self.animation_mode,
        );
        if self.visible_animation_key == Some(key) {
            return;
        }
        if self
            .visible_animation_retry
            .is_some_and(|(retry_key, retry_at)| retry_key == key && Instant::now() < retry_at)
        {
            return;
        }

        let rows = Arc::clone(&self.virtual_rows);
        let start = start.min(rows.len());
        let end = end.min(rows.len());
        let compact_animations =
            super::emote_picker_prefetch::picker_uses_compact_animation_variant(
                visible_animated_emote_cell_count(rows.as_slice(), start, end),
                self.animation_mode,
            );

        let mut cells = Vec::with_capacity(self.visible_animation_cells.len());
        let mut visible_min_duration_ms: Option<u64> = None;
        let mut resolved = true;

        for row in &rows[start..end] {
            let VirtualRow::EmoteRow { emotes, base_index } = row else {
                continue;
            };
            for (offset, (emote, _)) in emotes.iter().enumerate() {
                let emote_idx = *base_index + offset;
                if !emote.is_animated || hovered_index.is_some_and(|index| index != emote_idx) {
                    continue;
                }
                let primary_url = picker_display_url(emote);
                let active_url = super::emote_picker_prefetch::picker_active_animation_url(
                    emote,
                    primary_url,
                    compact_animations,
                );
                match image_cache::get_animation_metadata(active_url) {
                    Some(metadata) => {
                        let duration_ms = u64::from(metadata.min_frame_duration_ms);
                        if duration_ms > 0 {
                            visible_min_duration_ms = Some(
                                visible_min_duration_ms
                                    .map_or(duration_ms, |current| current.min(duration_ms)),
                            );
                        }
                        cells.push(VisibleAnimationCell {
                            cell_index: emote_idx,
                            active_url: active_url.to_string(),
                            metadata,
                        });
                    }
                    _ => resolved = false,
                }
            }
        }

        self.visible_animation_cells = cells;
        self.visible_animation_min_frame_ms = visible_min_duration_ms;
        self.visible_animation_key = resolved.then_some(key);
        self.visible_animation_retry = (!resolved).then(|| {
            (
                key,
                Instant::now() + Duration::from_millis(PICKER_ANIMATION_WAKE_MAX_MS),
            )
        });
    }

    fn visible_animation_tick_delay(&mut self) -> Option<Duration> {
        if !crate::emote_renderer::animation_enabled() {
            return None;
        }
        if picker_native_animated_enabled() {
            return None;
        }

        let hovered_index = match self.animation_mode {
            EmotePickerAnimationMode::Never => return None,
            EmotePickerAnimationMode::Always => None,
            EmotePickerAnimationMode::Hover => Some(hovered_popout_emote_index()?),
        };

        let (start, end) = get_popout_visible_range();
        if end <= start {
            return None;
        }
        self.sync_visible_animation_cells(start, end, hovered_index);

        let interval_ms = self
            .visible_animation_min_frame_ms
            .or_else(|| {
                self.visible_animation_retry
                    .map(|_| PICKER_ANIMATION_WAKE_MAX_MS)
            })?
            .max(crate::managers::dynamic_tick_scheduler::animation_min_tick_interval_ms())
            .min(PICKER_ANIMATION_WAKE_MAX_MS);
        Some(crate::managers::dynamic_tick_scheduler::aligned_tick_delay(
            interval_ms,
        ))
    }

    /// Hash the animation frames the visible cells would display right now.
    ///
    /// `None` means a non-resident frame. An empty resolved subset has a stable
    /// signature, so the metadata retry timer can continue without repainting
    /// an unchanged loading state on every retry.
    fn visible_animation_frame_signature(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let time_ms = crate::cache::frame_layer::current_animation_time_ms();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for cell in &self.visible_animation_cells {
            let frame_idx = cell.metadata.frame_index_at(time_ms);
            if !image_cache::has_animated_frame(&cell.active_url, frame_idx) {
                return None;
            }
            cell.cell_index.hash(&mut hasher);
            frame_idx.hash(&mut hasher);
        }
        Some(hasher.finish())
    }

    fn ensure_animation_waker(&mut self, cx: &mut Context<Self>) {
        if self.animation_waker_running {
            return;
        }
        let Some(initial_delay) = self.visible_animation_tick_delay() else {
            return;
        };

        self.animation_waker_running = true;
        let generation = self.animation_waker_generation;
        cx.spawn(async move |this, cx| {
            let mut delay = initial_delay;
            loop {
                cx.background_executor().timer(delay).await;

                let next_delay = this
                    .update(cx, |picker, cx| {
                        if picker.animation_waker_generation != generation {
                            return None;
                        }
                        let Some(next_delay) = picker.visible_animation_tick_delay() else {
                            picker.animation_waker_running = false;
                            return None;
                        };

                        match picker.visible_animation_frame_signature() {
                            Some(signature)
                                if picker.last_animation_frame_signature == Some(signature) =>
                            {
                                crate::backend::metrics::record_emote_event(
                                    "picker.popout.animation_frame_unchanged_skipped",
                                );
                                return Some(next_delay);
                            }
                            signature => picker.last_animation_frame_signature = signature,
                        }

                        crate::backend::metrics::record_ui_notify_source(
                            "picker.popout.animation_waker",
                        );
                        cx.notify();
                        Some(next_delay)
                    })
                    .ok()
                    .flatten();

                let Some(next_delay) = next_delay else {
                    break;
                };
                delay = next_delay;
            }
        })
        .detach();
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

    fn rebuild_virtual_rows(&mut self) {
        super::global_tooltip::clear_global_tooltip_if_source_prefix(POPOUT_TOOLTIP_SOURCE);
        reset_popout_visible_range();
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
        let elapsed = started_at.elapsed();
        crate::backend::metrics::record_emote_stage(
            "picker.popout.rebuild_virtual_rows",
            elapsed,
            rows.len(),
        );
        if crate::log::enabled("perf") {
            crate::log_perf!(
                debug,
                stage = "picker.popout.rebuild_virtual_rows",
                elapsed_ms = elapsed.as_millis(),
                rows = rows.len(),
                row_width_hint = self.row_width_hint,
                "Popout virtual rows rebuilt"
            );
        }
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

    fn update_row_width_hint(&mut self, window: &Window) {
        let viewport_size = window.viewport_size();
        let window_width: f32 = viewport_size.width.into();
        let next_width = (window_width - ROW_WIDTH_OVERHEAD).max(MIN_ROW_WIDTH);
        if (next_width - self.row_width_hint).abs() >= 8.0 {
            self.row_width_hint = next_width;
            self.rebuild_virtual_rows();
            self.prefetch_emotes();
        }
    }

    fn select_emote(&mut self, emote: &Emote, cx: &mut Context<Self>) {
        // Input can arrive before an existing window paints its requested retarget.
        let split_id = match PENDING_POPOUT_CHANNEL.lock_or_recover().as_ref() {
            Some((channel, split_id)) if channel == &self.core.channel => *split_id,
            Some(_) => return,
            None => self.origin_split_id,
        };
        let identity = emote_selection_identity_key(emote);
        if let Some(index) = self
            .core
            .flat_emotes
            .iter()
            .take(self.visible_emote_count)
            .position(|candidate| emote_selection_identity_key(candidate) == identity)
        {
            self.selected_emote_index = index;
            self.sync_selected_identity();
            self.scroll_selection_into_view();
        }
        self.core.record_usage(emote);
        send_to_main(PopoutMessage::EmoteSelected {
            name: emote.name.to_string(),
            channel: self.core.channel.clone(),
            split_id,
        });
        popout_message_events(cx).update(cx, |_, cx| cx.emit(()));
    }

    fn confirm_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected_emote_index >= self.visible_emote_count {
            return;
        }
        if let Some(emote) = self
            .core
            .flat_emotes
            .get(self.selected_emote_index)
            .cloned()
        {
            self.select_emote(emote.as_ref(), cx);
            cx.notify();
        }
    }

    fn clear_search_or_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_load_more_warning {
            self.cancel_load_more(cx);
            return;
        }
        let has_search = self
            .search_input
            .as_ref()
            .is_some_and(|input| !input.read(cx).value().is_empty())
            || !self.core.search_query.is_empty();
        if has_search {
            self.clear_search(window, cx);
        } else {
            window.remove_window();
        }
    }

    fn handle_grid_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "enter" {
            self.enter_pressed = true;
        }
        let search_focused = self
            .search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle().is_focused(window));
        let grid_focused = self
            .grid_focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window));
        let visible_rows = {
            let (start, end) = get_popout_visible_range();
            end.saturating_sub(start).max(1)
        };

        if self.show_load_more_warning {
            if event.keystroke.key == "escape" {
                self.cancel_load_more(cx);
                cx.stop_propagation();
            }
            return;
        }

        let handled = match event.keystroke.key.as_str() {
            "up" if search_focused || grid_focused => {
                self.flush_search_for_navigation(cx);
                self.move_selection_vertical(-1, cx);
                true
            }
            "down" if search_focused || grid_focused => {
                self.flush_search_for_navigation(cx);
                self.move_selection_vertical(1, cx);
                true
            }
            "left" if grid_focused => {
                self.move_selection_horizontal(-1, cx);
                true
            }
            "right" if grid_focused => {
                self.move_selection_horizontal(1, cx);
                true
            }
            "home" if grid_focused && event.keystroke.modifiers.control => {
                self.apply_selection_move(0, cx);
                true
            }
            "end" if grid_focused && event.keystroke.modifiers.control => {
                if let Some(last) = self.visible_emote_count.checked_sub(1) {
                    self.apply_selection_move(last, cx);
                }
                true
            }
            "home" if grid_focused => self
                .packed_navigation
                .row_edge(self.selected_emote_index, false)
                .map(|index| {
                    self.apply_selection_move(index, cx);
                    true
                })
                .unwrap_or(false),
            "end" if grid_focused => self
                .packed_navigation
                .row_edge(self.selected_emote_index, true)
                .map(|index| {
                    self.apply_selection_move(index, cx);
                    true
                })
                .unwrap_or(false),
            "pageup" if grid_focused => self
                .packed_navigation
                .page_neighbor(self.selected_emote_index, -1, visible_rows)
                .map(|index| {
                    self.apply_selection_move(index, cx);
                    true
                })
                .unwrap_or(false),
            "pagedown" if grid_focused => self
                .packed_navigation
                .page_neighbor(self.selected_emote_index, 1, visible_rows)
                .map(|index| {
                    self.apply_selection_move(index, cx);
                    true
                })
                .unwrap_or(false),
            "tab" if event.keystroke.modifiers.control => {
                let tabs = EmoteTab::popout_tabs();
                let current = tabs
                    .iter()
                    .position(|tab| *tab == self.core.active_tab)
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
                    self.confirm_selection(cx);
                }
                true
            }
            "escape"
                if !(search_focused
                    && self
                        .search_input
                        .as_ref()
                        .is_some_and(|input| input.read(cx).marked_range().is_some())) =>
            {
                self.clear_search_or_close(window, cx);
                true
            }
            "f" if (grid_focused || search_focused) && event.keystroke.modifiers.control => {
                self.flush_search_for_navigation(cx);
                if let Some(emote) = self
                    .core
                    .flat_emotes
                    .get(self.selected_emote_index)
                    .filter(|_| self.selected_emote_index < self.visible_emote_count)
                    .cloned()
                {
                    self.toggle_favorite(emote.as_ref(), cx);
                }
                true
            }
            key if (grid_focused || search_focused) && event.keystroke.modifiers.control => key
                .parse::<usize>()
                .ok()
                .filter(|number| *number > 0)
                .and_then(|number| EmoteTab::popout_tabs().get(number - 1).copied())
                .map(|tab| {
                    self.select_tab(tab, cx);
                    true
                })
                .unwrap_or(false),
            _ if grid_focused && crate::event_handlers::is_typed_text(event) => {
                let text = event.keystroke.key_char.as_deref();
                if let (Some(text), Some(search_input)) = (text, self.search_input.clone()) {
                    let focus = search_input.read(cx).focus_handle().clone();
                    window.focus(&focus, cx);
                    search_input.update(cx, |input, cx| input.insert(text, window, cx));
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

    fn toggle_favorite(&mut self, emote: &Emote, cx: &mut Context<Self>) {
        self.core.toggle_favorite(emote);
        self.rebuild_virtual_rows();
        self.prefetch_emotes();
        cx.notify();
    }

    fn toggle_category(&mut self, category_id: &str, cx: &mut Context<Self>) {
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
        self.prefetch_emotes();
        cx.notify();
    }

    fn select_tab(&mut self, tab: EmoteTab, cx: &mut Context<Self>) {
        self.core.select_tab(tab);
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.prefetch_emotes();
        cx.notify();
    }

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
                            if this.ignored_search_input_value.as_deref() == Some(text.as_str()) {
                                this.ignored_search_input_value = None;
                                return;
                            }
                            this.ignored_search_input_value = None;
                            this.schedule_search_update(text, cx);
                        }
                    }
                    InputEvent::PressEnter => {
                        this.flush_search_for_navigation(cx);
                        this.confirm_selection(cx);
                    }
                    InputEvent::Blur => this.enter_pressed = false,
                    InputEvent::Focus => {}
                },
            );

            let focus_handle = search_input.read(cx).focus_handle().clone();
            self.search_input = Some(search_input);
            self._subscriptions.push(subscription);
            window.focus(&focus_handle, cx);
        }
    }

    fn invalidate_pending_search(&mut self) -> u64 {
        self.search_revision = self.search_revision.wrapping_add(1);
        self.search_revision
    }

    fn schedule_search_update(&mut self, search: String, cx: &mut Context<Self>) {
        let revision = self.invalidate_pending_search();
        let channel = self.core.channel.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;

            this.update(cx, move |picker, cx| {
                if picker.search_revision != revision
                    || picker.core.channel != channel
                    || !POPOUT_OPEN.load(Ordering::SeqCst)
                {
                    return;
                }

                picker.core.set_search(search);
                picker.reset_visible_emote_limit();
                picker.rebuild_virtual_rows();
                picker.reset_selection_to_first();
                picker.prefetch_emotes();
                crate::backend::metrics::record_ui_notify_source(
                    "picker.popout.search_debounce_apply",
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn flush_search_for_navigation(&mut self, cx: &mut Context<Self>) {
        let search = self
            .search_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_else(|| self.core.search_query.clone());
        self.invalidate_pending_search();
        if self.core.search_query != search {
            self.core.set_search(search);
            self.reset_visible_emote_limit();
            self.rebuild_virtual_rows();
            self.reset_selection_to_first();
            self.prefetch_emotes();
            crate::backend::metrics::record_ui_notify_source(
                "picker.popout.search_navigation_flush",
            );
            cx.notify();
        }
    }

    fn clear_search_input_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate_pending_search();
        let search_input = self.search_input.clone();
        if let Some(search_input) = search_input
            && !search_input.read(cx).value().is_empty()
        {
            self.ignored_search_input_value = Some(String::new());
            search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.invalidate_pending_search();
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_search_input_value(window, cx);
        self.core.set_search(String::new());
        self.reset_visible_emote_limit();
        self.rebuild_virtual_rows();
        self.reset_selection_to_first();
        self.prefetch_emotes();
        if let Some(input) = &self.search_input {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn prefetch_emotes(&mut self) {
        self.prefetch_visible_range(
            0,
            PICKER_INITIAL_PREFETCH_ROWS,
            None,
            PickerPrefetchPlanKind::Initial,
        );
    }

    fn persist_window_bounds(&mut self, window: &Window, cx: &App) {
        if window.is_maximized() {
            return;
        }

        let bounds = window.window_bounds().get_bounds();
        let position = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let display_resolution_key = (
            position.0,
            position.1,
            size.0,
            size.1,
            window.scale_factor(),
        );
        if self.last_display_resolution_key == Some(display_resolution_key) {
            return;
        }
        let display_uuid = window
            .display(cx)
            .and_then(|display| display.uuid().ok())
            .map(|uuid| uuid.to_string());
        let position_changed = self
            .last_persisted_position
            .map(|last| (last.0 - position.0).abs() >= 1.0 || (last.1 - position.1).abs() >= 1.0)
            .unwrap_or(true);
        let size_changed = (self.last_persisted_size.0 - size.0).abs() >= 1.0
            || (self.last_persisted_size.1 - size.1).abs() >= 1.0;
        let display_changed = self.last_persisted_display_uuid != display_uuid;
        if !position_changed && !size_changed && !display_changed {
            self.last_display_resolution_key = Some(display_resolution_key);
            return;
        }

        if let Some(display_uuid) = display_uuid.as_deref() {
            crate::backend::settings::set_emote_picker_popout_window_geometry_for_display(
                display_uuid,
                position.0,
                position.1,
                size.0,
                size.1,
            );
        } else {
            crate::backend::settings::set_emote_picker_popout_window_position(
                position.0, position.1,
            );
            crate::backend::settings::set_emote_picker_popout_window_size(size.0, size.1);
        }
        self.last_persisted_position = Some(position);
        self.last_persisted_size = size;
        self.last_persisted_display_uuid = display_uuid;
        self.last_display_resolution_key = Some(display_resolution_key);
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
            image_cache::PickerViewport::Popout,
        );
        self.last_published_visible_range = published.then_some(key);
    }

    fn prefetch_visible_range(
        &mut self,
        start_row: usize,
        end_row: usize,
        exact_animated_range: Option<(usize, usize)>,
        kind: PickerPrefetchPlanKind,
    ) {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(self.window_scale);
        let started_at = Instant::now();

        let start_row = start_row.min(self.virtual_rows.len());
        let end_row = end_row.min(self.virtual_rows.len());
        let (exact_animated_start, exact_animated_end) = exact_animated_range
            .map(|(start, end)| {
                (
                    start.min(self.virtual_rows.len()),
                    end.min(self.virtual_rows.len()),
                )
            })
            .filter(|(start, end)| end > start)
            .unwrap_or((0, 0));
        let range_key = (
            kind,
            start_row,
            end_row,
            exact_animated_start,
            exact_animated_end,
        );
        let same_range = Some(range_key) == self.last_prefetch_range;

        let now = Instant::now();
        if same_range && now.duration_since(self.last_prefetch_time) < Duration::from_millis(700) {
            return;
        }
        self.last_prefetch_time = now;
        self.last_prefetch_range = Some(range_key);

        let mut buffered_emotes: Vec<Arc<Emote>> = Vec::with_capacity(220);
        let mut exact_animated_emotes: Vec<Arc<Emote>> = Vec::with_capacity(96);
        for row_idx in start_row..end_row {
            if let Some(VirtualRow::EmoteRow { emotes, .. }) = self.virtual_rows.get(row_idx) {
                for (emote, _) in emotes {
                    buffered_emotes.push(Arc::clone(emote));
                    if self.animation_mode == EmotePickerAnimationMode::Always
                        && emote.is_animated
                        && row_idx >= exact_animated_start
                        && row_idx < exact_animated_end
                    {
                        exact_animated_emotes.push(Arc::clone(emote));
                    }
                }
            }
        }

        let buffered_animation_mode = if self.animation_mode == EmotePickerAnimationMode::Always {
            EmotePickerAnimationMode::Hover
        } else {
            self.animation_mode
        };
        let mut plan = build_prefetch_plan(buffered_emotes.iter(), buffered_animation_mode);
        if self.animation_mode == EmotePickerAnimationMode::Always {
            let mut exact_plan = build_prefetch_plan(
                exact_animated_emotes.iter(),
                EmotePickerAnimationMode::Always,
            );
            plan.animated_requests
                .append(&mut exact_plan.animated_requests);
        }
        let plan_metric = match kind {
            PickerPrefetchPlanKind::Initial => "picker.popout.prefetch.initial_plan",
            PickerPrefetchPlanKind::Viewport => "picker.popout.prefetch.viewport_plan",
        };
        if plan.is_empty() {
            crate::backend::metrics::record_emote_stage(plan_metric, started_at.elapsed(), 0);
            return;
        }
        let prefetch_items =
            plan.static_requests.len() + plan.poster_requests.len() + plan.animated_requests.len();
        let elapsed = started_at.elapsed();
        crate::backend::metrics::record_emote_stage(plan_metric, elapsed, prefetch_items);
        if crate::log::enabled("emotes") || crate::log::enabled("perf") {
            crate::log_emotes!(
                debug,
                "popout {} prefetch rows={}..{} exact_animated_rows={}..{} emotes={} static={} posters={} animated={} elapsed_ms={}",
                kind.label(),
                start_row,
                end_row,
                exact_animated_start,
                exact_animated_end,
                buffered_emotes.len(),
                plan.static_requests.len(),
                plan.poster_requests.len(),
                plan.animated_requests.len(),
                elapsed.as_millis()
            );
        }
        execute_prefetch(plan, PICKER_ANIMATED_PREFETCH_BATCH_LIMIT);
    }

    fn render_search_header(
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
            self.focus_order.include(focus_handle.clone());
            apply_standard_input_chrome(
                div()
                    .id("emote-popout-search-shell")
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
                    .id("emote-popout-search-shell-fallback")
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

        let clear_search_focus =
            has_search.then(|| self.focus_order.register("popout-clear-search", cx));

        div()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(colors::border())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::text_primary())
                    .child(format!("Emotes - {}", self.core.channel)),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(search_child)
                    .when_some(clear_search_focus, |el, focus| {
                        el.child(
                            div()
                                .id("emote-popout-clear-search-btn")
                                .track_focus(&focus)
                                .role(Role::Button)
                                .aria_label("Clear search")
                                .on_hover(crate::components::overlays::text_tooltip_on_hover(
                                    "emote-popout-clear-search-btn".to_string(),
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
                    }),
            )
    }

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tabs = EmoteTab::popout_tabs();
        let tab_focuses: Vec<_> = tabs
            .iter()
            .map(|tab| self.focus_order.register(format!("popout-tab-{tab:?}"), cx))
            .collect();

        div()
            .id("popout-tab-bar")
            .role(Role::TabList)
            .aria_label("Emote categories")
            .aria_orientation(Orientation::Horizontal)
            .w_full()
            .h(px(36.0))
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
                    .px_2()
                    .gap_1()
                    .children(tabs.iter().copied().zip(tab_focuses).map(|(tab, focus)| {
                        let is_selected = tab == self.core.active_tab;

                        div()
                            .id(("popout-tab", tab as usize))
                            .role(Role::Tab)
                            .aria_label(tab.label())
                            .aria_selected(is_selected)
                            .track_focus(&focus)
                            .px_2()
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

    fn render_content(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let grid_focus = self.grid_focus.clone();
        if let Some(focus) = grid_focus.clone() {
            self.focus_order.include(focus);
        }
        let grid_label = format!("Emotes, {}", self.core.active_tab.label());
        let loading_state = self.core.get_loading_state();
        if loading_state == LoadingState::Loading {
            let loading_glyph = div()
                .text_2xl()
                .text_color(colors::accent_text())
                .child("⏳");
            let loading_glyph: AnyElement = if cx.reduce_motion() {
                loading_glyph.into_any_element()
            } else {
                loading_glyph
                    .with_animation(
                        "spinner",
                        Animation::new(std::time::Duration::from_secs(1))
                            .repeat()
                            .with_easing(pulsating_between(0.6, 1.0)),
                        |s, delta| s.opacity(delta),
                    )
                    .into_any_element()
            };
            return div()
                .id("popout-emote-grid")
                .role(Role::Grid)
                .aria_label(grid_label)
                .when_some(grid_focus, |grid, focus| grid.track_focus(&focus))
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(loading_glyph)
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

            return div()
                .id("popout-emote-grid")
                .role(Role::Grid)
                .aria_label(grid_label)
                .when_some(grid_focus, |grid, focus| grid.track_focus(&focus))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child(message),
                )
                .into_any_element();
        }

        let virtual_rows = Arc::clone(&self.virtual_rows);
        let row_count = virtual_rows.len();
        let version = self.render_version;
        let selected_idx = self.selected_emote_index;
        let animation_mode = self.animation_mode;
        let picker_channel = self.core.channel.clone();
        let picker = cx.entity().downgrade();
        let picker_timing_enabled = *PICKER_TIMING_ENABLED;
        let row_focuses: Arc<Vec<Option<FocusHandle>>> = Arc::new(
            virtual_rows
                .iter()
                .enumerate()
                .map(|(row_idx, row)| match row {
                    VirtualRow::Header(header) => Some(self.focus_order.register_row(
                        format!("popout-category-{}", header.id),
                        row_idx,
                        &self.scroll_handle,
                        cx,
                    )),
                    VirtualRow::LoadMore { .. } => Some(self.focus_order.register_row(
                        "popout-load-more",
                        row_idx,
                        &self.scroll_handle,
                        cx,
                    )),
                    VirtualRow::EmoteRow { .. } => None,
                })
                .collect(),
        );

        div()
            .id("popout-emote-grid")
            .role(Role::Grid)
            .aria_label(grid_label)
            .when_some(grid_focus, |grid, focus| grid.track_focus(&focus))
            .flex_1()
            .p_2()
            .child(
                uniform_list(
                    ("popout-emote-list", version),
                    row_count,
                    move |range: Range<usize>, window: &mut Window, _cx: &mut App| {
                        let _scale_scope =
                            crate::components::chat::canvas_renderer::layout_scale::enter(
                                window.scale_factor(),
                            );
                        let timing_started = picker_timing_enabled.then(Instant::now);
                        let observed_emote_cells = visible_emote_cell_count(
                            virtual_rows.as_slice(),
                            range.start,
                            range.end,
                        );
                        if observed_emote_cells > 0 && range.end > range.start {
                            if get_popout_visible_range() != (range.start, range.end) {
                                publish_visible_emote_rows(
                                    virtual_rows.as_slice(),
                                    range.start,
                                    range.end,
                                    animation_mode,
                                    image_cache::PickerViewport::Popout,
                                );
                            }
                            signal_popout_visible_range(range.start, range.end);
                        }
                        emote_picker_stability::record_visible_range(
                            range.start,
                            range.end,
                            observed_emote_cells,
                        );
                        let compact_animations =
                            super::emote_picker_prefetch::picker_uses_compact_animation_variant(
                                visible_animated_emote_cell_count(
                                    virtual_rows.as_slice(),
                                    range.start,
                                    range.end,
                                ),
                                animation_mode,
                            );
                        let built: Vec<_> = range
                            .map(|row_idx| {
                                let row = &virtual_rows[row_idx];
                                Self::render_virtual_row(
                                    row,
                                    row_idx,
                                    selected_idx,
                                    version,
                                    animation_mode,
                                    compact_animations,
                                    row_focuses[row_idx].clone(),
                                    &picker_channel,
                                    window.is_a11y_active(),
                                    &picker,
                                )
                            })
                            .collect();
                        if let Some(started) = timing_started {
                            PICKER_TIMING_ROW_BUILD_NS
                                .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                            PICKER_TIMING_ROW_BUILD_CALLBACKS.fetch_add(1, Ordering::Relaxed);
                        }
                        built
                    },
                )
                .flex_1()
                .h_full()
                .track_scroll(&self.scroll_handle),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_virtual_row(
        row: &VirtualRow,
        row_idx: usize,
        selected_idx: usize,
        render_version: u32,
        animation_mode: EmotePickerAnimationMode,
        compact_animations: bool,
        focus_handle: Option<FocusHandle>,
        channel: &str,
        a11y_active: bool,
        picker: &WeakEntity<Self>,
    ) -> AnyElement {
        match row {
            VirtualRow::Header(header) => {
                let header_id = header.id.clone();
                let expand_header_id = header.id.clone();
                let collapse_header_id = header.id.clone();
                let is_collapsed = header.is_collapsed;
                let control = div()
                    .id(("popout-header", row_idx))
                    .role(Role::Button)
                    .aria_label(header.name.clone())
                    .aria_expanded(!header.is_collapsed)
                    .when_some(focus_handle, |row, focus| row.track_focus(&focus))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(Radii::sm())
                    .cursor_pointer()
                    .hover(|s| s.bg(colors::bg_hover()))
                    .on_picker_activate(move |_, window, _| {
                        signal_popout_header_click(header_id.clone());
                        window.refresh();
                    })
                    .on_a11y_action(accesskit::Action::Expand, move |_, window, _| {
                        if is_collapsed {
                            signal_popout_header_click(expand_header_id.clone());
                            window.refresh();
                        }
                    })
                    .on_a11y_action(accesskit::Action::Collapse, move |_, window, _| {
                        if !is_collapsed {
                            signal_popout_header_click(collapse_header_id.clone());
                            window.refresh();
                        }
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
                            } else {
                                colors::accent()
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
                    );
                picker_control_row("popout-header-row", row_idx, control)
            }
            VirtualRow::LoadMore { shown, total } => {
                let remaining = total.saturating_sub(*shown);
                let control = div()
                    .id(("popout-emote-load-more", row_idx))
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
                            .id("emote-picker-popout-load-more")
                            .role(Role::Button)
                            .aria_label("Load more emotes")
                            .when_some(focus_handle, |button, focus| button.track_focus(&focus))
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
                                signal_popout_load_more_click();
                                window.refresh();
                            })
                            .child(if remaining > 0 {
                                format!("Load more ({remaining})")
                            } else {
                                "Load more".to_string()
                            }),
                    );
                picker_control_row("popout-load-more-row", row_idx, control)
            }
            VirtualRow::EmoteRow { emotes, base_index } => div()
                .id(("popout-emote-row", row_idx))
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
                    let active_media_identity = crate::backend::types::media_source_id_for_url(
                        emote,
                        &active_animation_url,
                    );
                    let (cell_width, cell_height, img_width, img_height) =
                        calculate_emote_dimensions(emote);
                    let is_animated = emote.is_animated;
                    let should_animate = is_animated
                        && animation_mode
                            .animates_cell(hovered_popout_emote_index() == Some(emote_idx));
                    let is_fav = *is_favorite;
                    let media_identity =
                        crate::backend::types::media_source_id_for_url(emote, &emote_url);
                    let tooltip_text = picker_tooltip_text(emote);
                    let tooltip_source =
                        format!("{POPOUT_TOOLTIP_SOURCE}:{render_version}:{emote_idx}");
                    let click_emote = Arc::clone(emote);
                    let accessible_click_emote = Arc::clone(emote);
                    let favorite_emote = Arc::clone(emote);
                    let focus_emote = Arc::clone(emote);
                    let focus_picker = picker.clone();
                    let click_picker = picker.clone();
                    let accessible_click_picker = picker.clone();

                    div()
                        .id(("popout-emote", picker_cell_id(emote)))
                        .role(Role::GridCell)
                        .aria_column_index(i + 1)
                        .when(a11y_active, |cell| {
                            cell.aria_label(picker_accessible_label(emote, channel))
                                .aria_selected(is_selected)
                                .when(is_selected, |cell| cell.aria_active_descendant())
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
                        .on_activate_without_tab_stop(move |_, _window, app| {
                            let _ = click_picker.update(app, |picker, cx| {
                                picker.select_emote(click_emote.as_ref(), cx);
                                cx.notify();
                            });
                        })
                        .on_a11y_action(accesskit::Action::Click, move |_, _window, app| {
                            let _ = accessible_click_picker.update(app, |picker, cx| {
                                picker.select_emote(accessible_click_emote.as_ref(), cx);
                                cx.notify();
                            });
                        })
                        .on_a11y_action(accesskit::Action::Focus, move |_, window, app| {
                            let focus = focus_picker
                                .update(app, |picker, cx| {
                                    let index = picker
                                        .core
                                        .flat_emotes
                                        .iter()
                                        .take(picker.visible_emote_count)
                                        .position(|emote| Arc::ptr_eq(emote, &focus_emote))?;
                                    picker.apply_selection_move(index, cx);
                                    picker.grid_focus.clone()
                                })
                                .ok()
                                .flatten();
                            if let Some(focus) = focus {
                                window.focus(&focus, app);
                            }
                        })
                        .on_right_click(move |_, window, _| {
                            signal_popout_favorite_click(Arc::clone(&favorite_emote));
                            window.refresh();
                        })
                        .on_hover({
                            let tooltip_text = tooltip_text.clone();
                            let tooltip_source = tooltip_source.clone();
                            move |hovered, window, _app| {
                                if animation_mode == EmotePickerAnimationMode::Hover
                                    && signal_hovered_popout_emote(
                                        emote_idx,
                                        is_animated,
                                        *hovered,
                                    )
                                {
                                    window.refresh();
                                }
                                if *hovered {
                                    show_text_tooltip(
                                        window.window_handle(),
                                        tooltip_source.clone(),
                                        tooltip_text.clone(),
                                        window.mouse_position(),
                                    );
                                    window.refresh();
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
                                emote_picker_stability::record_cell(
                                    emote_idx,
                                    PickerCellVisualState::Placeholder,
                                );
                                div()
                                    .w(px(img_width))
                                    .h(px(img_height))
                                    .rounded(Radii::sm())
                                    .bg(colors::bg_hover())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .px_1()
                                    .text_xs()
                                    .text_color(colors::text_muted())
                                    .child(emote.name.to_string())
                                    .into_any_element()
                            };
                            let rendered = |element: AnyElement| {
                                emote_picker_stability::record_cell(
                                    emote_idx,
                                    PickerCellVisualState::Rendered,
                                );
                                element
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
                                rendered(
                                    img(emote_url.clone())
                                        .id(("popout-animated-emote", stable_instance_id))
                                        .w(px(img_width))
                                        .h(px(img_height))
                                        .object_fit(ObjectFit::Contain)
                                        .into_any_element(),
                                )
                            } else if should_animate {
                                match super::emote_picker_prefetch::picker_current_animated_frame(
                                    &emote_url,
                                    fallback_animation_url.as_deref(),
                                    &active_animation_url,
                                ) {
                                    Some((
                                        render_image,
                                        _metadata,
                                        displayed_frame_index,
                                        active_url,
                                    )) => {
                                        emote_picker_stability::record_animated_frame_source(
                                            emote_idx,
                                            active_url,
                                            displayed_frame_index,
                                        );
                                        rendered(
                                            img(render_image)
                                                .w(px(img_width))
                                                .h(px(img_height))
                                                .object_fit(ObjectFit::Contain)
                                                .into_any_element(),
                                        )
                                    }
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
                                            image_cache::get_animated_poster_render_image(&emote_url)
                                        })
                                        .or_else(|| {
                                            image_cache::get_stable_animated_render_image(&emote_url)
                                        })
                                        .or_else(|| {
                                            image_cache::get_cached_render_image(&emote_url)
                                        }) {
                                            Some(render_image) => rendered(
                                                img(render_image)
                                                    .w(px(img_width))
                                                    .h(px(img_height))
                                                    .object_fit(ObjectFit::Contain)
                                                    .into_any_element(),
                                            ),
                                            _ => placeholder(),
                                        }
                                    }
                                }
                            } else if is_animated {
                                match image_cache::get_animated_poster_render_image(&emote_url)
                                    .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
                                {
                                    Some(render_image) => rendered(
                                        img(render_image)
                                            .w(px(img_width))
                                            .h(px(img_height))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                    ),
                                    _ => {
                                        if !image_cache::is_downloading(&emote_url) {
                                            image_cache::trigger_animated_poster_preload_media_single_with_source(
                                                &emote_url,
                                                media_identity.clone(),
                                                image_cache::PreloadSource::PickerCell,
                                            );
                                        }
                                        match image_cache::get_animated_poster_render_image(&emote_url)
                                            .or_else(|| image_cache::get_stable_animated_render_image(&emote_url))
                                        {
                                            Some(render_image) => rendered(
                                                img(render_image)
                                                    .w(px(img_width))
                                                    .h(px(img_height))
                                                    .object_fit(ObjectFit::Contain)
                                                    .into_any_element(),
                                            ),
                                            _ => placeholder(),
                                        }
                                    }
                                }
                            } else {
                                match image_cache::get_cached_render_image(&emote_url) {
                                    Some(render_image) => rendered(
                                        img(render_image)
                                            .w(px(img_width))
                                            .h(px(img_height))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                    ),
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
                .into_any_element(),
        }
    }
}

impl Render for EmotePickerPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        self.window_scale = window.scale_factor();
        self.always_on_top.sync(window);
        if *PICKER_TIMING_ENABLED {
            window.set_gpu_timing_enabled(true);
            self.timing_render_count = self.timing_render_count.saturating_add(1);
            let elapsed = self.timing_last_report.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let renders = std::mem::take(&mut self.timing_render_count);
                let row_ns = PICKER_TIMING_ROW_BUILD_NS.swap(0, Ordering::Relaxed);
                let row_callbacks = PICKER_TIMING_ROW_BUILD_CALLBACKS.swap(0, Ordering::Relaxed);
                let (start, end) = get_popout_visible_range();
                let visible_cells =
                    visible_emote_cell_count(self.virtual_rows.as_slice(), start, end);
                let animated_cells =
                    visible_animated_emote_cell_count(self.virtual_rows.as_slice(), start, end);
                let gpu = window.gpu_frame_statistics();
                let present = window.present_statistics();
                let cache = image_cache::picker_media_diagnostics();
                let (exact_frames, held_frames) =
                    super::emote_picker_prefetch::take_frame_delivery_timing();
                let (_, frame_bytes, _, source_bytes, _) = image_cache::decoded_media_residency();
                let present_count = present
                    .as_ref()
                    .map_or(0, |stats| stats.present_count as u64);
                let present_delta = present_count.saturating_sub(self.timing_last_present_count);
                self.timing_last_present_count = present_count;
                crate::log_ui!(
                    info,
                    render_count = renders,
                    active = window.is_window_active(),
                    exact_frames,
                    held_frames,
                    render_hz = renders as f64 / elapsed.as_secs_f64(),
                    visible_cells,
                    animated_cells,
                    total_catalog = self.total_available_emotes,
                    row_build_callbacks = row_callbacks,
                    row_build_ms = row_ns as f64 / 1_000_000.0,
                    gpu_ms = gpu.as_ref().map(|stats| stats.gpu_frame_time_ms),
                    gpu_samples = gpu.as_ref().map_or(0, |stats| stats.sample_count),
                    present_count,
                    present_delta,
                    present_hz = present_delta as f64 / elapsed.as_secs_f64(),
                    present_latency_ms = present.as_ref().map(|stats| stats.present_latency_ms),
                    hot_frames = cache.hot_frames,
                    hot_frame_urls = cache.hot_frame_urls,
                    hot_frames_per_url_max = cache.hot_frames_max_per_url,
                    hot_frame_entry_limit = cache.hot_frame_entry_limit,
                    hot_frame_entries_over_limit = cache.hot_frame_entries_over_limit,
                    picker_protected_frames = cache.picker_protected_frames,
                    picker_protected_bytes = cache.picker_protected_bytes,
                    frame_bytes,
                    decoded_animation_source_bytes = source_bytes,
                    picker_touched_urls = cache.picker_touched_urls,
                    "picker.timing.window"
                );
                #[cfg(feature = "gpui-frame-duration-histogram")]
                {
                    let snapshot = window.frame_duration_snapshot();
                    let draw = snapshot.draw_duration_histogram;
                    let present_hist = snapshot.present_interval_histogram;
                    crate::log_ui!(
                        info,
                        draw_samples = draw.len(),
                        draw_p50_ms = draw.value_at_quantile(0.50) as f64 / 1_000_000.0,
                        draw_p95_ms = draw.value_at_quantile(0.95) as f64 / 1_000_000.0,
                        present_samples = present_hist.len(),
                        present_p50_ms = present_hist.value_at_quantile(0.50) as f64 / 1_000_000.0,
                        present_p95_ms = present_hist.value_at_quantile(0.95) as f64 / 1_000_000.0,
                        "picker.timing.histogram"
                    );
                }
                self.timing_last_report = Instant::now();
            }
        }
        crate::utils::drain_render_image_drops_coalesced(window, cx);
        colors::begin_secondary_window_frame();
        let pending_signals = take_pending_popout_signals();
        if pending_signals {
            self.apply_pending_channel_switch(window, cx);
        }
        let settings_revision = POPOUT_SETTINGS_REVISION.load(Ordering::Relaxed);
        let animation_mode = match self
            .bench_sweep
            .as_ref()
            .and_then(|sweep| sweep.animation_mode_override)
        {
            Some(mode) => mode,
            None if settings_revision != self.settings_revision => {
                self.settings_revision = settings_revision;
                crate::backend::settings::get_emote_picker_animation_mode()
            }
            None => self.animation_mode,
        };
        if animation_mode != self.animation_mode {
            self.cancel_animation_waker();
            self.animation_mode = animation_mode;
            HOVERED_POPOUT_EMOTE_INDEX.store(NO_HOVERED_POPOUT_EMOTE, Ordering::Relaxed);
            self.last_prefetch_range = None;
            self.last_published_visible_range = None;
        }
        if self.title_channel != self.core.channel {
            self.title_channel = self.core.channel.clone();
            window.set_window_title(&format!("Emotes - {}", self.title_channel));
        }
        self.ensure_search_input(window, cx);
        if self.grid_focus.is_none() {
            self.grid_focus = Some(cx.focus_handle());
        }
        self.update_row_width_hint(window);
        let now = Instant::now();

        if now.duration_since(self.last_refresh) >= Duration::from_millis(100)
            && super::emote_picker::apply_pending_notifications()
        {
            self.last_refresh = now;
            cx.notify();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }

        if now.duration_since(self.last_update_check) >= self.update_check_interval {
            self.last_update_check = now;
            if self.core.check_for_emote_updates() {
                self.rebuild_virtual_rows();
                self.prefetch_emotes();
                self.update_check_interval = Duration::from_millis(UPDATE_CHECK_INTERVAL_MIN_MS);
            } else {
                let next_ms = (self.update_check_interval.as_millis() as u64 * 2)
                    .min(UPDATE_CHECK_INTERVAL_MAX_MS);
                self.update_check_interval = Duration::from_millis(next_ms);
            }
        }

        if pending_signals {
            if let Some(emote) = take_pending_popout_favorite_click() {
                self.toggle_favorite(emote.as_ref(), cx);
            }
            if let Some(header_id) = take_pending_popout_header_click() {
                self.toggle_category(&header_id, cx);
            }
            if take_pending_popout_load_more_click() {
                self.request_load_more(cx);
            }
        }

        {
            let (start, end) = get_popout_visible_range();
            self.publish_visible_emotes(start, end);
            if end > start {
                const BUFFER_ROWS: usize = 6;
                let buffered_start = start.saturating_sub(BUFFER_ROWS);
                let buffered_end = (end + BUFFER_ROWS).min(self.virtual_rows.len());
                self.prefetch_visible_range(
                    buffered_start,
                    buffered_end,
                    Some((start, end)),
                    PickerPrefetchPlanKind::Viewport,
                );
            }
        }
        self.drive_bench_picker_sweep(window, cx);
        self.persist_window_bounds(window, cx);
        self.ensure_animation_waker(cx);
        let grid_focus = self
            .grid_focus
            .as_ref()
            .expect("emote picker popout grid focus initialized")
            .clone();

        ensure_global_tooltip_animation_waker(window.window_handle(), cx);
        self.focus_order.begin();
        let search_header = self.render_search_header(window, cx);
        let tab_bar = self.render_tab_bar(cx);
        let content = self.render_content(cx);
        let load_more_warning = self.show_load_more_warning.then(|| {
            render_load_more_warning(
                "emote-popout-load-more-warning",
                self.hide_load_more_warning_next_time,
                Self::cancel_load_more,
                Self::toggle_hide_load_more_warning,
                Self::confirm_load_more,
                &mut self.focus_order,
                cx,
            )
            .into_any_element()
        });
        if load_more_warning.is_some() {
            self.focus_order.ensure_focused(window, cx);
        } else if self.pending_grid_focus {
            window.focus(&grid_focus, cx);
            self.pending_grid_focus = false;
        }

        div()
            .id("emote-picker-popout-root")
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .relative()
            .bg(colors::bg_surface())
            .key_context("EmotePickerPopout")
            .occlude()
            .capture_action(cx.listener(|this, _: &FocusNext, window, cx| {
                this.focus_order.cycle(false, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &FocusPrev, window, cx| {
                this.focus_order.cycle(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, _window, cx| {
                if this.enter_pressed {
                    cx.stop_propagation();
                } else {
                    this.enter_pressed = true;
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                let ime_composing = this.search_input.as_ref().is_some_and(|input| {
                    let input = input.read(cx);
                    input.focus_handle().is_focused(window) && input.marked_range().is_some()
                });
                if !ime_composing {
                    this.clear_search_or_close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_grid_key(event, window, cx);
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _window, _cx| {
                if event.keystroke.key == "enter" {
                    this.enter_pressed = false;
                }
            }))
            .child(search_header)
            .child(tab_bar)
            .child(content)
            .when_some(load_more_warning, |s, warning| s.child(warning))
            .when_some(
                render_global_tooltip_overlay(
                    window.window_handle(),
                    window.viewport_size(),
                    window.mouse_position(),
                ),
                |s, tooltip| s.child(tooltip),
            )
    }
}
