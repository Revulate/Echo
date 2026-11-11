//! Global application state
//!
//! Each tab owns its own split layout.

use super::{TabHighlightState, TabState, TabVisibility};
use crate::backend::lock_utils::MutexExt;
use crate::backend::{self, ChatMessage, ConnectionStatus, StreamInfo, WhisperThread};
use crate::config;
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;

static CHANNEL_CONNECT_WORKER_STARTED: OnceLock<()> = OnceLock::new();

/// Bumped on every connection request so the idle connect worker sleeps
/// until there is work instead of polling the queue.
static CHANNEL_CONNECT_SIGNAL: (std::sync::Mutex<u64>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// Backstop for a request queued without going through
/// `request_channel_connection`.
const CHANNEL_CONNECT_IDLE_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

fn request_channel_connection(channel: String, priority: backend::channel_lifecycle::LoadPriority) {
    backend::channel_lifecycle::get_lifecycle_manager().request_connection(channel, priority);
    let (generation, wake) = &CHANNEL_CONNECT_SIGNAL;
    let mut generation = generation.lock_or_recover();
    *generation = generation.wrapping_add(1);
    wake.notify_one();
}

fn wait_for_channel_connect_request(seen: &mut u64) {
    let (generation, wake) = &CHANNEL_CONNECT_SIGNAL;
    let guard = generation.lock_or_recover();
    let (guard, _) = wake
        .wait_timeout_while(guard, CHANNEL_CONNECT_IDLE_RECHECK, |generation| {
            *generation == *seen
        })
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *seen = *guard;
}

/// Channels waiting for their IRC join, plus whether the shared probe task is
/// running.
struct ChannelConnectProbes {
    /// Pending joins: channel -> (watch start, lowercased name).
    pending: Option<HashMap<String, (std::time::Instant, String)>>,
    task_running: bool,
}

static CHANNEL_CONNECT_PROBES: std::sync::Mutex<ChannelConnectProbes> =
    std::sync::Mutex::new(ChannelConnectProbes {
        pending: None,
        task_running: false,
    });

/// Cuts a slow-poll sleep short when a new join starts being watched.
static CHANNEL_CONNECT_PROBE_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

const CHANNEL_CONNECT_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Poll interval once every pending join is past the probe window.
const CHANNEL_CONNECT_SLOW_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// After this long a join that is no longer wanted is marked disconnected.
/// A channel that is still desired and open stays `Connecting` until the IRC
/// supervisor joins it, so a slow (re)connect is not reported as a failure.
const CHANNEL_CONNECT_PROBE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// After this long even a desired join is marked disconnected so the next
/// tab switch re-queues it.
const CHANNEL_CONNECT_PROBE_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(60);

/// Watch a connecting channel with one shared probe task on the backend
/// runtime instead of a thread per channel.
fn watch_channel_connect(channel: String) {
    let mut probes = CHANNEL_CONNECT_PROBES.lock_or_recover();
    let lower = channel.to_lowercase();
    probes
        .pending
        .get_or_insert_with(HashMap::new)
        .insert(channel, (std::time::Instant::now(), lower));
    if probes.task_running {
        CHANNEL_CONNECT_PROBE_WAKE.notify_one();
        return;
    }
    probes.task_running = true;
    drop(probes);

    backend::bridge::get_runtime().spawn(async {
        let mut interval = CHANNEL_CONNECT_PROBE_INTERVAL;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = CHANNEL_CONNECT_PROBE_WAKE.notified() => {}
            }
            let mut probes = CHANNEL_CONNECT_PROBES.lock_or_recover();
            let pending = probes.pending.get_or_insert_with(HashMap::new);
            let lifecycle = backend::channel_lifecycle::get_lifecycle_manager();
            let mut any_fresh = false;
            pending.retain(|channel, (started, lower)| {
                if lifecycle.get_state(channel)
                    != backend::channel_lifecycle::ChannelState::Connecting
                {
                    return false;
                }
                if backend::irc::is_channel_connected(channel) {
                    lifecycle.mark_connected(channel);
                    return false;
                }
                let elapsed = started.elapsed();
                if elapsed < CHANNEL_CONNECT_PROBE_WINDOW {
                    any_fresh = true;
                    return true;
                }
                if elapsed < CHANNEL_CONNECT_PROBE_GIVE_UP
                    && backend::irc::is_channel_desired(lower)
                    && !backend::irc::is_channel_closed(lower)
                {
                    return true;
                }
                lifecycle.mark_disconnected(channel);
                false
            });
            if pending.is_empty() {
                probes.pending = None;
                probes.task_running = false;
                break;
            }
            interval = if any_fresh {
                CHANNEL_CONNECT_PROBE_INTERVAL
            } else {
                CHANNEL_CONNECT_SLOW_PROBE_INTERVAL
            };
        }
    });
}

/// Spacing between background (distant-tab) channel JOINs in the connect worker.
///
/// Twitch permits roughly 20 JOINs per 10 seconds for a normal account, and the
/// twitch-irc client only rate-limits opening new connections, not JOIN commands.
/// 500ms spacing keeps distant-tab startup joins under that ceiling; visible and
/// adjacent connects are not staggered.
const BACKGROUND_CONNECT_STAGGER_MS: u64 = 500;

/// The single global activity rail section currently selected by the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivitySection {
    #[default]
    All,
    Mentions,
    Whispers,
    Events,
}

/// App-owned state for the one global activity rail.
///
/// This lives beside the main workspace layout because opening the rail changes
/// the width available to every split. Event filtering remains part of the same
/// state so `/automod` does not need a second notification-only surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityPanelState {
    pub open: bool,
    pub section: ActivitySection,
    pub event_filter: crate::backend::ui_actions::NotificationFilter,
}

impl Default for ActivityPanelState {
    fn default() -> Self {
        Self {
            open: false,
            section: ActivitySection::All,
            event_filter: crate::backend::ui_actions::NotificationFilter::All,
        }
    }
}

/// Global application state
pub struct AppState {
    /// List of tabs (each with its own split layout). Shared so render can
    /// clone it cheaply; mutate through `tabs_mut()`.
    pub tabs: Arc<Vec<TabState>>,
    /// Index of the active tab
    pub active_tab_idx: usize,
    /// Current connection status
    pub connection_status: ConnectionStatus,
    /// Whether the user is authenticated (synced from backend in polling loop)
    pub is_authenticated: bool,
    /// Username of the authenticated user (synced from backend in polling loop)
    pub username: Option<String>,
    /// Chat backend entity (manages all channel states reactively)
    pub chat_backend: Entity<super::ChatBackend>,
    /// Cached stream info per channel
    pub stream_info: HashMap<String, Option<StreamInfo>>,
    /// The one global combined Activity feed and its source filters.
    pub activity_panel: ActivityPanelState,
    /// Cached mentions, shared so render can clone them cheaply.
    pub cached_mentions: Arc<Vec<(String, ChatMessage)>>,
    /// Cached whisper threads, shared so render can clone them cheaply.
    pub cached_whispers: Arc<Vec<WhisperThread>>,
    /// Splits whose channel chooser is open, with the channel each showed
    /// before. The old channel stays joined until the change is committed.
    pub pending_channel_changes: HashMap<super::SplitId, String>,
    /// The window hosting `EchoApp`, refreshed on tab switches.
    pub main_window: Option<gpui::AnyWindowHandle>,
    /// Tabs closed this session with their former indices, one entry per close
    /// action, newest last. SplitIds are kept so drafts come back on reopen.
    closed_tabs: std::collections::VecDeque<(u64, Vec<(usize, TabState)>)>,
    closed_tab_generation: u64,
}

const MAX_CLOSED_TAB_BATCHES: usize = 10;

/// Whether the tab at `idx` sits directly beside the tab at `active`.
fn is_adjacent_tab(idx: usize, active: usize) -> bool {
    idx.abs_diff(active) == 1
}

/// How urgently a tab's channels connect: the active tab now, its neighbours
/// as a prefetch, and every other tab in the background queue.
fn tab_load_priority(idx: usize, active: usize) -> backend::channel_lifecycle::LoadPriority {
    if idx == active {
        backend::channel_lifecycle::LoadPriority::Immediate
    } else if is_adjacent_tab(idx, active) {
        backend::channel_lifecycle::LoadPriority::Prefetch
    } else {
        backend::channel_lifecycle::LoadPriority::Background
    }
}

fn migrated_or_default_tabs() -> Vec<TabState> {
    if let Some(migrated) = config::migrate_workspace_to_tabs() {
        crate::log_ui!(info, "Migrated workspace to {} tab(s)", migrated.len());
        if let Err(e) = config::save_tabs(&migrated) {
            crate::log_ui!(warn, "Failed to save migrated tabs: {}", e);
        }
        migrated
    } else {
        crate::log_ui!(info, "Creating default tab");
        let mut tab = TabState::new();
        tab.is_selected = true;
        vec![tab]
    }
}

impl AppState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        if crate::bench::ui_fixture::disposable_workspace_enabled() {
            let mut placeholder = TabState::new();
            placeholder.is_selected = true;
            let mut state = Self::with_tabs(vec![placeholder], 0, cx);
            let snapshot = crate::bench::ui_fixture::seed_app_state(&mut state);
            debug_assert!(snapshot.is_some(), "UI fixture failed to seed");
            return state;
        }

        let tabs = if let Some(channel) = crate::bench::live_channel::requested_channel() {
            let mut tab = TabState::new();
            tab.is_selected = true;
            let assigned = tab.set_channel_at_path(&[], channel.to_string());
            debug_assert!(
                assigned,
                "live benchmark channel must replace the root split"
            );
            crate::log_ui!(
                info,
                "Live benchmark startup opening #{} through the normal active-tab lifecycle",
                channel
            );
            vec![tab]
        } else {
            match config::load_tabs() {
                Ok(loaded) if !loaded.is_empty() => {
                    crate::log_ui!(info, "Loaded {} tabs from tabs.json", loaded.len());
                    loaded
                }
                Ok(_) => {
                    crate::log_ui!(info, "No persisted tabs found, creating default workspace");
                    migrated_or_default_tabs()
                }
                Err(e) => {
                    crate::log_ui!(warn, "Failed to load tabs: {}", e);
                    migrated_or_default_tabs()
                }
            }
        };

        let mut tabs = tabs;
        let has_selected = tabs.iter().any(|t| t.is_selected);
        if !has_selected && !tabs.is_empty() {
            tabs[0].is_selected = true;
        }

        let active_tab_idx = tabs.iter().position(|t| t.is_selected).unwrap_or(0);
        let popup_channels = backend::settings::get_open_chat_popup_channels();

        let network_lanes_suppressed = crate::bench::network_lanes_suppressed();
        let mut startup_asset_channels = Vec::new();
        let mut connected = HashSet::new();

        for (idx, tab) in tabs.iter().enumerate() {
            for channel in tab.channels() {
                if !connected.insert(channel.clone()) {
                    continue;
                }
                if network_lanes_suppressed {
                    continue;
                }

                let priority = tab_load_priority(idx, active_tab_idx);

                if priority != backend::channel_lifecycle::LoadPriority::Background
                    && !backend::whispers::is_whispers_channel(&channel)
                {
                    startup_asset_channels.push(channel.clone());
                }
                request_channel_connection(channel.clone(), priority);
            }
        }

        let mut popup_only_channel_count = 0usize;
        for channel in &popup_channels {
            if connected.insert(channel.clone()) {
                if network_lanes_suppressed {
                    continue;
                }
                popup_only_channel_count += 1;
                if !backend::whispers::is_whispers_channel(channel) {
                    startup_asset_channels.push(channel.clone());
                }
                request_channel_connection(
                    channel.clone(),
                    backend::channel_lifecycle::LoadPriority::Immediate,
                );
            }
        }

        if network_lanes_suppressed {
            crate::log_net!(
                info,
                channels = connected.len(),
                "Offline benchmark suppressed restored channel connections and asset hydration"
            );
        } else {
            crate::log_ui!(
                info,
                "Startup connect: {} selected-tab, {} popup-only, {} prefetch (adjacent), {} background (distant tabs)",
                tabs.get(active_tab_idx)
                    .map(|t| t.channels().len())
                    .unwrap_or(0),
                popup_only_channel_count,
                tabs.iter()
                    .enumerate()
                    .filter(|(idx, _)| is_adjacent_tab(*idx, active_tab_idx))
                    .map(|(_, t)| t.channels().len())
                    .sum::<usize>(),
                tabs.iter()
                    .enumerate()
                    .filter(|(idx, _)| {
                        *idx != active_tab_idx && !is_adjacent_tab(*idx, active_tab_idx)
                    })
                    .map(|(_, t)| t.channels().len())
                    .sum::<usize>()
            );
        }

        if !network_lanes_suppressed {
            CHANNEL_CONNECT_WORKER_STARTED.get_or_init(|| {
                let spawn_result = std::thread::Builder::new()
                    .name("channel-connect-worker".to_string())
                    .spawn(move || {
                        use backend::channel_lifecycle::get_lifecycle_manager;
                        let lifecycle = get_lifecycle_manager();
                        let mut seen_request = *CHANNEL_CONNECT_SIGNAL.0.lock_or_recover();

                        loop {
                            if let Some((priority, channel)) = lifecycle.next_to_connect() {
                                crate::log_irc!(debug, "Connecting channel '{}'", channel);
                                lifecycle.mark_connecting(&channel);

                                backend::connect_channel(&channel);

                                watch_channel_connect(channel.clone());

                                if priority == backend::channel_lifecycle::LoadPriority::Background
                                {
                                    std::thread::sleep(std::time::Duration::from_millis(
                                        BACKGROUND_CONNECT_STAGGER_MS,
                                    ));
                                }
                            } else {
                                wait_for_channel_connect_request(&mut seen_request);
                            }
                        }
                    });

                if let Err(e) = spawn_result {
                    crate::log_irc!(error, "Failed to start channel connect worker: {}", e);
                }
            });
        }

        if !startup_asset_channels.is_empty() {
            backend::load_channel_assets_batch(startup_asset_channels.clone());
        }

        Self::with_tabs(tabs, active_tab_idx, cx)
    }

    /// A disconnected, signed-out state over `tabs` with a fresh chat backend.
    fn with_tabs(tabs: Vec<TabState>, active_tab_idx: usize, cx: &mut Context<Self>) -> Self {
        Self {
            tabs: Arc::new(tabs),
            active_tab_idx,
            connection_status: ConnectionStatus::Disconnected,
            is_authenticated: false,
            username: None,
            chat_backend: cx.new(|_cx| super::ChatBackend::new()),
            stream_info: HashMap::new(),
            activity_panel: ActivityPanelState::default(),
            cached_mentions: Arc::new(Vec::new()),
            cached_whispers: Arc::new(Vec::new()),
            pending_channel_changes: HashMap::new(),
            main_window: None,
            closed_tabs: std::collections::VecDeque::new(),
            closed_tab_generation: 0,
        }
    }

    /// Create the chat channel entity for `channel` unless it exists; returns
    /// whether it was created.
    fn ensure_chat_channel(&mut self, channel: &str, cx: &mut Context<Self>) -> bool {
        if self.chat_backend.read(cx).get_channel(channel).is_some() {
            return false;
        }
        self.chat_backend.update(cx, |backend, cx| {
            backend.get_or_create_channel(channel, cx);
        });
        true
    }

    /// Put the pre-chooser channel back on a split whose chooser was dismissed.
    pub fn restore_pending_channel_change(&mut self, split_id: super::SplitId) -> bool {
        let Some(previous) = self.pending_channel_changes.remove(&split_id) else {
            return false;
        };
        self.tabs_mut().iter_mut().any(|tab| {
            tab.split_path_for_id(split_id)
                .is_some_and(|path| tab.set_channel_at_path(&path, previous.clone()))
        })
    }

    /// Drop open-chooser entries whose split no longer exists and return the
    /// channels they were holding.
    pub fn prune_pending_channel_changes(&mut self) -> Vec<String> {
        if self.pending_channel_changes.is_empty() {
            return Vec::new();
        }
        let tabs = &self.tabs;
        let mut removed = Vec::new();
        self.pending_channel_changes.retain(|split_id, previous| {
            let live = tabs
                .iter()
                .any(|tab| tab.split_path_for_id(*split_id).is_some());
            if !live {
                removed.push(previous.clone());
            }
            live
        });
        removed
    }

    /// Tabs as they should be persisted: a split with an open chooser is saved
    /// with the channel it still shows underneath.
    pub fn tabs_for_persistence(&self) -> Arc<Vec<TabState>> {
        if self.pending_channel_changes.is_empty() {
            return self.tabs.clone();
        }
        let mut tabs = self.tabs.as_ref().clone();
        for (split_id, previous) in &self.pending_channel_changes {
            for tab in &mut tabs {
                if let Some(path) = tab.split_path_for_id(*split_id) {
                    tab.set_channel_at_path(&path, previous.clone());
                }
            }
        }
        Arc::new(tabs)
    }

    /// Get mutable access to tabs (uses Arc::make_mut for copy-on-write)
    /// This is O(1) if there's only one Arc reference, O(n) otherwise
    #[inline]
    pub fn tabs_mut(&mut self) -> &mut Vec<TabState> {
        Arc::make_mut(&mut self.tabs)
    }

    /// Get the active tab
    pub fn active_tab(&self) -> &TabState {
        self.tabs.get(self.active_tab_idx).unwrap_or_else(|| {
            crate::log_ui!(
                error,
                "active_tab_idx {} out of bounds for {} tabs, using first tab",
                self.active_tab_idx,
                self.tabs.len()
            );
            &self.tabs[0]
        })
    }

    /// Get the active tab mutably
    pub fn active_tab_mut(&mut self) -> &mut TabState {
        let idx = if self.active_tab_idx < self.tabs.len() {
            self.active_tab_idx
        } else {
            crate::log_ui!(
                error,
                "active_tab_idx {} out of bounds for {} tabs, using first tab",
                self.active_tab_idx,
                self.tabs.len()
            );
            0
        };
        &mut self.tabs_mut()[idx]
    }

    pub fn tab_visibility_mode(&self) -> TabVisibility {
        self.tabs
            .get(self.active_tab_idx)
            .or_else(|| self.tabs.first())
            .map(|tab| tab.visibility.clone())
            .unwrap_or_default()
    }

    pub fn should_hide_tab_bar(&self) -> bool {
        matches!(self.tab_visibility_mode(), TabVisibility::Hidden)
    }

    pub fn set_tab_visibility_mode(&mut self, visibility: TabVisibility) -> bool {
        let tabs = self.tabs_mut();
        let mut changed = false;
        for tab in tabs.iter_mut() {
            if tab.visibility != visibility {
                tab.visibility = visibility.clone();
                changed = true;
            }
        }
        changed
    }

    pub fn toggle_live_tabs_only(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        let next_visibility = match self.tab_visibility_mode() {
            TabVisibility::OnlyLive => TabVisibility::ShowAll,
            _ => TabVisibility::OnlyLive,
        };

        let changed = self.set_tab_visibility_mode(next_visibility);
        let active_changed = self.ensure_active_tab_visible(cx);
        changed || active_changed
    }

    pub fn toggle_tab_bar_visibility(&mut self) -> bool {
        let next_visibility = match self.tab_visibility_mode() {
            TabVisibility::Hidden => TabVisibility::ShowAll,
            _ => TabVisibility::Hidden,
        };

        self.set_tab_visibility_mode(next_visibility)
    }

    fn tab_has_live_channels(&self, idx: usize) -> bool {
        self.tabs
            .get(idx)
            .map(|tab| {
                tab.channels().into_iter().any(|channel| {
                    self.stream_info
                        .get(&channel)
                        .and_then(|info| info.as_ref())
                        .is_some_and(|info| info.is_live)
                })
            })
            .unwrap_or(false)
    }

    fn live_tab_indices(&self) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&idx| self.tab_has_live_channels(idx))
            .collect()
    }

    pub fn visible_tab_indices(&self) -> Vec<usize> {
        if self.tabs.is_empty() {
            return Vec::new();
        }

        match self.tab_visibility_mode() {
            TabVisibility::ShowAll => (0..self.tabs.len()).collect(),
            TabVisibility::Hidden => Vec::new(),
            TabVisibility::OnlyLive => {
                let active_idx = self.active_tab_idx.min(self.tabs.len() - 1);
                let mut visible = self.live_tab_indices();
                if let Err(insert_at) = visible.binary_search(&active_idx) {
                    visible.insert(insert_at, active_idx);
                }
                visible
            }
        }
    }

    fn navigable_tab_indices(&self) -> Vec<usize> {
        if self.tabs.is_empty() {
            return Vec::new();
        }

        match self.tab_visibility_mode() {
            TabVisibility::OnlyLive => self.visible_tab_indices(),
            TabVisibility::ShowAll | TabVisibility::Hidden => (0..self.tabs.len()).collect(),
        }
    }

    pub fn ensure_active_tab_visible(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        if !matches!(self.tab_visibility_mode(), TabVisibility::OnlyLive) {
            return false;
        }

        let visible = self.visible_tab_indices();
        if visible.is_empty() || visible.contains(&self.active_tab_idx) {
            return false;
        }

        self.switch_tab(visible[0], cx);
        true
    }

    pub fn switch_tab_relative(&mut self, delta: isize, cx: &mut gpui::Context<Self>) -> bool {
        let visible = self.navigable_tab_indices();
        if visible.len() <= 1 {
            return false;
        }

        let current_pos = visible
            .iter()
            .position(|&idx| idx == self.active_tab_idx)
            .unwrap_or(0);
        let len = visible.len() as isize;
        let next_pos = (current_pos as isize + delta).rem_euclid(len) as usize;
        let next_idx = visible[next_pos];
        if next_idx == self.active_tab_idx {
            return false;
        }

        self.switch_tab(next_idx, cx);
        true
    }

    pub fn switch_tab_by_number(&mut self, number: usize, cx: &mut gpui::Context<Self>) -> bool {
        if number == 0 {
            return false;
        }

        let visible = self.navigable_tab_indices();
        let target_idx = if number == 9 {
            visible.last().copied()
        } else {
            visible.get(number - 1).copied()
        };
        let Some(target_idx) = target_idx else {
            return false;
        };
        if target_idx == self.active_tab_idx {
            return false;
        }

        self.switch_tab(target_idx, cx);
        true
    }

    /// Switch to a different tab by index.
    pub fn switch_tab(&mut self, idx: usize, cx: &mut gpui::Context<Self>) {
        let old_idx = self.active_tab_idx;
        let activating = idx != self.active_tab_idx;
        let Some(active_channels) = self.activate_tab(idx, activating) else {
            return;
        };

        for channel in active_channels {
            self.ensure_chat_channel(&channel, cx);
        }

        let adjacent_channels: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(tab_idx, _)| is_adjacent_tab(*tab_idx, idx))
            .flat_map(|(_, tab)| tab.channels())
            .collect();
        for channel in adjacent_channels {
            self.ensure_chat_channel(&channel, cx);
        }

        self.rebalance_message_retention(cx);

        match self.main_window {
            Some(main_window) => cx.defer(move |cx| {
                let _ = main_window.update(cx, |_, window, _| window.refresh());
            }),
            None => cx.refresh_windows(),
        }

        crate::log_ui!(
            debug,
            "Tab switch {} -> {}, queued on-demand + prefetch",
            old_idx,
            idx
        );
    }

    /// Synchronize selection state and, when the selected tab's identity
    /// changed, request the active and neighboring channels. Structural close
    /// operations use this too, because an index can stay the same while its
    /// tab changes after a removal.
    fn activate_tab(&mut self, idx: usize, activating: bool) -> Option<Vec<String>> {
        if idx >= self.tabs.len() {
            return None;
        }

        {
            let tabs = self.tabs_mut();
            for (tab_idx, tab) in tabs.iter_mut().enumerate() {
                tab.is_selected = tab_idx == idx;
            }
            tabs[idx].has_unread = false;
            tabs[idx].highlight_state = TabHighlightState::None;
        }
        self.active_tab_idx = idx;

        if !activating {
            return None;
        }

        let active_channels = self.tabs[idx].channels();
        let lifecycle = backend::channel_lifecycle::get_lifecycle_manager();
        for channel in &active_channels {
            if !lifecycle.is_connected(channel) {
                if !backend::whispers::is_whispers_channel(channel) {
                    backend::preload_channel_emotes_predictive(channel);
                }
                request_channel_connection(
                    channel.clone(),
                    backend::channel_lifecycle::LoadPriority::Immediate,
                );
            }
        }

        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            if is_adjacent_tab(tab_idx, idx) {
                for channel in tab.channels() {
                    if !lifecycle.is_connected(&channel) {
                        request_channel_connection(
                            channel,
                            backend::channel_lifecycle::LoadPriority::Prefetch,
                        );
                    }
                }
            }
        }

        Some(active_channels)
    }

    /// Drain per-channel activity signals and escalate inactive tabs' highlight
    /// state (tab new-message/mention coloring).
    ///
    /// Runs from the app's periodic UI housekeeping task. Returns whether any
    /// tab changed so the caller can trigger a repaint. The active tab is never
    /// marked; NewMessage requires the tab's `mark_unread_on_new` preference and
    /// Highlighted requires `highlights_enabled`.
    pub fn update_tab_highlight_states(&mut self) -> bool {
        let activity = crate::backend::irc::drain_channel_tab_activity();
        if activity.is_empty() {
            return false;
        }

        let active_idx = self.active_tab_idx;
        let mut changed = false;
        let tabs = self.tabs_mut();
        for (channel, flags) in activity {
            for (idx, tab) in tabs.iter_mut().enumerate() {
                if idx == active_idx {
                    continue;
                }
                let matches_channel = tab
                    .channels()
                    .iter()
                    .any(|tab_channel| tab_channel.eq_ignore_ascii_case(&channel));
                if !matches_channel {
                    continue;
                }

                let target = if flags & crate::backend::irc::TAB_ACTIVITY_MENTION != 0
                    && tab.highlights_enabled
                {
                    TabHighlightState::Highlighted
                } else if flags & crate::backend::irc::TAB_ACTIVITY_MESSAGE != 0
                    && tab.mark_unread_on_new
                {
                    TabHighlightState::NewMessage
                } else {
                    continue;
                };

                if tab.highlight_state.escalate(target) {
                    tab.has_unread = true;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Add a new tab and return its index
    pub fn add_tab(&mut self, tab: TabState, cx: &mut gpui::Context<Self>) -> usize {
        let idx = self.tabs.len();
        self.tabs_mut().push(tab);

        let mut all_channels = Vec::new();

        for channel in self.tabs[idx].channels() {
            if !backend::whispers::is_whispers_channel(&channel) {
                all_channels.push(channel.clone());
            }

            if self.ensure_chat_channel(&channel, cx) {
                if !backend::whispers::is_whispers_channel(&channel) {
                    backend::preload_channel_emotes_predictive(&channel);
                }

                request_channel_connection(
                    channel.clone(),
                    backend::channel_lifecycle::LoadPriority::Immediate,
                );
            }
        }

        if !all_channels.is_empty() {
            crate::cache::loader::trigger_channels_preload_batch(all_channels);
        }

        idx
    }

    /// Remove a tab by index, returns the removed tab if successful
    pub fn remove_tab(&mut self, idx: usize) -> Option<TabState> {
        if self.tabs.len() <= 1 {
            return None;
        }
        if idx >= self.tabs.len() {
            return None;
        }

        let active_tab_id = self.active_tab().id.clone();
        let removed = self.tabs_mut().remove(idx);
        self.record_closed_tabs(vec![(idx, removed.clone())]);

        if self.active_tab_idx >= self.tabs.len() {
            self.active_tab_idx = self.tabs.len() - 1;
        } else if self.active_tab_idx > idx {
            self.active_tab_idx -= 1;
        }

        let active_idx = self.active_tab_idx;
        let activating = self.tabs[active_idx].id != active_tab_id;
        let _ = self.activate_tab(active_idx, activating);
        self.prune_pending_channel_changes();

        Some(removed)
    }

    fn record_closed_tabs(&mut self, mut batch: Vec<(usize, TabState)>) {
        if batch.is_empty() {
            return;
        }
        for (_, tab) in &mut batch {
            tab.is_selected = false;
            for (split_id, previous) in &self.pending_channel_changes {
                if let Some(path) = tab.split_path_for_id(*split_id) {
                    tab.set_channel_at_path(&path, previous.clone());
                }
            }
        }
        if self.closed_tabs.len() >= MAX_CLOSED_TAB_BATCHES {
            self.closed_tabs.pop_front();
        }
        self.closed_tab_generation = self.closed_tab_generation.wrapping_add(1);
        self.closed_tabs
            .push_back((self.closed_tab_generation, batch));
    }

    /// Bumps on every recorded tab close; the newest close action carries it.
    pub fn closed_tab_generation(&self) -> u64 {
        self.closed_tab_generation
    }

    /// Number of tabs the next reopen brings back.
    pub fn last_closed_tab_count(&self) -> usize {
        self.closed_tabs.back().map_or(0, |(_, batch)| batch.len())
    }

    /// Split ids of closed tabs that can still be reopened.
    pub fn closed_tab_split_ids(&self) -> impl Iterator<Item = super::SplitId> + '_ {
        self.closed_tabs
            .iter()
            .flat_map(|(_, batch)| batch.iter())
            .flat_map(|(_, tab)| tab.layout.collect_split_ids())
    }

    /// Reopen the most recent close action's tabs at their old positions through
    /// the normal add-tab lifecycle. Returns how many tabs came back.
    pub fn reopen_closed_tabs(&mut self, cx: &mut gpui::Context<Self>) -> usize {
        let Some((_, batch)) = self.closed_tabs.pop_back() else {
            return 0;
        };
        self.reopen_tab_batch(batch, cx)
    }

    /// Reopen the close action recorded at `generation`, if it is still stored.
    pub fn reopen_closed_tab_batch(
        &mut self,
        generation: u64,
        cx: &mut gpui::Context<Self>,
    ) -> usize {
        let Some(pos) = self
            .closed_tabs
            .iter()
            .position(|(batch_generation, _)| *batch_generation == generation)
        else {
            return 0;
        };
        let Some((_, batch)) = self.closed_tabs.remove(pos) else {
            return 0;
        };
        self.reopen_tab_batch(batch, cx)
    }

    fn reopen_tab_batch(
        &mut self,
        batch: Vec<(usize, TabState)>,
        cx: &mut gpui::Context<Self>,
    ) -> usize {
        let count = batch.len();
        let mut first_idx = None;
        for (old_idx, tab) in batch {
            let added = self.add_tab(tab, cx);
            let target = old_idx.min(added);
            self.move_tab(added, target);
            first_idx.get_or_insert(target);
        }
        if count == 1
            && let Some(idx) = first_idx
        {
            self.switch_tab(idx, cx);
        }
        count
    }

    /// Move a tab from one position to another (for drag reorder)
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }

        let tabs = self.tabs_mut();
        let tab = tabs.remove(from);
        tabs.insert(to, tab);

        if self.active_tab_idx == from {
            self.active_tab_idx = to;
        } else if from < self.active_tab_idx && to >= self.active_tab_idx {
            self.active_tab_idx -= 1;
        } else if from > self.active_tab_idx && to <= self.active_tab_idx {
            self.active_tab_idx += 1;
        }
    }

    /// Duplicate a tab and insert after the original
    pub fn duplicate_tab(&mut self, idx: usize) -> Option<usize> {
        if idx >= self.tabs.len() {
            return None;
        }

        let duplicate = self.tabs[idx].duplicate();
        let new_idx = idx + 1;
        self.tabs_mut().insert(new_idx, duplicate);
        if idx < self.active_tab_idx {
            self.active_tab_idx += 1;
        }

        Some(new_idx)
    }

    /// Close all tabs except the specified one
    pub fn close_other_tabs(&mut self, keep_idx: usize) {
        if keep_idx >= self.tabs.len() {
            return;
        }

        let active_tab_id = self.active_tab().id.clone();
        let tabs = self.tabs_mut();
        let keep = tabs.remove(keep_idx);
        let closed: Vec<(usize, TabState)> = tabs
            .drain(..)
            .enumerate()
            .map(|(idx, tab)| (if idx < keep_idx { idx } else { idx + 1 }, tab))
            .collect();
        tabs.push(keep);
        self.record_closed_tabs(closed);
        self.active_tab_idx = 0;
        let activating = self.tabs[0].id != active_tab_id;
        let _ = self.activate_tab(0, activating);
        self.prune_pending_channel_changes();
    }

    /// Append several tabs at once and connect them with the same tiers as
    /// startup: the tab that is (or will be) active connects now, its
    /// neighbours prefetch, and the rest wait in the background queue.
    pub fn add_tabs_with_startup_priority(
        &mut self,
        tabs: Vec<TabState>,
        active_tab_id: Option<&str>,
    ) {
        let first_new = self.tabs.len();
        self.tabs_mut().extend(tabs);
        if crate::bench::network_lanes_suppressed() {
            return;
        }
        let active_idx = active_tab_id
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(self.active_tab_idx);
        let lifecycle = backend::channel_lifecycle::get_lifecycle_manager();
        let mut requested = HashSet::new();
        let mut asset_channels = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate().skip(first_new) {
            let priority = tab_load_priority(idx, active_idx);
            for channel in tab.channels() {
                if lifecycle.is_connected(&channel) || !requested.insert(channel.clone()) {
                    continue;
                }
                if priority != backend::channel_lifecycle::LoadPriority::Background
                    && !backend::whispers::is_whispers_channel(&channel)
                {
                    asset_channels.push(channel.clone());
                }
                request_channel_connection(channel, priority);
            }
        }
        if !asset_channels.is_empty() {
            crate::cache::loader::trigger_channels_preload_batch(asset_channels);
        }
    }

    /// Close the tabs with these ids as one reopenable batch. At least one tab
    /// must stay open; returns how many closed.
    pub fn close_tabs_with_ids(&mut self, ids: &HashSet<String>) -> usize {
        let closing = self.tabs.iter().filter(|tab| ids.contains(&tab.id)).count();
        if closing == 0 || closing >= self.tabs.len() {
            return 0;
        }

        let active_tab_id = self.active_tab().id.clone();
        let mut closed = Vec::with_capacity(closing);
        let mut kept = Vec::with_capacity(self.tabs.len() - closing);
        for (idx, tab) in std::mem::take(self.tabs_mut()).into_iter().enumerate() {
            if ids.contains(&tab.id) {
                closed.push((idx, tab));
            } else {
                kept.push(tab);
            }
        }
        *self.tabs_mut() = kept;
        self.record_closed_tabs(closed);
        self.active_tab_idx = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_tab_id)
            .unwrap_or(0);
        let active_idx = self.active_tab_idx;
        let activating = self.tabs[active_idx].id != active_tab_id;
        let _ = self.activate_tab(active_idx, activating);
        self.prune_pending_channel_changes();
        closing
    }

    /// Close all tabs to the right of the specified index
    pub fn close_tabs_to_right(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }

        let active_tab_id = self.active_tab().id.clone();
        let closed: Vec<(usize, TabState)> = self
            .tabs_mut()
            .drain(idx + 1..)
            .enumerate()
            .map(|(offset, tab)| (idx + 1 + offset, tab))
            .collect();
        self.record_closed_tabs(closed);
        if self.active_tab_idx >= self.tabs.len() {
            self.active_tab_idx = self.tabs.len() - 1;
        }
        let active_idx = self.active_tab_idx;
        let activating = self.tabs[active_idx].id != active_tab_id;
        let _ = self.activate_tab(active_idx, activating);
        self.prune_pending_channel_changes();
    }

    /// Sort tabs alphabetically by display title
    pub fn sort_tabs_alphabetically(&mut self) {
        let current_id = self.tabs[self.active_tab_idx].id.clone();
        self.tabs_mut().sort_by_cached_key(|tab| {
            let title = tab.display_title();
            title.strip_prefix('#').unwrap_or(&title).to_lowercase()
        });

        self.active_tab_idx = self
            .tabs
            .iter()
            .position(|t| t.id == current_id)
            .unwrap_or(0);
    }

    /// Add a new channel to the active tab's layout
    pub fn add_channel<T: 'static>(&mut self, channel: String, cx: &mut Context<T>) {
        use std::sync::Arc;
        let normalized = channel.trim().to_lowercase();
        if normalized.is_empty() {
            return;
        }

        let tab = self.active_tab_mut();
        let existing_channels = tab.channels();

        Arc::make_mut(&mut tab.layout).add_channel_to_layout(normalized.clone());

        if !existing_channels.contains(&normalized) {
            backend::connect_channel(&normalized);
        }

        cx.notify();
    }

    /// Open the global activity rail at a specific section.
    ///
    /// Mention and whisper snapshots are refreshed synchronously so the first
    /// frame never shows the polling loop's stale closed-rail cache.
    pub fn open_activity<T: 'static>(
        &mut self,
        section: ActivitySection,
        event_filter: crate::backend::ui_actions::NotificationFilter,
        cx: &mut Context<T>,
    ) {
        self.activity_panel.open = true;
        self.activity_panel.section = section;
        self.activity_panel.event_filter = event_filter;
        self.refresh_sidebar_caches();
        cx.notify();
    }

    /// Toggle the rail from a section-specific action (a hotkey or command).
    ///
    /// Invoking the action for the visible section closes the rail; invoking a
    /// different action switches sections without creating another sidebar.
    pub fn toggle_activity_section<T: 'static>(
        &mut self,
        section: ActivitySection,
        event_filter: crate::backend::ui_actions::NotificationFilter,
        cx: &mut Context<T>,
    ) {
        let same_target = self.activity_panel.open
            && self.activity_panel.section == section
            && (section != ActivitySection::Events
                || self.activity_panel.event_filter == event_filter);
        if same_target {
            self.activity_panel.open = false;
            cx.notify();
        } else {
            self.open_activity(section, event_filter, cx);
        }
    }

    /// Toggle the one activity rail while preserving its last selected section.
    pub fn toggle_activity_panel<T: 'static>(&mut self, cx: &mut Context<T>) {
        if self.activity_panel.open {
            self.activity_panel.open = false;
        } else {
            self.activity_panel.open = true;
            self.refresh_sidebar_caches();
        }
        cx.notify();
    }

    pub fn close_activity<T: 'static>(&mut self, cx: &mut Context<T>) {
        if self.activity_panel.open {
            self.activity_panel.open = false;
            cx.notify();
        }
    }

    pub fn activity_section_open(&self, section: ActivitySection) -> bool {
        self.activity_panel.open && self.activity_panel.section == section
    }

    /// Select a section inside an already-open rail without toggle semantics.
    pub fn select_activity_section<T: 'static>(
        &mut self,
        section: ActivitySection,
        cx: &mut Context<T>,
    ) {
        let filter = if section == ActivitySection::Events {
            crate::backend::ui_actions::NotificationFilter::All
        } else {
            self.activity_panel.event_filter
        };
        self.open_activity(section, filter, cx);
    }

    /// Whether the one global activity rail currently consumes layout width.
    pub fn activity_open(&self) -> bool {
        self.activity_panel.open
    }

    /// Refresh activity data immediately after a backend mutation.
    pub fn refresh_activity<T: 'static>(&mut self, cx: &mut Context<T>) {
        self.refresh_sidebar_caches();
        cx.notify();
    }

    /// Refresh cached sidebar data. Call from the polling loop, not render; the
    /// mentions come from the global mentions buffer, not a scan of every channel.
    pub fn refresh_sidebar_caches(&mut self) {
        self.cached_mentions = Arc::new(backend::get_global_mentions(100));

        self.cached_whispers = Arc::new(backend::get_whisper_threads());
    }

    pub fn apply_stream_info_updates(
        &mut self,
        updates: Vec<(String, Option<StreamInfo>)>,
    ) -> usize {
        let mut changed = 0usize;
        let active_channels = self.all_active_channels();
        for (channel, info) in updates {
            let canonical_channel = if self.stream_info.contains_key(&channel) {
                channel.clone()
            } else if let Some(existing) = self
                .stream_info
                .keys()
                .find(|existing| existing.eq_ignore_ascii_case(&channel))
            {
                existing.clone()
            } else if let Some(active) = active_channels
                .iter()
                .find(|active| active.eq_ignore_ascii_case(&channel))
            {
                active.clone()
            } else {
                channel
            };

            if (info.is_some() || !self.stream_info.contains_key(&canonical_channel))
                && self.stream_info.get(&canonical_channel) != Some(&info)
            {
                self.stream_info.retain(|key, _| {
                    key == &canonical_channel || !key.eq_ignore_ascii_case(&canonical_channel)
                });
                self.stream_info.insert(canonical_channel, info);
                changed += 1;
            }
        }
        changed
    }

    /// Get the set of all active channel names across all tabs
    pub fn all_active_channels(&self) -> HashSet<String> {
        let mut channels: HashSet<String> =
            self.tabs.iter().flat_map(|tab| tab.channels()).collect();
        channels.extend(backend::settings::get_open_chat_popup_channels());
        channels.extend(
            self.pending_channel_changes
                .values()
                .filter_map(|channel| backend::browser_watching::resolve_layout_channel(channel)),
        );
        channels
    }

    /// Rebalance backend and UI message buffers around the selected tab.
    ///
    /// Echo keeps a reactive UI snapshot per channel beside the backend buffer,
    /// so selected-tab retention must be enforced in both storage layers to
    /// actually lower resident memory.
    pub fn rebalance_message_retention(&mut self, cx: &mut gpui::Context<Self>) {
        let mut active_channels: Vec<String> = self
            .active_tab()
            .channels()
            .into_iter()
            .map(|channel| channel.to_lowercase())
            .collect();
        active_channels.extend(backend::settings::get_open_chat_popup_channels());
        active_channels.sort_unstable();
        active_channels.dedup();
        let active_set: HashSet<String> = active_channels.iter().cloned().collect();

        backend::trim_inactive_channels(&active_channels);
        let ui_trimmed = self.chat_backend.update(cx, |backend, cx| {
            backend.rebalance_message_retention(&active_set, cx)
        });

        if ui_trimmed > 0 {
            crate::log_ui!(
                debug,
                ui_trimmed,
                active_channels = ?active_set,
                "Rebalanced UI message retention"
            );
        }
    }

    /// Release a channel that just left the layout. Chat states are swept by
    /// `cleanup_orphaned_states`, but a channel whose tab was never shown this
    /// session has no chat state while its startup connection request has
    /// already joined it (or is still queued), so it is parted here.
    pub fn release_closed_channel(&mut self, channel: &str, cx: &mut gpui::Context<Self>) {
        let key = channel.trim_start_matches('#').to_lowercase();
        let has_chat_state = self.chat_backend.read(cx).channels.contains_key(&key);
        self.cleanup_orphaned_states(cx);
        if has_chat_state
            || key.is_empty()
            || super::is_placeholder(&key)
            || backend::browser_watching::is_watching_channel(&key)
            || self
                .all_active_channels()
                .iter()
                .any(|active| active.eq_ignore_ascii_case(&key))
        {
            return;
        }
        self.chat_backend.update(cx, |backend, cx| {
            backend.remove_channel(&key, cx);
        });
    }

    /// Clean up orphaned chat states and stream info.
    /// Call this periodically or after removing tabs/channels to prevent memory leaks
    pub fn cleanup_orphaned_states(&mut self, cx: &mut gpui::Context<Self>) {
        let active_channels: HashSet<String> = self
            .all_active_channels()
            .into_iter()
            .map(|c| c.to_lowercase())
            .collect();

        let removed_channels: Vec<String> = self
            .chat_backend
            .read(cx)
            .channels
            .keys()
            .filter(|channel| !active_channels.contains(*channel))
            .cloned()
            .collect();

        for channel in &removed_channels {
            self.chat_backend.update(cx, |backend, cx| {
                backend.remove_channel(channel, cx);
            });
        }

        let before_stream = self.stream_info.len();
        self.stream_info
            .retain(|channel, _| active_channels.contains(&channel.to_lowercase()));

        let removed_stream = before_stream - self.stream_info.len();
        if !removed_channels.is_empty() || removed_stream > 0 {
            crate::log_ui!(
                debug,
                removed_chat_entities = removed_channels.len(),
                removed_stream_info = removed_stream,
                active_channels = ?active_channels,
                "cleanup_orphaned_states"
            );
        }

        backend::bridge::cleanup_emote_category_cache(&active_channels);
    }
}
