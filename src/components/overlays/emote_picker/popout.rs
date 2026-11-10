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
use crate::components::ui::input::{Enter, Escape};
use crate::components::ui::{FocusNext, FocusPrev, Retarget, SingletonWindow};
use crate::state::SplitId;
use crate::theme::colors;
use benchmark_driver::BenchPickerSweepState;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use super::core::emote_selection_identity_key;
use super::core::{
    EmotePickerCore, EmoteTab, LoadingState, clear_picker_catalog_handoff, picker_display_url,
    picker_native_animated_enabled,
};
use super::grid::{
    self, MIN_ROW_WIDTH, PICKER_ANIMATION_WAKE_MAX_MS, PickerGrid, PickerHost, PickerMetricNames,
    PickerShell, PrefetchReport, ROW_WIDTH_OVERHEAD, SEARCH_DEBOUNCE_MS, SearchStyle, TabBarStyle,
};
use super::rows::{
    self, GridElementIds, GridStyle, HoverRepaint, PlaceholderStyle, RowBuildTiming,
    visible_animated_emote_cell_count, visible_emote_cell_count,
};
use super::shared::{VirtualRow, render_load_more_warning};
use super::signals::POPOUT_SIGNALS;
use super::stability::{self};
use crate::components::overlays::global_tooltip::{
    ensure_global_tooltip_animation_waker, render_global_tooltip_overlay,
};
use crate::components::overlays::image_ready::ImageReadyConsumer;
use crate::components::overlays::traits::OverlayPointerIsolation;

const POPOUT_TOOLTIP_SOURCE: &str = "emote-picker-popout-cell";

static PICKER_TIMING_ENABLED: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag("ECHO_PICKER_TIMING"));
static PICKER_TIMING_ROW_BUILD_NS: AtomicU64 = AtomicU64::new(0);
static PICKER_TIMING_ROW_BUILD_CALLBACKS: AtomicU64 = AtomicU64::new(0);

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

/// Pending payload: the channel and composer split to retarget the open
/// window to.
static POPOUT: SingletonWindow<(String, Option<SplitId>)> = SingletonWindow::new();
static POPOUT_HOST: PickerHost = PickerHost {
    signals: &POPOUT_SIGNALS,
    tooltip_source: POPOUT_TOOLTIP_SOURCE,
    metrics: PickerMetricNames {
        rebuild_rows: "picker.popout.rebuild_virtual_rows",
        initial_prefetch_plan: "picker.popout.prefetch.initial_plan",
        viewport_prefetch_plan: "picker.popout.prefetch.viewport_plan",
        search_navigation_flush: "picker.popout.search_navigation_flush",
    },
    initial_prefetch_animations: false,
    eager_prefetch: true,
    log_rebuild_perf: true,
    log_prefetch: log_popout_prefetch,
    tabs: EmoteTab::popout_tabs,
    search: SearchStyle {
        shell_id: "emote-popout-search-shell",
        fallback_id: "emote-popout-search-shell-fallback",
        clear_id: "emote-popout-clear-search-btn",
        clear_focus_key: "popout-clear-search",
    },
    image_ready: ImageReadyConsumer::PopoutPicker,
    repaint_on_image_ready: true,
    tab_bar: TabBarStyle {
        bar_id: "popout-tab-bar",
        tab_id: "popout-tab",
        height: 36.0,
        compact: true,
    },
    style: GridStyle {
        ids: GridElementIds {
            grid: "popout-emote-grid",
            list: "popout-emote-list",
            header: "popout-header",
            header_row: "popout-header-row",
            load_more: "popout-emote-load-more",
            load_more_button: "emote-picker-popout-load-more",
            load_more_row: "popout-load-more-row",
            emote_row: "popout-emote-row",
            emote_cell: "popout-emote",
            animated_emote: "popout-animated-emote",
        },
        highlight_special_headers: false,
        empty_state_icon: false,
        placeholder: PlaceholderStyle::Padded,
        hover_repaint: HoverRepaint::RefreshWindow,
        track_ranges_without_emotes: false,
        record_stability: true,
        row_build_timing: Some(RowBuildTiming {
            enabled: picker_timing_enabled,
            record: record_row_build_time,
        }),
    },
};

fn picker_timing_enabled() -> bool {
    *PICKER_TIMING_ENABLED
}

fn record_row_build_time(elapsed: Duration) {
    PICKER_TIMING_ROW_BUILD_NS.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    PICKER_TIMING_ROW_BUILD_CALLBACKS.fetch_add(1, Ordering::Relaxed);
}

fn log_popout_prefetch(report: &PrefetchReport) {
    if report.requests() == 0 || !(crate::log::enabled("emotes") || crate::log::enabled("perf")) {
        return;
    }
    crate::log_emotes!(
        debug,
        "popout {} prefetch rows={}..{} exact_animated_rows={}..{} emotes={} static={} posters={} animated={} elapsed_ms={}",
        report.kind.label(),
        report.rows.0,
        report.rows.1,
        report.exact_animated_rows.0,
        report.exact_animated_rows.1,
        report.emotes,
        report.static_requests,
        report.poster_requests,
        report.animated_requests,
        report.elapsed.as_millis()
    );
}
static POPOUT_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Advances whenever an application-level publication targets the popout, which
/// is how an animation-policy settings change reaches this window.
static POPOUT_SETTINGS_REVISION: AtomicU64 = AtomicU64::new(0);

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
    stability::cancel_stationary_window();
    POPOUT_SIGNALS.reset_visible_range();
    if POPOUT.mark_closed() {
        image_cache::close_picker_media_scope();
    }
    POPOUT_SIGNALS.clear_pending_actions();
    clear_picker_catalog_handoff();
}

/// Close the real picker window during the opt-in hermetic lifecycle benchmark.
/// Ordinary application code must continue to use the window's normal close path.
pub fn benchmark_close_popout(cx: &mut App) -> bool {
    if !crate::bench::ui_fixture::lifecycle_churn_enabled() {
        return false;
    }
    let closed = POPOUT
        .handle()
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
    POPOUT.is_open()
}

/// Refresh the open picker popout from an application-level publication.
///
/// Copy the window handle out of the mutex before entering GPUI. Window updates can
/// synchronously run application code, so retaining the handle lock across `update`
/// would make close/reopen paths vulnerable to lock inversion.
pub fn refresh_open_emote_picker_popout(cx: &mut App) -> bool {
    POPOUT_SETTINGS_REVISION.fetch_add(1, Ordering::Relaxed);
    POPOUT.refresh(cx)
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

    if POPOUT.is_open() {
        POPOUT_SIGNALS.mark_pending();
    }
    let channel = match POPOUT.retarget((channel, split_id), cx) {
        Retarget::Retargeted => return,
        Retarget::NotOpen((channel, _)) => channel,
        Retarget::Stale((channel, _)) => {
            mark_popout_closed();
            channel
        }
    };

    if !POPOUT.mark_open() {
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
            POPOUT.set_handle(window.window_handle());
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
    always_on_top: crate::components::ui::AlwaysOnTopFollower,
    ignored_search_input_value: Option<String>,
    animation_waker_running: bool,
    /// Invalidates a detached wake loop when the popout closes or its policy changes.
    animation_waker_generation: u64,
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
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    last_persisted_display_uuid: Option<String>,
    last_display_resolution_key: Option<(f32, f32, f32, f32, f32)>,
    bench_sweep: Option<BenchPickerSweepState>,
    timing_last_report: Instant,
    timing_render_count: u64,
    timing_last_present_count: u64,
    grid: PickerGrid,
}

impl PickerShell for EmotePickerPopout {
    fn grid(&self) -> &PickerGrid {
        &self.grid
    }

    fn grid_mut(&mut self) -> &mut PickerGrid {
        &mut self.grid
    }

    fn activate_emote(&mut self, emote: Arc<Emote>, _keep_open: bool, cx: &mut Context<Self>) {
        self.select_emote(&emote, cx);
        cx.notify();
    }

    fn accessible_channel(&self) -> &str {
        &self.grid.core.channel
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        EmotePickerPopout::clear_search(self, window, cx);
    }

    fn dismiss(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    fn search_changed(&mut self, text: String, cx: &mut Context<Self>) {
        // A programmatic clear already applied its query; its echo is skipped.
        if self.ignored_search_input_value.take().as_deref() == Some(text.as_str()) {
            return;
        }
        self.schedule_search_update(text, cx);
    }

    fn search_enter_keeps_open(&self) -> bool {
        false
    }
}

impl Drop for EmotePickerPopout {
    fn drop(&mut self) {
        self.cancel_animation_waker();
        self.grid.invalidate_search();
        if POPOUT_GENERATION.load(Ordering::SeqCst) == self.generation {
            crate::components::overlays::global_tooltip::clear_global_tooltip_if_source_prefix(
                POPOUT_TOOLTIP_SOURCE,
            );
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
            always_on_top: Default::default(),
            ignored_search_input_value: None,
            animation_waker_running: false,
            animation_waker_generation: 0,
            settings_revision: POPOUT_SETTINGS_REVISION.load(Ordering::Relaxed),
            title_channel: String::new(),
            visible_animation_key: None,
            visible_animation_retry: None,
            visible_animation_cells: Vec::new(),
            visible_animation_min_frame_ms: None,
            last_animation_frame_signature: None,
            last_persisted_position,
            last_persisted_size,
            last_persisted_display_uuid,
            last_display_resolution_key: None,
            bench_sweep,
            timing_last_report: Instant::now(),
            timing_render_count: 0,
            timing_last_present_count: 0,
            grid: PickerGrid::new(
                &POPOUT_HOST,
                core,
                420.0 - ROW_WIDTH_OVERHEAD,
                animation_mode,
            ),
        };
        picker.grid.rebuild_virtual_rows();
        picker.grid.prefetch_initial();
        picker
    }

    fn apply_pending_channel_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next_channel = POPOUT.take_pending();

        let Some((channel, split_id)) = next_channel else {
            return;
        };

        self.origin_split_id = split_id;

        if channel == self.grid.core.channel {
            clear_picker_catalog_handoff();
            return;
        }

        POPOUT_SIGNALS.clear_pending_actions();
        self.clear_search_input_value(window, cx);
        self.grid.core.set_channel(channel);
        self.grid.core.select_tab(EmoteTab::Channel);
        self.grid.core.set_search(String::new());
        self.grid.reset_visible_emote_limit();
        self.grid.rebuild_virtual_rows();
        self.grid.reset_selection_to_first();
        self.grid.prefetch_initial();
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
            self.grid.render_version,
            start,
            end,
            hovered_index.unwrap_or(usize::MAX),
            image_cache::animated_cache_state_revision(),
            self.grid.animation_mode,
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

        let rows = Arc::clone(&self.grid.virtual_rows);
        let start = start.min(rows.len());
        let end = end.min(rows.len());
        let compact_animations = super::prefetch::picker_uses_compact_animation_variant(
            visible_animated_emote_cell_count(rows.as_slice(), start, end),
            self.grid.animation_mode,
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
                let active_url = super::prefetch::picker_active_animation_url(
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

        let hovered_index = match self.grid.animation_mode {
            EmotePickerAnimationMode::Never => return None,
            EmotePickerAnimationMode::Always => None,
            EmotePickerAnimationMode::Hover => Some(POPOUT_SIGNALS.hovered_emote()?),
        };

        let (start, end) = POPOUT_SIGNALS.visible_range();
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

    fn select_emote(&mut self, emote: &Emote, cx: &mut Context<Self>) {
        // Input can arrive before an existing window paints its requested retarget.
        let Some(split_id) = POPOUT.with_pending(|pending| match pending {
            Some((channel, split_id)) if channel == &self.grid.core.channel => Some(*split_id),
            Some(_) => None,
            None => Some(self.origin_split_id),
        }) else {
            return;
        };
        let identity = emote_selection_identity_key(emote);
        if let Some(index) = self
            .grid
            .visible_position(|candidate| emote_selection_identity_key(candidate) == identity)
        {
            self.grid.select_index(index);
        }
        self.grid.core.record_usage(emote);
        send_to_main(PopoutMessage::EmoteSelected {
            name: emote.name.to_string(),
            channel: self.grid.core.channel.clone(),
            split_id,
        });
        popout_message_events(cx).update(cx, |_, cx| cx.emit(()));
    }

    fn schedule_search_update(&mut self, search: String, cx: &mut Context<Self>) {
        let revision = self.grid.invalidate_search();
        let channel = self.grid.core.channel.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(SEARCH_DEBOUNCE_MS))
                .await;

            this.update(cx, move |picker, cx| {
                if picker.grid.search_revision != revision
                    || picker.grid.core.channel != channel
                    || !POPOUT.is_open()
                {
                    return;
                }

                picker.grid.core.set_search(search);
                picker.grid.reset_visible_emote_limit();
                picker.grid.rebuild_virtual_rows();
                picker.grid.reset_selection_to_first();
                picker.grid.prefetch_initial();
                crate::backend::metrics::record_ui_notify_source(
                    "picker.popout.search_debounce_apply",
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn clear_search_input_value(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.grid.invalidate_search();
        let search_input = self.grid.search_input.clone();
        if let Some(search_input) = search_input
            && !search_input.read(cx).value().is_empty()
        {
            self.ignored_search_input_value = Some(String::new());
            search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
        }
        self.grid.invalidate_search();
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_search_input_value(window, cx);
        self.grid.core.set_search(String::new());
        self.grid.reset_visible_emote_limit();
        self.grid.rebuild_virtual_rows();
        self.grid.reset_selection_to_first();
        self.grid.prefetch_initial();
        if let Some(input) = &self.grid.search_input {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        cx.notify();
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

    fn render_search_header(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        if let Some(input) = &self.grid.search_input {
            let focus_handle = input.read(cx).focus_handle().clone();
            self.grid.focus_order.include(focus_handle);
        }
        let search_child = grid::render_search_field(self, window, cx);
        let clear_button = grid::render_clear_search_button(self, cx);

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
                    .child(format!("Emotes - {}", self.grid.core.channel)),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(search_child)
                    .children(clear_button),
            )
    }
}

impl Render for EmotePickerPopout {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scale_scope =
            crate::components::chat::canvas_renderer::layout_scale::enter(window.scale_factor());
        self.grid.window_scale = window.scale_factor();
        self.always_on_top.sync(window);
        if *PICKER_TIMING_ENABLED {
            window.set_gpu_timing_enabled(true);
            self.timing_render_count = self.timing_render_count.saturating_add(1);
            let elapsed = self.timing_last_report.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let renders = std::mem::take(&mut self.timing_render_count);
                let row_ns = PICKER_TIMING_ROW_BUILD_NS.swap(0, Ordering::Relaxed);
                let row_callbacks = PICKER_TIMING_ROW_BUILD_CALLBACKS.swap(0, Ordering::Relaxed);
                let (start, end) = POPOUT_SIGNALS.visible_range();
                let visible_cells =
                    visible_emote_cell_count(self.grid.virtual_rows.as_slice(), start, end);
                let animated_cells = visible_animated_emote_cell_count(
                    self.grid.virtual_rows.as_slice(),
                    start,
                    end,
                );
                let gpu = window.gpu_frame_statistics();
                let present = window.present_statistics();
                let cache = image_cache::picker_media_diagnostics();
                let (exact_frames, held_frames) = super::prefetch::take_frame_delivery_timing();
                let image_cache::DecodedMediaResidency {
                    frame_bytes,
                    animation_source_bytes: source_bytes,
                    ..
                } = image_cache::decoded_media_residency();
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
                    total_catalog = self.grid.total_available_emotes,
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
        let pending_signals = POPOUT_SIGNALS.take_pending();
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
            None => self.grid.animation_mode,
        };
        if self.grid.set_animation_mode(animation_mode) {
            self.cancel_animation_waker();
        }
        if self.title_channel != self.grid.core.channel {
            self.title_channel = self.grid.core.channel.clone();
            window.set_window_title(&format!("Emotes - {}", self.title_channel));
        }
        grid::ensure_search_input(self, window, cx);
        if self.grid.grid_focus.is_none() {
            self.grid.grid_focus = Some(cx.focus_handle());
        }
        {
            let window_width: f32 = window.viewport_size().width.into();
            self.grid
                .set_row_width_hint((window_width - ROW_WIDTH_OVERHEAD).max(MIN_ROW_WIDTH));
        }
        let now = Instant::now();

        grid::repaint_for_ready_images(self, now, cx);

        self.grid.poll_emote_catalog(now);

        if pending_signals {
            grid::apply_pending_row_actions(self, cx);
        }

        self.grid.publish_and_prefetch_viewport();
        self.drive_bench_picker_sweep(window, cx);
        self.persist_window_bounds(window, cx);
        self.ensure_animation_waker(cx);
        let grid_focus = self
            .grid
            .grid_focus
            .as_ref()
            .expect("emote picker popout grid focus initialized")
            .clone();

        ensure_global_tooltip_animation_waker(window.window_handle(), cx);
        self.grid.focus_order.begin();
        let search_header = self.render_search_header(window, cx);
        let tab_bar = grid::render_tab_bar(self, cx);
        let content = rows::render_grid(self, cx);
        let load_more_warning = self.grid.show_load_more_warning.then(|| {
            render_load_more_warning(
                "emote-popout-load-more-warning",
                self.grid.hide_load_more_warning_next_time,
                grid::cancel_load_more::<Self>,
                grid::toggle_hide_load_more_warning::<Self>,
                grid::confirm_load_more::<Self>,
                &mut self.grid.focus_order,
                cx,
            )
            .into_any_element()
        });
        if load_more_warning.is_some() {
            self.grid.focus_order.ensure_focused(window, cx);
        } else if self.grid.pending_grid_focus {
            window.focus(&grid_focus, cx);
            self.grid.pending_grid_focus = false;
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
            .capture_action(cx.listener(|this, _: &FocusNext, window, cx| {
                this.grid.focus_order.cycle(false, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &FocusPrev, window, cx| {
                this.grid.focus_order.cycle(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, _window, cx| {
                if this.grid.enter_down {
                    cx.stop_propagation();
                } else {
                    this.grid.enter_down = true;
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                grid::capture_escape(this, window, cx);
            }))
            .block_overlay_pointer_passthrough()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                grid::handle_grid_key(this, event, window, cx);
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _window, _cx| {
                if event.keystroke.key == "enter" {
                    this.grid.enter_down = false;
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
