//! Entity-based chat backend for reactive state management
//!
//! Uses GPUI entities for channel state and emits app-level channel lifecycle events.
//! Message updates stay split-local through each `ChatView`'s `ChatState` subscription.

use crate::backend;
use crate::backend::observers::ChannelEvent;
use crate::state::{
    ChatState, ChatStateObserver, VersionRegistry,
    chat_observer_recovery::{ObserverDrainAction, observer_drain_action},
};
use gpui::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Events emitted by ChatBackend for app-level reactions
#[derive(Clone, Debug)]
pub enum ChatBackendEvent {
    /// A channel was added
    ChannelAdded { channel: String },
    /// A channel was removed
    ChannelRemoved { channel: String },
}

/// Backend entity that manages all chat channel states reactively
pub struct ChatBackend {
    /// Channel entities (one per connected channel)
    /// Using Entity<ChatState> instead of plain ChatState for reactivity
    pub channels: HashMap<String, Entity<ChatState>>,
    /// Observers for each channel (kept alive)
    pub observers: HashMap<String, Arc<ChatStateObserver>>,
    /// Last seen channel emote versions for detecting missed retokenization events.
    /// Used in has_pending_events() to check for version mismatches without needing a Context.
    last_seen_channel_versions: HashMap<String, u64>,
    /// Last seen global emote version for detecting missed retokenization events.
    last_seen_global_emote_version: u64,
    /// Last seen user emote version for detecting missed retokenization events.
    last_seen_user_emote_version: u64,
}

impl EventEmitter<ChatBackendEvent> for ChatBackend {}

#[derive(Default)]
struct ObserverProcessResult {
    count: usize,
    changed: bool,
    refreshed_from_backend: bool,
    oldest_wait: Option<std::time::Duration>,
}

impl ChatBackend {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            observers: HashMap::new(),
            last_seen_channel_versions: HashMap::new(),
            last_seen_global_emote_version: 0,
            last_seen_user_emote_version: 0,
        }
    }

    /// Get or create a channel entity
    /// This is the main entry point for accessing channel state
    pub fn get_or_create_channel(
        &mut self,
        channel: &str,
        cx: &mut Context<Self>,
    ) -> Entity<ChatState> {
        let channel_lower = channel.to_lowercase();

        if let Some(entity) = self.channels.get(&channel_lower) {
            return entity.clone();
        }

        let channel_entity = cx.new(|_cx| ChatState::new(channel_lower.clone()));

        if !self.observers.contains_key(&channel_lower) {
            self.create_observer_for_channel(channel_lower.clone(), cx);
        }

        let existing_messages = backend::get_messages(
            &channel_lower,
            crate::state::message_retention::active_channel_capacity() as u32,
        );
        if !existing_messages.is_empty() {
            crate::log_irc!(
                debug,
                "Initializing '{}' with {} existing messages",
                channel_lower,
                existing_messages.len()
            );
            channel_entity.update(cx, |state, _cx| {
                state.replace_snapshot_from_backend(existing_messages);
            });
        }

        self.channels
            .insert(channel_lower.clone(), channel_entity.clone());

        if !crate::backend::whispers::is_whispers_channel(&channel_lower) {
            let has_emotes = crate::backend::emotes::channel_has_lookup_entry(&channel_lower);
            if !has_emotes {
                if crate::backend::emotes::EMOTE_CACHE.channel_emote_count(&channel_lower) > 0 {
                    let heal_channel = channel_lower.clone();
                    crate::backend::bridge::get_runtime().spawn(async move {
                        crate::backend::emotes::rebuild_emote_indices(&heal_channel).await;
                    });
                }
                crate::log_emotes!(
                    info,
                    "get_or_create_channel: No emotes loaded for '{}', triggering predictive preload",
                    channel_lower
                );
                crate::backend::bridge::preload_channel_emotes_predictive(&channel_lower);
                crate::backend::emotes::spawn_channel_open_emote_watchdog(&channel_lower);
            }
        }

        cx.emit(ChatBackendEvent::ChannelAdded {
            channel: channel_lower,
        });
        cx.notify();

        channel_entity
    }

    /// Get channel entity if it exists
    pub fn get_channel(&self, channel: &str) -> Option<Entity<ChatState>> {
        self.channels.get(&channel.to_lowercase()).cloned()
    }

    /// Recover active channel entities whose reactive state missed the backend history load.
    pub fn recover_empty_channels_from_backend(
        &mut self,
        channels: &[String],
        cx: &mut Context<Self>,
    ) -> bool {
        let started_at = std::time::Instant::now();
        let mut recovered_channels = 0usize;
        let mut recovered_messages = 0usize;

        for channel in channels {
            let channel_lower = channel.to_lowercase();
            let Some(entity) = self.channels.get(&channel_lower).cloned() else {
                continue;
            };

            if !entity.read(cx).messages.is_empty() {
                continue;
            }

            let backend_msgs = backend::get_messages(
                &channel_lower,
                crate::state::message_retention::active_channel_capacity() as u32,
            );
            if backend_msgs.is_empty() {
                continue;
            }

            let message_count = backend_msgs.len();
            crate::log_irc!(
                info,
                "Recovering {} messages for '{}' from CHAT_MESSAGES (observer missed)",
                message_count,
                channel_lower
            );
            let changed = entity.update(cx, |chat_state, cx| {
                chat_state.replace_messages_reactive(backend_msgs, cx)
            });
            if changed {
                recovered_channels += 1;
                recovered_messages += message_count;
            }
        }

        if recovered_channels > 0 {
            crate::backend::metrics::record_emote_stage(
                "chat_backend.recover_empty_channels",
                started_at.elapsed(),
                recovered_messages,
            );
            crate::backend::metrics::record_ui_notify_source("chat_backend.recover_empty_channels");
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Create and register an observer for a channel
    /// The observer will trigger entity updates when backend events occur
    fn create_observer_for_channel(&mut self, channel: String, _cx: &mut Context<Self>) {
        let observer = crate::state::chat_observer::ChatStateObserver::new(channel.clone());

        observer.set_notify_callback(|| {
            crate::managers::frame_rate_controller::wakeup();
            crate::managers::frame_rate_controller::record_activity();
        });

        backend::observers::get_observer_registry().subscribe(&channel, observer.clone());

        self.observers.insert(channel, observer);
    }

    /// Process pending events from all observers
    /// Call this from the UI thread to apply backend updates to entities
    /// Returns true if any updates occurred (needs repaint)
    ///
    /// NOTE: This is still needed because IRC messages arrive on background threads.
    /// The observer queues events, and this method processes them on the UI thread.
    /// In the future, we could use AsyncApp::update_global to eliminate this.
    pub fn process_observer_events(&mut self, cx: &mut Context<Self>) -> bool {
        let process_started = std::time::Instant::now();
        let mut any_updates = false;
        let mut refreshed_channels = HashSet::new();

        let channels_with_events: Vec<(String, Arc<ChatStateObserver>)> = self
            .observers
            .iter()
            .filter(|(_, obs)| obs.has_pending_events())
            .map(|(ch, obs)| (ch.clone(), obs.clone()))
            .collect();

        let mut drained_events = 0usize;
        let mut oldest_wait = std::time::Duration::ZERO;
        for (channel, observer) in &channels_with_events {
            let channel_entity = self.get_or_create_channel(channel, cx);
            let mut result = ObserverProcessResult::default();

            channel_entity.update(cx, |state, cx| {
                result = Self::process_observer_events_reactive(state, observer, cx);
                if result.count > 0 {
                    crate::log_irc!(
                        trace,
                        "Processed {} events for '{}' (reactive)",
                        result.count,
                        channel
                    );
                }
            });
            if result.refreshed_from_backend {
                let fresh_channel_version = VersionRegistry::channel_emote_version(channel);
                self.last_seen_channel_versions
                    .insert(channel.clone(), fresh_channel_version);
                refreshed_channels.insert(channel.clone());
                crate::log_emotes!(
                    debug,
                    "Observed retokenization for '{}' in event queue; channel version now {}",
                    channel,
                    fresh_channel_version
                );
            }
            any_updates |= result.changed;
            drained_events += result.count;
            oldest_wait = oldest_wait.max(result.oldest_wait.unwrap_or_default());
        }
        if !channels_with_events.is_empty() {
            crate::backend::metrics::record_emote_stage(
                "chat_backend.observer_drain",
                process_started.elapsed(),
                drained_events,
            );
            crate::backend::metrics::record_emote_stage(
                "chat_backend.observer_queue_wait",
                oldest_wait,
                channels_with_events.len(),
            );
        }

        let current_global_version = VersionRegistry::global_emote_version();
        let current_user_version = VersionRegistry::user_emote_version();
        let global_changed = current_global_version > self.last_seen_global_emote_version;
        let user_changed = current_user_version > self.last_seen_user_emote_version;

        if global_changed || user_changed {
            let log_type = if global_changed && user_changed {
                "global+user"
            } else if global_changed {
                "global"
            } else {
                "user"
            };
            crate::log_emotes!(
                debug,
                "Observed {} emote version change; relying on channel retokenization events for split-local refresh (channels={})",
                log_type,
                self.channels.len()
            );
            self.last_seen_global_emote_version = current_global_version;
            self.last_seen_user_emote_version = current_user_version;
        } else {
            let channels_to_check: Vec<String> = self.channels.keys().cloned().collect();
            for channel in channels_to_check {
                let current_channel_version = VersionRegistry::channel_emote_version(&channel);
                let last_seen = self
                    .last_seen_channel_versions
                    .get(&channel)
                    .copied()
                    .unwrap_or(0);

                if current_channel_version > last_seen
                    && let Some(channel_entity) = self.channels.get(&channel)
                {
                    crate::log_emotes!(
                        info,
                        "Missed retokenization event for '{}' - refreshing (version {} -> {})",
                        channel,
                        last_seen,
                        current_channel_version
                    );
                    let changed = channel_entity
                        .update(cx, |state, cx| state.refresh_from_backend_reactive(cx));
                    self.last_seen_channel_versions
                        .insert(channel.clone(), current_channel_version);
                    if changed {
                        refreshed_channels.insert(channel.clone());
                        any_updates = true;
                    }
                }
            }
        }

        if any_updates {
            crate::managers::frame_rate_controller::record_observer_drain();
            crate::backend::metrics::record_emote_stage(
                "chat_backend.process_observer_events",
                process_started.elapsed(),
                refreshed_channels.len(),
            );
            crate::backend::metrics::record_ui_notify_source(
                "chat_backend.process_observer_events",
            );
        }
        any_updates
    }

    /// Process observer events using reactive methods (emits events + notifies)
    /// PERF: Batches consecutive NewMessage events to reduce cx.notify() calls
    fn process_observer_events_reactive(
        state: &mut ChatState,
        observer: &ChatStateObserver,
        cx: &mut Context<ChatState>,
    ) -> ObserverProcessResult {
        let drained = observer.drain_events();
        let count = drained.events.len();
        let mut result = ObserverProcessResult {
            count,
            changed: false,
            refreshed_from_backend: false,
            oldest_wait: drained.oldest_wait,
        };

        if observer_drain_action(drained.dropped_count)
            == ObserverDrainAction::RefreshFromBackendSnapshot
        {
            crate::log_irc!(
                warn,
                "Observer for '{}' dropped {} events, forcing reactive refresh",
                state.channel,
                drained.dropped_count
            );
            result.changed |= state.refresh_from_backend_reactive(cx);
            result.refreshed_from_backend = true;
            // The snapshot refresh covers message events only. Replay the
            // decoration and room-state events still in this batch, and when a
            // dropped event was a decoration change, invalidate badges so no
            // split keeps layouts shaped with stale badges or paints.
            let mut badges_invalidated = false;
            for event in drained.events {
                match event {
                    ChannelEvent::UserDecorationsChanged {
                        user_login,
                        user_id,
                    } => state.user_decorations_changed(user_login, user_id, cx),
                    ChannelEvent::BadgeDecorationsChanged if !badges_invalidated => {
                        state.badge_decorations_changed(cx);
                        badges_invalidated = true;
                    }
                    ChannelEvent::RoomStateChanged => {
                        result.changed = true;
                        cx.notify();
                    }
                    _ => {}
                }
            }
            if drained.dropped_decoration_event && !badges_invalidated {
                state.badge_decorations_changed(cx);
            }
            return result;
        }

        let mut pending_messages: Vec<crate::backend::ChatMessage> = Vec::new();
        let mut retokenized_refresh_applied = false;

        for event in drained.events {
            match event {
                ChannelEvent::NewMessage(msg) => {
                    pending_messages.push(msg);
                }
                ChannelEvent::HistoryLoaded(msgs) => {
                    if !pending_messages.is_empty() {
                        result.changed |= state
                            .update_messages_reactive(std::mem::take(&mut pending_messages), cx);
                    }
                    let owned = Arc::try_unwrap(msgs).unwrap_or_else(|arc| (*arc).clone());
                    result.changed |= state.replace_messages_reactive(owned, cx);
                }
                ChannelEvent::MessagesRetokenized => {
                    crate::log_emotes!(
                        info,
                        "Processing MessagesRetokenized event for channel '{}'",
                        state.channel
                    );
                    if !pending_messages.is_empty() {
                        result.changed |= state
                            .update_messages_reactive(std::mem::take(&mut pending_messages), cx);
                    }
                    if !retokenized_refresh_applied {
                        result.changed |= state.refresh_from_backend_reactive(cx);
                        retokenized_refresh_applied = true;
                        result.refreshed_from_backend = true;
                    }
                }
                ChannelEvent::UserDecorationsChanged {
                    user_login,
                    user_id,
                } => {
                    state.user_decorations_changed(user_login, user_id, cx);
                }
                ChannelEvent::BadgeDecorationsChanged => {
                    state.badge_decorations_changed(cx);
                }
                ChannelEvent::RoomStateChanged => {
                    crate::log_irc!(
                        debug,
                        "Processing RoomStateChanged event for channel '{}'",
                        state.channel
                    );
                    result.changed = true;
                    cx.notify();
                }
            }
        }

        if !pending_messages.is_empty() {
            result.changed |= state.update_messages_reactive(pending_messages, cx);
        }

        result
    }

    /// Clean up closed channels with proper observer cleanup
    pub fn remove_channel(&mut self, channel: &str, cx: &mut Context<Self>) {
        let channel_lower = channel.to_lowercase();

        if let Some(entity) = self.channels.get(&channel_lower) {
            crate::state::layout_eviction::queue_evicted_ids(
                entity
                    .read(cx)
                    .messages
                    .iter()
                    .map(|message| message.id.clone()),
            );
        }

        self.channels.remove(&channel_lower);
        self.observers.remove(&channel_lower);
        self.last_seen_channel_versions.remove(&channel_lower);
        if !crate::backend::whispers::is_whispers_channel(&channel_lower) {
            crate::backend::irc::leave_channel_immediately(&channel_lower);
            crate::backend::emotes::seventv_eventapi::unsubscribe_channel(&channel_lower);
            crate::backend::eventsub::release_open_channel(&channel_lower);
            crate::backend::emotes::remove_channel_emotes(&channel_lower);
        }
        crate::state::VersionRegistry::remove_channel_emote_version(&channel_lower);

        cx.emit(ChatBackendEvent::ChannelRemoved {
            channel: channel_lower,
        });
        cx.notify();
    }

    /// Rebalance UI-owned message snapshots to match visible-tab retention.
    ///
    /// The backend `CHAT_MESSAGES` buffers are only half of Echo's resident
    /// message memory. Each reactive `ChatState` owns a render snapshot, so
    /// inactive tabs need trimming there too.
    pub fn rebalance_message_retention(
        &mut self,
        active_channels: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> usize {
        let full_capacity = crate::state::message_retention::active_channel_capacity();
        let inactive_capacity = crate::state::message_retention::inactive_channel_capacity();
        let channels: Vec<(String, Entity<ChatState>)> = self
            .channels
            .iter()
            .map(|(channel, entity)| (channel.clone(), entity.clone()))
            .collect();

        let mut trimmed_channels = 0;
        for (channel, entity) in channels {
            let target_capacity = if active_channels.contains(&channel) {
                full_capacity
            } else {
                inactive_capacity
            };

            let changed =
                entity.update(cx, |state, _cx| state.set_message_capacity(target_capacity));

            if changed {
                trimmed_channels += 1;
            }
        }

        trimmed_channels
    }

    /// Check if any channel has pending events (for polling optimization)
    /// Also checks for version mismatches to catch missed retokenization events.
    pub fn has_pending_events(&self) -> bool {
        let observer_count = self.observers.len();
        let has_pending = self.observers.values().any(|obs| obs.has_pending_events());

        if has_pending {
            return true;
        }

        if VersionRegistry::global_emote_version() > self.last_seen_global_emote_version {
            return true;
        }

        if VersionRegistry::user_emote_version() > self.last_seen_user_emote_version {
            return true;
        }

        for channel in self.channels.keys() {
            let current_version = VersionRegistry::channel_emote_version(channel);
            let last_seen = self
                .last_seen_channel_versions
                .get(channel)
                .copied()
                .unwrap_or(0);
            if current_version > last_seen {
                return true;
            }
        }

        if observer_count > 0 {
            static LAST_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = crate::utils::unix_now_secs();
            let last = LAST_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now > last + 5 {
                LAST_LOG.store(now, std::sync::atomic::Ordering::Relaxed);
                crate::log_ui!(
                    debug,
                    "has_pending_events: {} observers, pending=false",
                    observer_count
                );
            }
        }

        false
    }
}

impl Default for ChatBackend {
    fn default() -> Self {
        Self::new()
    }
}
