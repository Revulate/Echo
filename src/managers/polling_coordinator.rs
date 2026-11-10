//! Polling Coordinator
//!
//! ## Reactive Architecture (GPUI Best Practices)
//!
//! Chat messages use GPUI's reactive Entity/EventEmitter pattern:
//! - ChatBackend drains observer events into per-channel ChatState entities
//! - ChatView subscribes directly to its channel ChatState for split-local redraws
//! - App-level notifies are reserved for structural UI changes
//!
//! This coordinator still handles:
//! - Observer event processing (bridging background IRC to UI thread)
//! - Twitch events (250ms), UI actions (on wake)
//! - Connection status, auth state (1s)
//! - Stream info refresh (30s)
//! - Animation ticks (33ms adaptive via FrameRateController)
//! - Layout cleanup (30s)
//! - Image cleanup
//!
//! ## Frame Rate Control
//!
//! Uses FrameRateController for adaptive polling:
//! - Interactive: 16ms (60fps) - user actively scrolling/dragging/typing
//! - Animating: 33ms (30fps) - emotes animating but no user input
//! - FocusedIdle: 100ms (10fps) - recently active but idle
//! - DeepIdle/TrueIdle: low-frequency or event-driven idle based on real activity
//!
//! Window focus is not part of polling cadence. Losing OS focus must not delay
//! chat delivery, emote loads, 7TV live updates, layout work, or animation wakeups.
//!
//! UI actions requiring direct split/input access (reply, thread open, mention insert)
//! are forwarded to EchoApp via pending queues in backend::ui_actions.

mod ui_action_dispatch;

use crate::backend;
use crate::cache::loader as image_cache;
use crate::components::chat::canvas_renderer::global_layout_cache;
use crate::config;
use crate::managers::dynamic_tick_scheduler;
use crate::managers::frame_rate_controller;
use crate::managers::{OverlayManager, refresh_auth_cache_if_changed, take_pending_popouts};
use crate::state::{AppState, EmoteStateManager, is_placeholder};
use crate::theme::colors::invalidate_theme_cache;
use echo_core::settings_event::settings_event_requires_layout_invalidation;
use gpui::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use ui_action_dispatch::dispatch_ui_actions;

static SETTINGS_EVENT_SEEN_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_SETTINGS_EVENT_LOG_MS: AtomicU64 = AtomicU64::new(0);

static SIDEBAR_REFRESH_LAST_US: AtomicU64 = AtomicU64::new(0);
static SIDEBAR_REFRESH_MAX_US: AtomicU64 = AtomicU64::new(0);
static SIDEBAR_REFRESH_LOG_TIME_MS: AtomicU64 = AtomicU64::new(0);

fn stream_info_refresh_snapshot(state: &AppState) -> (Vec<String>, Vec<(String, u64)>) {
    let mut channels: Vec<String> = state
        .all_active_channels()
        .into_iter()
        .filter(|channel| !backend::whispers::is_whispers_channel(channel))
        .collect();
    channels.sort_unstable();
    channels.dedup();
    let last_hashes = channels
        .iter()
        .filter_map(|channel| {
            state.stream_info.get(channel).map(|info| {
                (
                    channel.clone(),
                    backend::stream_info_gate_hash(info.as_ref()),
                )
            })
        })
        .collect();
    (channels, last_hashes)
}

/// Split headers print live uptime in whole minutes, but stream-info polls
/// skip unchanged results. Notify once whenever any live channel's minute
/// rolls over so the label keeps counting without a repaint loop. The
/// fingerprint covers each live (channel, minute) pair, so a channel going
/// offline in the same tick as another's rollover cannot cancel it out.
fn notify_live_uptime_rollover(
    app_state: &Entity<AppState>,
    last_uptime_fingerprint: &mut u64,
    cx: &mut AsyncApp,
) {
    use std::hash::{Hash, Hasher};

    app_state.update(cx, |state, cx| {
        let mut live_minutes: Vec<(&str, i64)> = state
            .stream_info
            .iter()
            .map(|(channel, info)| {
                (
                    channel.as_str(),
                    backend::live_uptime_minutes(info.as_ref()),
                )
            })
            .filter(|(_, minutes)| *minutes > 0)
            .collect();
        live_minutes.sort_unstable();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        live_minutes.hash(&mut hasher);
        let fingerprint = hasher.finish();
        if fingerprint != *last_uptime_fingerprint {
            *last_uptime_fingerprint = fingerprint;
            crate::backend::metrics::record_ui_notify_source("polling.live_uptime");
            cx.notify();
        }
    });
}

fn open_external_link(url: &str, mode: backend::ui_actions::LinkOpenMode) {
    match mode {
        backend::ui_actions::LinkOpenMode::Configured => crate::utils::open_url(url),
        backend::ui_actions::LinkOpenMode::Normal => crate::utils::open_url_normal(url),
        backend::ui_actions::LinkOpenMode::Incognito => crate::utils::open_url_incognito(url),
    }
}

/// Set by a finished background clear so the next poll refreshes every
/// window, including a settings popout the main window's notify never reaches.
static CLEAR_FILES_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);

/// Run a clear action off the UI thread, then report it and refresh the
/// logging section.
fn spawn_clear_files(
    clear: fn() -> Result<backend::history_cache::ClearedFiles, String>,
    success: &'static str,
    noun: &'static str,
    failure: &'static str,
) {
    let spawned = std::thread::Builder::new()
        .name("echo-clear-files".to_string())
        .spawn(move || {
            let result = clear();
            crate::components::settings::invalidate_logging_scan();
            CLEAR_FILES_REFRESH_PENDING.store(true, Ordering::Release);
            push_clear_files_result(result, success, noun, failure);
        });
    if let Err(error) = spawned {
        push_clear_files_result(
            Err(format!("Could not start the clear task: {error}")),
            success,
            noun,
            failure,
        );
    }
}

fn push_clear_files_result(
    result: Result<backend::history_cache::ClearedFiles, String>,
    success: &str,
    noun: &str,
    failure: &str,
) {
    let action = match result {
        Ok(cleared) if cleared.failed == 0 => backend::UiAction::ShowSuccessToast {
            message: success.to_string(),
            detail: Some(format!("Removed {} {noun} file(s)", cleared.removed)),
        },
        Ok(cleared) => {
            crate::log_persist!(
                warn,
                "{failure}: removed {}, failed {}",
                cleared.removed,
                cleared.failed
            );
            backend::UiAction::ShowWarningToast {
                message: failure.to_string(),
                detail: Some(format!(
                    "Removed {}, failed {} {noun} file(s)",
                    cleared.removed, cleared.failed
                )),
            }
        }
        Err(error) => {
            crate::log_persist!(warn, "{failure}: {error}");
            backend::UiAction::ShowWarningToast {
                message: failure.to_string(),
                detail: Some(error),
            }
        }
    };
    backend::push_ui_action(action);
}

#[inline]
fn settings_event_requires_picker_refresh(event: &crate::backend::settings::SettingsEvent) -> bool {
    matches!(
        event,
        crate::backend::settings::SettingsEvent::AnimationPerformanceChanged
    )
}

#[inline]
fn settings_event_requires_reduce_motion_sync(
    event: &crate::backend::settings::SettingsEvent,
) -> bool {
    matches!(
        event,
        crate::backend::settings::SettingsEvent::AnimationPerformanceChanged
    )
}

fn handle_settings_event_side_effects(event: &crate::backend::settings::SettingsEvent) {
    use crate::backend::settings::SettingsEvent;
    match event {
        SettingsEvent::ThemeChanged => invalidate_theme_cache(),
        SettingsEvent::ProxyChanged => {
            backend::eventsub::reconnect_for_proxy_change();
            backend::emotes::seventv_eventapi::restart_for_proxy_change();
            backend::irc::reconnect_with_current_auth();
        }
        _ => {}
    }
}

#[derive(Default)]
struct DrainedSettingsEvents {
    saw_event: bool,
    needs_invalidation: bool,
    needs_picker_refresh: bool,
    needs_reduce_motion_sync: bool,
}

/// Drain pending settings changes while keeping each polling mode responsible
/// for its own notification and activity policy.
fn drain_settings_events(
    rx: &mut tokio::sync::broadcast::Receiver<crate::backend::settings::SettingsEvent>,
    mut before_side_effects: impl FnMut(&crate::backend::settings::SettingsEvent),
) -> DrainedSettingsEvents {
    let mut drained = DrainedSettingsEvents::default();

    while let Ok(event) = rx.try_recv() {
        before_side_effects(&event);
        handle_settings_event_side_effects(&event);
        drained.needs_invalidation |= settings_event_requires_layout_invalidation(&event);
        drained.needs_picker_refresh |= settings_event_requires_picker_refresh(&event);
        drained.needs_reduce_motion_sync |= settings_event_requires_reduce_motion_sync(&event);
        drained.saw_event = true;
    }

    drained
}

#[inline]
fn maybe_reload_custom_theme_file(cadence: &mut Cadence) {
    if !cadence.take_due() {
        return;
    }
    if let Err(err) = backend::settings::reload_custom_theme_file_if_changed() {
        crate::log_ui!(warn, "Theme file auto-reload failed: {}", err);
    }
}

#[inline]
fn maybe_refresh_system_theme(cadence: &mut Cadence) {
    if !cadence.is_due() || !backend::settings::is_system_theme_active() {
        return;
    }
    cadence.mark();
    let _ = backend::settings::refresh_system_theme_resolution(false);
}

#[inline]
/// A lane that has never run (`None`) is due immediately.
fn poll_due(last: Option<Instant>, interval: Duration) -> bool {
    last.is_none_or(|last| last.elapsed() >= interval)
}

fn maybe_refresh_streamer_detection(cadence: &mut Cadence) {
    if !cadence.is_due()
        || !matches!(
            backend::streamer_mode::get_streamer_mode_settings().enabled,
            backend::streamer_mode::StreamerModeStatus::Auto
        )
    {
        return;
    }
    cadence.mark();
    let _ = backend::streamer_mode::is_streaming_software_running();
}

/// Change key for the Activity sidebar snapshots owned by AppState.
///
/// `refresh_sidebar_caches` rebuilds both Vecs unconditionally, so buffer
/// identity cannot distinguish a real change from a rebuild of identical rows.
fn sidebar_cache_signature(state: &AppState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.cached_mentions.len().hash(&mut hasher);
    for (channel, message) in state.cached_mentions.iter() {
        channel.hash(&mut hasher);
        message.id.hash(&mut hasher);
        (Arc::as_ptr(&message.slots).cast::<()>() as usize).hash(&mut hasher);
    }
    state.cached_whispers.len().hash(&mut hasher);
    for thread in state.cached_whispers.iter() {
        thread.username.hash(&mut hasher);
        thread.unread_count.hash(&mut hasher);
        thread.last_timestamp.hash(&mut hasher);
        thread.last_message.hash(&mut hasher);
    }
    hasher.finish()
}

/// Coordinator for background polling operations
///
/// This is the "central nervous system" of the application, responsible for:
/// - Checking backend state at various intervals
/// - Updating app state when changes are detected
/// - Batching notifications to minimize re-renders
/// - Coordinating with managers via Entity updates
/// - Bridging emote loading events to the GPUI reactive system
pub struct PollingCoordinator {
    app_state: Entity<AppState>,
    overlay_manager: Entity<OverlayManager>,
    emote_state: Option<Entity<EmoteStateManager>>,
    _task: Option<Task<()>>,
}

impl PollingCoordinator {
    pub fn new(app_state: Entity<AppState>, overlay_manager: Entity<OverlayManager>) -> Self {
        Self {
            app_state,
            overlay_manager,
            emote_state: None,
            _task: None,
        }
    }

    /// Set the EmoteStateManager entity (called from EchoApp after creation)
    pub fn set_emote_state(&mut self, emote_state: Entity<EmoteStateManager>) {
        self.emote_state = Some(emote_state);
    }

    /// Start the polling loop as a background task
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if !polling_allowed(crate::bench::network_lanes_suppressed()) {
            crate::log_ui!(
                info,
                "Offline benchmark suppressed PollingCoordinator network lanes"
            );
            return;
        }
        let mut poll = PollLoop::new(
            self.app_state.clone(),
            self.overlay_manager.clone(),
            self.emote_state.clone(),
        );

        let task = cx.spawn(async move |this, cx| {
            loop {
                let render_mode = frame_rate_controller::current_mode_cached();
                if render_mode.is_event_driven() {
                    poll.event_driven_pass(render_mode, &this, cx).await;
                } else {
                    poll.timed_pass(render_mode, &this, cx).await;
                }
            }
        });

        self._task = Some(task);
    }
}

/// One polling lane's schedule: when it last ran and how often it runs.
struct Cadence {
    /// `None` until the lane first runs, which makes it due at once.
    last: Option<Instant>,
    every: Duration,
}

impl Cadence {
    /// A lane that first becomes due one interval from now.
    fn after(every: Duration) -> Self {
        Self {
            last: Some(Instant::now()),
            every,
        }
    }

    /// A lane that is due on its first check.
    fn immediate(every: Duration) -> Self {
        Self { last: None, every }
    }

    fn is_due(&self) -> bool {
        poll_due(self.last, self.every)
    }

    fn mark(&mut self) {
        self.last = Some(Instant::now());
    }

    /// Mark the lane as run and return true when it is due.
    fn take_due(&mut self) -> bool {
        let due = self.is_due();
        if due {
            self.mark();
        }
        due
    }
}

/// Everything the polling loop carries from one pass to the next.
struct PollLoop {
    app_state: Entity<AppState>,
    overlay_manager: Entity<OverlayManager>,
    emote_state: Option<Entity<EmoteStateManager>>,

    events: Cadence,
    status: Cadence,
    emotes: Cadence,
    stream_info: Cadence,
    image_cleanup: Cadence,
    layout_cleanup: Cadence,
    sidebar_refresh: Cadence,
    empty_channel_recovery: Cadence,
    theme_file_reload: Cadence,
    system_theme: Cadence,
    streamer_detection: Cadence,

    last_uptime_fingerprint: u64,
    tracked_global_emote_version: u32,
    tracked_user_emote_version: u64,
    tracked_channel_emote_versions: FxHashMap<String, u32>,
    initialized_channels: FxHashSet<String>,
    tracked_channels_key: Option<u64>,
    settings_rx: Option<tokio::sync::broadcast::Receiver<crate::backend::settings::SettingsEvent>>,
}

impl PollLoop {
    fn new(
        app_state: Entity<AppState>,
        overlay_manager: Entity<OverlayManager>,
        emote_state: Option<Entity<EmoteStateManager>>,
    ) -> Self {
        Self {
            app_state,
            overlay_manager,
            emote_state,
            events: Cadence::after(config::event_poll_interval()),
            status: Cadence::after(config::status_poll_interval()),
            emotes: Cadence::after(Duration::from_millis(250)),
            stream_info: Cadence::immediate(config::stream_info_interval()),
            image_cleanup: Cadence::after(Duration::from_secs(60)),
            layout_cleanup: Cadence::after(Duration::from_secs(30)),
            sidebar_refresh: Cadence::after(Duration::from_secs(2)),
            empty_channel_recovery: Cadence::immediate(Duration::from_millis(500)),
            theme_file_reload: Cadence::after(Duration::from_secs(2)),
            system_theme: Cadence::after(Duration::from_secs(2)),
            streamer_detection: Cadence::immediate(Duration::from_secs(2)),
            last_uptime_fingerprint: 0,
            tracked_global_emote_version: 0,
            tracked_user_emote_version: 0,
            tracked_channel_emote_versions: FxHashMap::default(),
            initialized_channels: FxHashSet::default(),
            tracked_channels_key: None,
            settings_rx: backend::settings::subscribe_settings(),
        }
    }

    /// One pass of an event-driven idle mode: sleep until activity or the
    /// next health check, then run only what idle needs.
    async fn event_driven_pass(
        &mut self,
        render_mode: frame_rate_controller::RenderMode,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) {
        let woken_by_activity = self
            .sleep_until_activity_or_health_check(render_mode, cx)
            .await;

        dispatch_ui_actions(&self.app_state, &self.overlay_manager, cx);

        if self.status.take_due() {
            let _ = backend::poll_connection_status();
        }
        maybe_reload_custom_theme_file(&mut self.theme_file_reload);
        maybe_refresh_system_theme(&mut self.system_theme);
        maybe_refresh_streamer_detection(&mut self.streamer_detection);

        let should_recover_empty_channels = self.empty_channel_recovery.is_due();
        if woken_by_activity || should_recover_empty_channels {
            self.process_idle_wakeup(render_mode, should_recover_empty_channels, this, cx);
            if should_recover_empty_channels {
                self.empty_channel_recovery.mark();
            }
            self.drain_idle_settings_events(this, cx);
        }

        self.refresh_stream_info(cx);
    }

    async fn sleep_until_activity_or_health_check(
        &self,
        render_mode: frame_rate_controller::RenderMode,
        cx: &mut AsyncApp,
    ) -> bool {
        let wakeup = frame_rate_controller::get_wakeup_notify();
        let mut health_interval = if backend::settings::get_custom_theme_file_auto_reload()
            || backend::settings::is_system_theme_active()
        {
            self.theme_file_reload.every
        } else {
            Duration::from_secs(30)
        };
        if matches!(
            backend::streamer_mode::get_streamer_mode_settings().enabled,
            backend::streamer_mode::StreamerModeStatus::Auto
        ) {
            health_interval = health_interval.min(self.streamer_detection.every);
        }
        health_interval = health_interval.min(self.stream_info.every);

        tokio::select! {
            _ = wakeup.notified() => {
                crate::log_perf!(debug, "Woken from {:?} mode", render_mode);
                true
            }
            _ = cx.background_executor().timer(health_interval) => {
                false
            }
        }
    }

    /// Initialize newly opened channels, drain observer events, recover empty
    /// channels when due, and repaint emote pickers whose images arrived.
    fn process_idle_wakeup(
        &mut self,
        render_mode: frame_rate_controller::RenderMode,
        should_recover_empty_channels: bool,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) {
        let initialized_channels = &mut self.initialized_channels;
        if let Err(e) = this.update(cx, |coordinator, cx| {
            let channels = coordinator.app_state.read(cx).active_tab().channels();

            let new_channels: Vec<String> = channels
                .iter()
                .filter(|ch| !initialized_channels.contains(*ch))
                .cloned()
                .collect();

            if !new_channels.is_empty() {
                crate::log_irc!(
                    debug,
                    "Initializing {} new channels in {:?} mode",
                    new_channels.len(),
                    render_mode
                );
                coordinator.app_state.update(cx, |state, cx| {
                    state.chat_backend.update(cx, |backend_entity, cx| {
                        for channel in &new_channels {
                            backend_entity.get_or_create_channel(channel, cx);
                            initialized_channels.insert(channel.clone());
                        }
                    });
                });
            }

            let has_pending = coordinator
                .app_state
                .read(cx)
                .chat_backend
                .read(cx)
                .has_pending_events();

            if has_pending {
                let processed = coordinator.app_state.update(cx, |state, cx| {
                    state
                        .chat_backend
                        .update(cx, |backend, cx| backend.process_observer_events(cx))
                });
                if processed {
                    crate::log_irc!(debug, "Processed observer events in {:?} mode", render_mode);
                    frame_rate_controller::record_activity();
                }
            }

            if should_recover_empty_channels {
                let recovered = coordinator.app_state.update(cx, |state, cx| {
                    state.chat_backend.update(cx, |backend, cx| {
                        backend.recover_empty_channels_from_backend(&channels, cx)
                    })
                });
                if recovered {
                    frame_rate_controller::record_activity();
                    crate::backend::metrics::record_ui_notify_source(
                        "polling.idle.recover_empty_channels",
                    );
                    coordinator.app_state.update(cx, |_state, cx| {
                        cx.notify();
                    });
                }
            }

            if crate::components::overlays::take_image_ready(
                crate::components::overlays::ImageReadyConsumer::App,
            ) {
                crate::backend::metrics::record_ui_notify_source(
                    "polling.idle.image_ready.drained",
                );
                let emote_picker = coordinator.overlay_manager.read(cx).emote_picker.clone();
                if emote_picker.read(cx).state.is_open {
                    crate::backend::metrics::record_ui_notify_source(
                        "polling.idle.image_ready.emote_picker",
                    );
                    emote_picker.update(cx, |_, cx| cx.notify());
                }
                if crate::components::overlays::refresh_open_emote_picker_popout(cx) {
                    crate::backend::metrics::record_ui_notify_source(
                        "polling.idle.image_ready.emote_picker_popout",
                    );
                }
            }
        }) {
            crate::log_ui!(
                error,
                "Failed to process observer events in idle mode: {:?}",
                e
            );
        }
    }

    fn drain_idle_settings_events(
        &mut self,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) {
        let Some(rx) = self.settings_rx.as_mut() else {
            return;
        };
        let settings_events = drain_settings_events(rx, |_| {});
        apply_settings_events(
            &settings_events,
            "polling.idle.settings.emote_picker_popout",
            this,
            cx,
        );
        if settings_events.saw_event {
            self.app_state.update(cx, |_state, cx| {
                crate::backend::metrics::record_ui_notify_source("polling.idle.settings");
                cx.notify();
            });
        }
    }

    /// One pass of a timed mode: sleep for the mode's interval (or until
    /// activity), then run every lane that is due.
    async fn timed_pass(
        &mut self,
        render_mode: frame_rate_controller::RenderMode,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) {
        let timer_interval = render_mode.poll_interval();
        let timer_sleep = if render_mode == frame_rate_controller::RenderMode::Animating {
            dynamic_tick_scheduler::aligned_tick_delay(timer_interval.as_millis() as u64)
        } else {
            timer_interval
        };
        let wakeup = frame_rate_controller::get_wakeup_notify();
        let woke_by_activity = tokio::select! {
            _ = cx.background_executor().timer(timer_sleep) => false,
            _ = wakeup.notified() => true,
        };

        if woke_by_activity {
            crate::log_perf!(trace, "Woken from {:?} polling sleep", render_mode);
        }

        dispatch_ui_actions(&self.app_state, &self.overlay_manager, cx);

        if frame_rate_controller::should_skip_frame() {
            return;
        }

        if render_mode == frame_rate_controller::RenderMode::DeepIdle {
            self.deep_idle_housekeeping(this, cx);
        }

        let mut needs_notify = false;

        maybe_refresh_streamer_detection(&mut self.streamer_detection);

        if !image_cache::has_visible_animations() {
            dynamic_tick_scheduler::reset_to_default();
        }

        self.repaint_emote_pickers_for_loaded_images(cx);
        needs_notify |= self.drain_timed_settings_events(this, cx);
        needs_notify |= expire_toasts(this, cx);
        needs_notify |= self.sync_active_channels(this, cx);
        needs_notify |= self.poll_twitch_events();
        self.poll_connection_and_auth(this, cx);
        maybe_reload_custom_theme_file(&mut self.theme_file_reload);
        maybe_refresh_system_theme(&mut self.system_theme);
        self.refresh_stream_info(cx);
        self.refresh_sidebar_caches(cx);
        self.poll_emote_versions(this, cx);

        if self.image_cleanup.take_due() {
            image_cache::sweep_idle_decoded_media();
        }

        crate::backend::memory_report::maybe_log_memory_breakdown();

        if self.layout_cleanup.take_due() {
            self.periodic_cleanup(cx);
        }

        open_requested_windows(cx);

        if needs_notify {
            self.app_state.update(cx, |_state, cx| {
                crate::backend::metrics::record_ui_notify_source("polling.batched");
                cx.notify();
            });
        }
    }

    /// The extra checks deep idle runs before the shared timed lanes.
    fn deep_idle_housekeeping(&mut self, this: &WeakEntity<PollingCoordinator>, cx: &mut AsyncApp) {
        if self.status.take_due() {
            let _ = backend::poll_connection_status();
        }
        maybe_reload_custom_theme_file(&mut self.theme_file_reload);
        if let Ok(has_events) = this.update(cx, |coordinator, cx| {
            coordinator
                .app_state
                .read(cx)
                .chat_backend
                .read(cx)
                .has_pending_events()
        }) && has_events
        {
            frame_rate_controller::record_activity();
        }
        if let Some(rx) = self.settings_rx.as_mut() {
            let settings_events = drain_settings_events(rx, |_| {});
            apply_settings_events(
                &settings_events,
                "polling.deep_idle.settings.emote_picker_popout",
                this,
                cx,
            );
            if settings_events.saw_event {
                self.app_state.update(cx, |_state, cx| {
                    crate::backend::metrics::record_ui_notify_source("polling.deep_idle.settings");
                    cx.notify();
                });
                frame_rate_controller::record_activity();
            }
        }
    }

    fn repaint_emote_pickers_for_loaded_images(&self, cx: &mut AsyncApp) {
        if !crate::components::overlays::take_image_ready(
            crate::components::overlays::ImageReadyConsumer::App,
        ) {
            return;
        }
        crate::backend::metrics::record_ui_notify_source("polling.image_ready.drained");
        self.overlay_manager.update(cx, |manager, cx| {
            let emote_picker = manager.emote_picker.clone();
            if emote_picker.read(cx).state.is_open {
                crate::backend::metrics::record_ui_notify_source(
                    "polling.image_ready.emote_picker",
                );
                emote_picker.update(cx, |_, cx| cx.notify());
            }
            if crate::components::overlays::refresh_open_emote_picker_popout(cx) {
                crate::backend::metrics::record_ui_notify_source(
                    "polling.image_ready.emote_picker_popout",
                );
            }
        });
    }

    /// Returns whether a settings event arrived, which the batched notify covers.
    fn drain_timed_settings_events(
        &mut self,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) -> bool {
        let Some(rx) = self.settings_rx.as_mut() else {
            return false;
        };
        let settings_events = drain_settings_events(rx, |event| {
            let seen = SETTINGS_EVENT_SEEN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            let now = crate::utils::unix_now_ms();
            let last = LAST_SETTINGS_EVENT_LOG_MS.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= 500
                && LAST_SETTINGS_EVENT_LOG_MS
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                crate::log_ui!(
                    debug,
                    count = seen,
                    "PollingCoordinator saw settings event: {:?}",
                    event
                );
            }
        });
        apply_settings_events(
            &settings_events,
            "polling.settings.emote_picker_popout",
            this,
            cx,
        );
        settings_events.saw_event
    }

    /// Initialize newly opened channels, drain observer events and recover
    /// empty channels when due. Returns whether channels were recovered.
    fn sync_active_channels(
        &mut self,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) -> bool {
        let should_recover_empty_channels = self.empty_channel_recovery.is_due();
        let initialized_channels = &mut self.initialized_channels;
        let tracked_channel_emote_versions = &mut self.tracked_channel_emote_versions;
        let tracked_channels_key = &mut self.tracked_channels_key;
        let recovered_empty_channels = this
            .update(cx, |coordinator, cx| {
                let has_pending = coordinator
                    .app_state
                    .read(cx)
                    .chat_backend
                    .read(cx)
                    .has_pending_events();

                let channels_key = coordinator
                    .app_state
                    .read(cx)
                    .active_tab()
                    .channels_change_key();
                if !has_pending
                    && !should_recover_empty_channels
                    && *tracked_channels_key == Some(channels_key)
                {
                    return false;
                }

                // Forget channels the backend closed so a reopened
                // channel is initialized again and neither set grows
                // for the whole session.
                {
                    let app_state = coordinator.app_state.read(cx);
                    let open = &app_state.chat_backend.read(cx).channels;
                    initialized_channels.retain(|channel| {
                        if channel
                            .bytes()
                            .any(|byte| byte.is_ascii_uppercase() || !byte.is_ascii())
                        {
                            open.contains_key(&channel.to_lowercase())
                        } else {
                            open.contains_key(channel.as_str())
                        }
                    });
                    tracked_channel_emote_versions.retain(|channel, _| open.contains_key(channel));
                }

                let channels = coordinator.app_state.read(cx).active_tab().channels();

                let has_new_channels = channels.iter().any(|ch| !initialized_channels.contains(ch));

                if !has_new_channels && !has_pending && !should_recover_empty_channels {
                    *tracked_channels_key = Some(channels_key);
                    return false;
                }

                let new_channels: Vec<String> = if has_new_channels {
                    channels
                        .iter()
                        .filter(|ch| !initialized_channels.contains(*ch))
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };

                if !new_channels.is_empty() {
                    coordinator.app_state.update(cx, |state, cx| {
                        state.chat_backend.update(cx, |backend_entity, cx| {
                            for channel in &new_channels {
                                backend_entity.get_or_create_channel(channel, cx);
                                initialized_channels.insert(channel.clone());
                            }
                        });
                    });
                }
                *tracked_channels_key = Some(channels_key);

                if has_pending {
                    let processed = coordinator.app_state.update(cx, |state, cx| {
                        state.chat_backend.update(cx, |backend_entity, cx| {
                            backend_entity.process_observer_events(cx)
                        })
                    });

                    if processed {
                        frame_rate_controller::record_activity();
                    }
                }

                let mut recovered_empty_channels = false;
                if should_recover_empty_channels {
                    recovered_empty_channels = coordinator.app_state.update(cx, |state, cx| {
                        state.chat_backend.update(cx, |backend, cx| {
                            backend.recover_empty_channels_from_backend(&channels, cx)
                        })
                    });
                    if recovered_empty_channels {
                        frame_rate_controller::record_activity();
                    }
                }

                recovered_empty_channels
            })
            .unwrap_or(false);
        if should_recover_empty_channels {
            self.empty_channel_recovery.mark();
        }
        if recovered_empty_channels {
            frame_rate_controller::record_activity();
        }
        recovered_empty_channels
    }

    /// Returns whether Twitch events arrived.
    fn poll_twitch_events(&mut self) -> bool {
        if !self.events.take_due() {
            return false;
        }
        let events = backend::poll_twitch_events();
        if events.is_empty() {
            return false;
        }
        frame_rate_controller::record_activity();
        true
    }

    fn poll_connection_and_auth(
        &mut self,
        this: &WeakEntity<PollingCoordinator>,
        cx: &mut AsyncApp,
    ) {
        if !self.status.take_due() {
            return;
        }
        let status = backend::poll_connection_status();
        let auth_cache_changed = refresh_auth_cache_if_changed();
        if auth_cache_changed {
            frame_rate_controller::record_activity();
        }

        let is_auth = backend::is_authenticated();
        let auth_state = backend::get_auth_state();
        let should_update_state = auth_cache_changed
            || this
                .update(cx, |coordinator, cx| {
                    let state = coordinator.app_state.read(cx);
                    state.connection_status != status
                        || state.is_authenticated != is_auth
                        || state.username != auth_state.username
                })
                .unwrap_or(true);

        if should_update_state {
            self.app_state.update(cx, |state, cx| {
                state.connection_status = status;
                state.is_authenticated = is_auth;
                state.username = auth_state.username.clone();
                cx.notify();
            });
        }
    }

    /// Queue a stream-info refresh when due, apply finished results, and keep
    /// live uptime labels counting.
    fn refresh_stream_info(&mut self, cx: &mut AsyncApp) {
        if self.stream_info.is_due() {
            let (channels, last_hashes) = self
                .app_state
                .update(cx, |state, _| stream_info_refresh_snapshot(state));
            if backend::queue_stream_info_refresh(channels, last_hashes) {
                self.stream_info.mark();
            }
        }
        if let Some(updates) = backend::drain_pending_stream_info() {
            self.app_state.update(cx, |state, state_cx| {
                let applied = state.apply_stream_info_updates(updates);
                if applied > 0 {
                    crate::log_ui!(debug, "stream info applied: {} channels", applied);
                    crate::backend::metrics::record_ui_notify_source("polling.stream_info");
                    state_cx.notify();
                }
            });
        }
        notify_live_uptime_rollover(&self.app_state, &mut self.last_uptime_fingerprint, cx);
    }

    fn refresh_sidebar_caches(&mut self, cx: &mut AsyncApp) {
        if !self.sidebar_refresh.take_due() {
            return;
        }
        self.app_state.update(cx, |state, cx| {
            if !state.activity_open() {
                return;
            }
            let previous_signature = sidebar_cache_signature(state);
            let scan_start = Instant::now();
            state.refresh_sidebar_caches();
            let scan_elapsed_us = scan_start.elapsed().as_micros() as u64;

            SIDEBAR_REFRESH_LAST_US.store(scan_elapsed_us, Ordering::Relaxed);

            let current_max = SIDEBAR_REFRESH_MAX_US.load(Ordering::Relaxed);
            if scan_elapsed_us > current_max {
                SIDEBAR_REFRESH_MAX_US.store(scan_elapsed_us, Ordering::Relaxed);
                crate::log_perf!(debug, "Sidebar refresh scan new max: {}μs", scan_elapsed_us);
            }

            let now_ms = crate::utils::unix_now_ms();
            let last_log_ms = SIDEBAR_REFRESH_LOG_TIME_MS.load(Ordering::Relaxed);
            if now_ms.saturating_sub(last_log_ms) >= 60_000 {
                SIDEBAR_REFRESH_LOG_TIME_MS.store(now_ms, Ordering::Relaxed);
                crate::log_perf!(
                    debug,
                    "Sidebar refresh: last={}μs, max={}μs",
                    scan_elapsed_us,
                    current_max.max(scan_elapsed_us)
                );
            }

            if sidebar_cache_signature(state) != previous_signature {
                crate::backend::metrics::record_ui_notify_source("polling.sidebar_caches");
                cx.notify();
            }
        });
    }

    /// Tell the emote state about global, user and active-channel emote set changes.
    fn poll_emote_versions(&mut self, this: &WeakEntity<PollingCoordinator>, cx: &mut AsyncApp) {
        if !self.emotes.take_due() {
            return;
        }
        let Some(emote_state_entity) = self.emote_state.as_ref() else {
            return;
        };

        let current_global = backend::emotes::get_global_emote_version();
        if current_global != self.tracked_global_emote_version {
            let emote_count = backend::emotes::GLOBAL_EMOTES_LIST
                .read()
                .map(|list| list.len())
                .unwrap_or(0);

            crate::log_emotes!(
                debug,
                "Global emotes changed: {} -> {} ({} emotes)",
                self.tracked_global_emote_version,
                current_global,
                emote_count
            );
            self.tracked_global_emote_version = current_global;

            emote_state_entity.update(cx, |state, cx| {
                state.notify_global_emotes_loaded(emote_count, cx);
            });
        }

        let current_user = crate::state::VersionRegistry::user_emote_version();
        if current_user != self.tracked_user_emote_version {
            let emote_count = backend::emotes::USER_EMOTES_LOOKUP.load().len();

            crate::log_emotes!(
                debug,
                "User emotes changed: {} -> {} ({} emotes)",
                self.tracked_user_emote_version,
                current_user,
                emote_count
            );
            self.tracked_user_emote_version = current_user;

            emote_state_entity.update(cx, |state, cx| {
                state.notify_user_emotes_loaded(emote_count, cx);
            });
        }

        let Ok(channel_updates) = this.update(cx, |coordinator, cx| {
            let active_channels: Vec<String> = coordinator
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .channels()
                .into_iter()
                .filter(|ch| !is_placeholder(ch))
                .collect();

            let mut updates = Vec::new();
            for channel in active_channels {
                let channel_lower = channel.to_lowercase();
                let current_version = backend::emotes::get_emote_cache_version(&channel_lower);
                updates.push((channel_lower, current_version));
            }
            updates
        }) else {
            return;
        };
        for (channel_lower, current_version) in channel_updates {
            let tracked_version = self
                .tracked_channel_emote_versions
                .get(&channel_lower)
                .copied()
                .unwrap_or(0);

            if current_version != tracked_version {
                let emote_count = backend::emotes::get_channel_emotes_fast(&channel_lower).len();

                crate::log_emotes!(
                    debug,
                    "Channel '{}' emotes changed: {} -> {} ({} emotes)",
                    channel_lower,
                    tracked_version,
                    current_version,
                    emote_count
                );
                self.tracked_channel_emote_versions
                    .insert(channel_lower.clone(), current_version);

                emote_state_entity.update(cx, |state, cx| {
                    state.notify_channel_emotes_loaded(&channel_lower, emote_count, cx);
                });
            }
        }
    }

    /// The slow housekeeping lane: caches, orphaned state, retention and stale
    /// emote/observer/texture bookkeeping.
    fn periodic_cleanup(&mut self, cx: &mut AsyncApp) {
        backend::twitch_api::sweep_user_card_caches();

        self.app_state.update(cx, |state, cx| {
            state.cleanup_orphaned_states(cx);

            state.rebalance_message_retention(cx);
        });

        let reclaimed_layouts = global_layout_cache::drain_evicted_message_layouts();
        if reclaimed_layouts > 0 {
            crate::log_perf!(
                debug,
                "Reclaimed {} evicted-message layouts from global cache",
                reclaimed_layouts
            );
        }

        let cleaned = crate::backend::emote_candidates::cleanup_stale_pending(|channel| {
            crate::backend::emotes::EMOTE_CACHE.is_channel_loading(channel)
        });
        if cleaned > 0 {
            crate::log_emotes!(debug, "Cleaned up {} stale pending emote channels", cleaned);
        }

        crate::backend::observers::get_observer_registry().cleanup_stale_observers();

        crate::cache::frame_layer::cleanup_texture_cache();
    }
}

/// Apply the side effects every polling mode shares for drained settings
/// events. Each mode decides how to notify for `saw_event` itself.
fn apply_settings_events(
    settings_events: &DrainedSettingsEvents,
    picker_refresh_source: &'static str,
    this: &WeakEntity<PollingCoordinator>,
    cx: &mut AsyncApp,
) {
    if settings_events.needs_reduce_motion_sync {
        cx.update(|cx| {
            cx.set_reduce_motion(backend::settings::get_reduce_decorative_motion());
        });
    }
    if settings_events.needs_invalidation {
        let _ = global_layout_cache::maybe_invalidate_for_settings_change();
    }
    if settings_events.needs_picker_refresh {
        let _ = this.update(cx, |_coordinator, cx| {
            if crate::components::overlays::refresh_open_emote_picker_popout(cx) {
                crate::backend::metrics::record_ui_notify_source(picker_refresh_source);
            }
        });
    }
}

/// Returns whether an expired toast was removed.
fn expire_toasts(this: &WeakEntity<PollingCoordinator>, cx: &mut AsyncApp) -> bool {
    let mut removed = false;
    let _ = this.update(cx, |coordinator, cx| {
        removed = coordinator.overlay_manager.update(cx, |manager, cx| {
            manager
                .toast_manager
                .update(cx, |toast_mgr, _cx| toast_mgr.remove_expired())
        });
    });
    removed
}

/// Open the emote popouts and settings popout that were requested, and
/// refresh windows after a clear-files action.
fn open_requested_windows(cx: &mut AsyncApp) {
    let popout_requests = take_pending_popouts();
    for channel in popout_requests {
        cx.update(|cx| {
            crate::components::overlays::open_emote_popout(channel, cx);
        });
    }

    if CLEAR_FILES_REFRESH_PENDING.swap(false, Ordering::AcqRel) {
        cx.update(|cx| cx.refresh_windows());
    }

    if let Some(request) = crate::components::settings::drain_settings_popout_request() {
        cx.update(|cx| {
            crate::components::settings::open_settings_popout(request, cx);
        });
    }
}

fn polling_allowed(network_lanes_suppressed: bool) -> bool {
    !network_lanes_suppressed
}

#[cfg(test)]
mod benchmark_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn offline_benchmark_suppresses_polling_coordinator() {
        assert!(!polling_allowed(true));
        assert!(polling_allowed(false));
    }
}
