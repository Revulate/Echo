//! Channel lifecycle management for lazy loading with prefetching.
//!
//! Implements on-demand channel connection with adjacent tab prefetching
//! to minimize startup time while maintaining instant tab switching.

use crate::sync::RwLockExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use std::sync::RwLock;

static IS_WHISPERS_CHANNEL: OnceLock<fn(&str) -> bool> = OnceLock::new();

/// Install the application's whispers-channel predicate.
///
/// The whispers pseudo-channel is an application concept, not a Twitch room, so
/// it must never be queued for an IRC connection. Without an installed
/// predicate no channel name is treated as the whispers channel.
pub fn install_whispers_channel_predicate(callback: fn(&str) -> bool) {
    let _ = IS_WHISPERS_CHANNEL.set(callback);
}

fn is_whispers_channel(channel: &str) -> bool {
    IS_WHISPERS_CHANNEL
        .get()
        .is_some_and(|callback| callback(channel))
}

/// Priority levels for channel loading
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadPriority {
    /// Active tab channels (load immediately)
    Immediate = 0,
    /// Adjacent tab prefetch (±1 tabs)
    Prefetch = 5,
    /// Distant saved tabs. Connected at startup so every saved channel is
    /// live, but drained after the visible set and staggered by the connect
    /// worker so a large workspace stays within Twitch's JOIN rate limit.
    Background = 10,
}

/// Connection state for a channel
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelState {
    /// Not yet connected
    Disconnected,
    /// Connection in progress
    Connecting,
    /// Fully connected with history loaded
    Connected,
}

/// Tracks channel connection state and manages lazy loading
pub struct ChannelLifecycleManager {
    /// Current state of each channel
    states: RwLock<HashMap<String, ChannelState>>,
    /// Priority queue for pending connections
    queue: RwLock<VecDeque<(LoadPriority, String)>>,
    /// Channels currently being connected (to avoid duplicates)
    in_progress: RwLock<HashSet<String>>,
}

impl ChannelLifecycleManager {
    pub fn new() -> Self {
        Self {
            states: RwLock::new(HashMap::new()),
            queue: RwLock::new(VecDeque::new()),
            in_progress: RwLock::new(HashSet::new()),
        }
    }

    /// Request channel connection with given priority
    /// Returns true if channel was queued (not already connected/connecting)
    pub fn request_connection(&self, channel: String, priority: LoadPriority) -> bool {
        if is_whispers_channel(&channel) {
            return false;
        }
        let state = {
            let states = self.states.read_or_recover();
            states.get(&channel).cloned()
        };

        match state {
            Some(ChannelState::Connected) => {
                // Already connected
                false
            }
            Some(ChannelState::Connecting) => {
                // Already connecting, don't duplicate
                false
            }
            Some(ChannelState::Disconnected) | None => {
                // Need to connect
                let mut in_progress = self.in_progress.write_or_recover();
                if in_progress.contains(&channel) {
                    // Already queued or actively connecting. If it is still
                    // waiting in the queue at a lower priority (e.g. a
                    // Background startup connect for a tab the user just
                    // visited), promote it so it doesn't sit behind the
                    // staggered background drain.
                    let mut queue = self.queue.write_or_recover();
                    if let Some(pos) = queue
                        .iter()
                        .position(|(p, c)| c == &channel && *p > priority)
                    {
                        queue.remove(pos);
                        let insert_pos = queue
                            .iter()
                            .position(|(p, _)| p > &priority)
                            .unwrap_or(queue.len());
                        queue.insert(insert_pos, (priority, channel.clone()));
                        crate::log_irc!(
                            debug,
                            "Promoted queued channel '{}' to priority {:?}",
                            channel,
                            priority
                        );
                    }
                    return false;
                }
                in_progress.insert(channel.clone());

                let mut queue = self.queue.write_or_recover();
                // Insert with priority (lower number = higher priority)
                let insert_pos = queue
                    .iter()
                    .position(|(p, _)| p > &priority)
                    .unwrap_or(queue.len());
                queue.insert(insert_pos, (priority, channel.clone()));

                crate::log_irc!(
                    debug,
                    "Queued channel '{}' with priority {:?}",
                    channel,
                    priority
                );
                true
            }
        }
    }

    /// Get next channel to connect (highest priority), with its queued priority
    /// so the connect worker can stagger low-priority background joins.
    pub fn next_to_connect(&self) -> Option<(LoadPriority, String)> {
        let mut queue = self.queue.write_or_recover();
        queue.pop_front()
    }

    /// Mark channel as connecting
    pub fn mark_connecting(&self, channel: &str) {
        let mut states = self.states.write_or_recover();
        states.insert(channel.to_string(), ChannelState::Connecting);
        crate::log_irc!(debug, "Channel '{}' connecting", channel);
    }

    /// Mark channel as connected
    pub fn mark_connected(&self, channel: &str) {
        let mut states = self.states.write_or_recover();
        let mut in_progress = self.in_progress.write_or_recover();
        states.insert(channel.to_string(), ChannelState::Connected);
        in_progress.remove(channel);
        crate::log_irc!(info, "Channel '{}' connected", channel);
    }

    /// Mark channel as disconnected (or connection failed) so it can be retried.
    pub fn mark_disconnected(&self, channel: &str) {
        let mut states = self.states.write_or_recover();
        let mut in_progress = self.in_progress.write_or_recover();
        states.insert(channel.to_string(), ChannelState::Disconnected);
        in_progress.remove(channel);
        crate::log_irc!(debug, "Channel '{}' disconnected", channel);
    }

    /// Remove all lifecycle state for a channel that has been closed or parted.
    pub fn remove_channel(&self, channel: &str) {
        let mut states = self.states.write_or_recover();
        let mut in_progress = self.in_progress.write_or_recover();
        let mut queue = self.queue.write_or_recover();

        states.remove(channel);
        in_progress.remove(channel);
        queue.retain(|(_, queued_channel)| queued_channel != channel);

        crate::log_irc!(debug, "Channel '{}' removed from lifecycle state", channel);
    }

    /// Get current state of a channel
    pub fn get_state(&self, channel: &str) -> ChannelState {
        let states = self.states.read_or_recover();
        states
            .get(channel)
            .cloned()
            .unwrap_or(ChannelState::Disconnected)
    }

    /// Check if channel is connected
    pub fn is_connected(&self, channel: &str) -> bool {
        matches!(self.get_state(channel), ChannelState::Connected)
    }
}

impl Default for ChannelLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        let manager = ChannelLifecycleManager::new();

        // Queue channels with different priorities (intentionally out of order)
        manager.request_connection("low".to_string(), LoadPriority::Background);
        manager.request_connection("high".to_string(), LoadPriority::Immediate);
        manager.request_connection("med".to_string(), LoadPriority::Prefetch);

        // Should dequeue in priority order, background last
        assert_eq!(
            manager.next_to_connect(),
            Some((LoadPriority::Immediate, "high".to_string()))
        );
        assert_eq!(
            manager.next_to_connect(),
            Some((LoadPriority::Prefetch, "med".to_string()))
        );
        assert_eq!(
            manager.next_to_connect(),
            Some((LoadPriority::Background, "low".to_string()))
        );
        assert_eq!(manager.next_to_connect(), None);
    }

    #[test]
    fn test_no_duplicate_connections() {
        let manager = ChannelLifecycleManager::new();

        // First request should queue
        assert!(manager.request_connection("test".to_string(), LoadPriority::Immediate));
        // Second request should be ignored (already queued)
        assert!(!manager.request_connection("test".to_string(), LoadPriority::Immediate));
    }

    #[test]
    fn test_state_transitions() {
        let manager = ChannelLifecycleManager::new();

        assert_eq!(manager.get_state("test"), ChannelState::Disconnected);

        manager.mark_connecting("test");
        assert_eq!(manager.get_state("test"), ChannelState::Connecting);

        manager.mark_connected("test");
        assert_eq!(manager.get_state("test"), ChannelState::Connected);
        assert!(manager.is_connected("test"));
    }
}
