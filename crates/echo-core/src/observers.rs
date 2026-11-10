//! Channel observer system for push-based state updates.
//!
//! Replaces polling-based version counters with direct notifications when
//! messages arrive, reducing latency from 100ms to <16ms (next frame).
#![allow(clippy::large_enum_variant)] // Size difference is acceptable for event enum

use crate::message::ChatMessage;
use crate::sync::RwLockExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock, Weak};
use tokio::sync::watch;

/// Events that can be notified to channel observers
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    /// New message arrived
    NewMessage(ChatMessage),
    /// History batch loaded
    /// PERF: Arc-wrapped to avoid cloning entire message buffer (500-1000 messages)
    HistoryLoaded(Arc<Vec<ChatMessage>>),
    /// Messages were retokenized (emotes loaded, need to refresh from CHAT_MESSAGES)
    MessagesRetokenized,
    /// User-level decorations changed without changing message slots.
    UserDecorationsChanged {
        user_login: Arc<str>,
        user_id: Option<Arc<str>>,
    },
    /// Channel-scoped badge artwork or assignments changed without changing
    /// any message tokens.
    BadgeDecorationsChanged,
    /// Channel room-state changed (chat modes, slow mode, followers-only, etc.)
    RoomStateChanged,
}

/// Trait for objects that want to observe channel events
pub trait ChannelObserver: Send + Sync {
    fn on_event(&self, channel: &str, event: ChannelEvent);
}

/// Monotonic live-message invalidation epoch.
///
/// This is deliberately a watch publication rather than another queued
/// `ChannelObserver`: secondary windows only need to know that *some* live
/// message arrived, then can read their selected channel's bounded backend
/// tail. A channel name would lose a selected-channel wake when messages for
/// multiple channels coalesce before a receiver runs; the epoch preserves that
/// bounded wake contract without retaining per-message data.
#[derive(Clone, Debug, Default)]
pub struct ChannelMessageUpdate {
    pub epoch: u64,
}

static CHANNEL_MESSAGE_UPDATES: LazyLock<watch::Sender<ChannelMessageUpdate>> =
    LazyLock::new(|| {
        let (sender, _) = watch::channel(ChannelMessageUpdate::default());
        sender
    });

static CHANNEL_MESSAGE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Subscribe to live-message invalidations for secondary channel views.
pub fn subscribe_channel_message_updates() -> watch::Receiver<ChannelMessageUpdate> {
    CHANNEL_MESSAGE_UPDATES.subscribe()
}

fn publish_channel_message_update() {
    let epoch = CHANNEL_MESSAGE_EPOCH
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    CHANNEL_MESSAGE_UPDATES.send_replace(ChannelMessageUpdate { epoch });
}

/// Registry for channel observers using weak references
/// Observers are automatically cleaned up when dropped
pub struct ChannelObserverRegistry {
    /// Map of channel name -> list of observers
    /// Uses Weak to avoid keeping observers alive
    observers: RwLock<HashMap<String, Vec<Weak<dyn ChannelObserver>>>>,
}

impl ChannelObserverRegistry {
    pub fn new() -> Self {
        Self {
            observers: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribe an observer to a channel
    pub fn subscribe(&self, channel: &str, observer: Arc<dyn ChannelObserver>) {
        let mut observers = self.observers.write_or_recover();
        let channel_observers = observers
            .entry(channel.to_lowercase())
            .or_insert_with(Vec::new);

        // Add weak reference to avoid keeping observer alive
        channel_observers.push(Arc::downgrade(&observer));

        crate::log_ui!(
            debug,
            "Subscribed to channel '{}' (total: {})",
            channel,
            channel_observers.len()
        );
    }

    /// Notify all observers of a channel event
    pub fn notify(&self, channel: &str, event: ChannelEvent) {
        let channel_key = channel.to_lowercase();
        if matches!(&event, ChannelEvent::NewMessage(_)) {
            publish_channel_message_update();
        }
        let (upgraded, dropped) = {
            let observers = self.observers.read_or_recover();
            if let Some(channel_observers) = observers.get(&channel_key) {
                let mut upgraded = Vec::with_capacity(channel_observers.len());
                let mut dropped = 0usize;
                for weak_observer in channel_observers {
                    match weak_observer.upgrade() {
                        Some(observer) => {
                            upgraded.push(observer);
                        }
                        _ => {
                            dropped += 1;
                        }
                    }
                }
                (upgraded, dropped)
            } else {
                (Vec::new(), 0)
            }
        };

        if upgraded.is_empty() {
            return;
        }

        let mut pending_event = Some(event);
        for (idx, observer) in upgraded.iter().enumerate() {
            let event_for_observer = if idx + 1 == upgraded.len() {
                // Move the original event on the last callback to avoid one clone.
                pending_event
                    .take()
                    .expect("pending event should exist for final observer")
            } else {
                pending_event
                    .as_ref()
                    .expect("pending event should exist for cloning")
                    .clone()
            };
            observer.on_event(channel, event_for_observer);
        }

        crate::log_ui!(
            trace,
            "Notified {} observers for '{}' ({} dropped)",
            upgraded.len(),
            channel,
            dropped
        );
    }

    /// Notify every channel that currently has observers.
    pub fn notify_all(&self, event: ChannelEvent) {
        let channels: Vec<String> = self.observers.read_or_recover().keys().cloned().collect();
        for channel in channels {
            self.notify(&channel, event.clone());
        }
    }

    pub fn has_live_observers(&self, channel: &str) -> bool {
        self.observers
            .read_or_recover()
            .get(&channel.to_lowercase())
            .is_some_and(|observers| observers.iter().any(|observer| observer.strong_count() > 0))
    }

    /// Number of live observers for a channel
    #[cfg(test)]
    pub fn observer_count(&self, channel: &str) -> usize {
        let observers = self.observers.read_or_recover();
        observers
            .get(&channel.to_lowercase())
            .map(|channel_observers| {
                channel_observers
                    .iter()
                    .filter(|weak| weak.strong_count() > 0)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Clean up dead weak references (called periodically)
    pub fn cleanup_stale_observers(&self) {
        let mut observers = self.observers.write_or_recover();
        let mut total_removed = 0;

        for (channel, channel_observers) in observers.iter_mut() {
            let before = channel_observers.len();
            channel_observers.retain(|weak| weak.strong_count() > 0);
            let removed = before - channel_observers.len();
            total_removed += removed;

            if removed > 0 {
                crate::log_ui!(
                    debug,
                    "Cleaned {} stale observers from '{}'",
                    removed,
                    channel
                );
            }
        }

        // Remove empty channel entries
        observers.retain(|_, v| !v.is_empty());

        if total_removed > 0 {
            crate::log_ui!(
                info,
                "Cleanup complete: removed {} stale observers",
                total_removed
            );
        }
    }
}

impl Default for ChannelObserverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Global observer registry
static OBSERVER_REGISTRY: LazyLock<ChannelObserverRegistry> =
    LazyLock::new(ChannelObserverRegistry::new);

/// Get the global observer registry
pub fn get_observer_registry() -> &'static ChannelObserverRegistry {
    &OBSERVER_REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestObserver {
        events: Arc<RwLock<Vec<(String, ChannelEvent)>>>,
    }

    impl ChannelObserver for TestObserver {
        fn on_event(&self, channel: &str, event: ChannelEvent) {
            let mut events = self.events.write().unwrap();
            events.push((channel.to_string(), event));
        }
    }

    #[test]
    fn test_observer_registration() {
        let registry = ChannelObserverRegistry::new();
        let events = Arc::new(RwLock::new(Vec::new()));
        let observer = Arc::new(TestObserver {
            events: events.clone(),
        });

        registry.subscribe("test_channel", observer.clone());
        assert_eq!(registry.observer_count("test_channel"), 1);

        // Notify should reach observer
        use super::super::ChatMessage;
        let msg = ChatMessage {
            id: Arc::from("test1"),
            channel: Arc::from("test_channel"),
            sender: Arc::from("testuser"),
            display_name: Arc::from("TestUser"),
            content: Arc::from("Hello"),
            ..Default::default()
        };

        registry.notify("test_channel", ChannelEvent::NewMessage(msg.clone()));

        let received_events = events.read().unwrap();
        assert_eq!(received_events.len(), 1);
        assert_eq!(received_events[0].0, "test_channel");
    }

    #[test]
    fn test_weak_reference_cleanup() {
        let registry = ChannelObserverRegistry::new();
        let events = Arc::new(RwLock::new(Vec::new()));

        {
            let observer = Arc::new(TestObserver {
                events: events.clone(),
            });
            registry.subscribe("test_channel", observer.clone());
            assert_eq!(registry.observer_count("test_channel"), 1);
        } // observer dropped here

        // Cleanup should remove the weak reference
        registry.cleanup_stale_observers();
        assert_eq!(registry.observer_count("test_channel"), 0);
    }
}
