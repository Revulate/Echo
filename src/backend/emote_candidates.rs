//! Emote candidate registry for tracking known emote names and pending channels.
//!
//! With the slot-based parsing system, this module provides:
//! - Pending channel tracking (for creating PendingEmote slots during emote loading)
//! - Failed channel tracking (for extended retry during loading failures)
//!
//! When a channel's emotes are being loaded, we mark it as "pending". Messages
//! arriving during this time can create PendingEmote slots for words that look
//! like emotes, which will resolve once the emotes finish loading.
//!
//! If loading fails, channels are added to a retry queue. We continue treating
//! them as "pending" if a retry is imminent (within 30 seconds), allowing emotes
//! to resolve once the retry succeeds.

use dashmap::DashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Channels where emotes are currently loading
/// Maps channel_login (lowercase) -> timestamp when loading started
static PENDING_CHANNELS: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);

/// Maximum time a channel can be in pending state before we give up (30 seconds)
const PENDING_TIMEOUT_SECS: u64 = 30;

/// Mark a channel as loading emotes (creates pending state)
/// Any words in messages for this channel that match cached emote names
/// will be created as PendingEmote slots.
pub fn mark_channel_loading(channel: &str) {
    let channel_lower = channel.to_lowercase();
    PENDING_CHANNELS.insert(channel_lower.clone(), Instant::now());
    crate::log_emotes!(
        debug,
        "Channel '{}' marked as loading emotes",
        channel_lower
    );
}

/// Clear the pending state for a channel (emotes finished loading)
pub fn clear_channel_pending(channel: &str) {
    let channel_lower = channel.to_lowercase();
    PENDING_CHANNELS.remove(&channel_lower);
    crate::log_emotes!(debug, "Channel '{}' pending state cleared", channel_lower);
}

/// Return whether a channel is still inside the emote-loading pending window.
pub fn is_channel_pending(channel: &str) -> bool {
    let channel_lower = channel.to_lowercase();
    PENDING_CHANNELS
        .get(&channel_lower)
        .is_some_and(|started| started.elapsed() < Duration::from_secs(PENDING_TIMEOUT_SECS))
}

/// Proactively clean up stale pending channels
/// Call this periodically (e.g., every 60 seconds) to prevent unbounded memory growth
/// if emote loading fails without calling `clear_channel_pending()`.
pub fn cleanup_stale_pending(mut channel_still_loading: impl FnMut(&str) -> bool) -> usize {
    let timeout = Duration::from_secs(PENDING_TIMEOUT_SECS);
    let mut cleaned = 0;

    let stale_channels: Vec<String> = PENDING_CHANNELS
        .iter()
        .filter(|entry| entry.value().elapsed() >= timeout && !channel_still_loading(entry.key()))
        .map(|entry| entry.key().clone())
        .collect();

    for channel in stale_channels {
        PENDING_CHANNELS.remove(&channel);
        cleaned += 1;
        crate::log_emotes!(
            warn,
            "Cleaned up stale pending state for channel '{}' (timed out after {}s)",
            channel,
            PENDING_TIMEOUT_SECS
        );
    }

    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn stale_cleanup_preserves_active_loads_and_removes_orphans() {
        let active = format!("pending-active-{}", std::process::id());
        let orphan = format!("pending-orphan-{}", std::process::id());
        let stale = Instant::now() - Duration::from_secs(PENDING_TIMEOUT_SECS + 1);
        PENDING_CHANNELS.insert(active.clone(), stale);
        PENDING_CHANNELS.insert(orphan.clone(), stale);

        assert_eq!(cleanup_stale_pending(|channel| channel == active), 1);
        assert!(PENDING_CHANNELS.contains_key(&active));
        assert!(!PENDING_CHANNELS.contains_key(&orphan));

        PENDING_CHANNELS.remove(&active);
    }
}
