//! Periodic background refresh scheduler for emotes
//!
//! This module provides a background task that periodically refreshes emotes:
//! - Retries failed channel loads from the FAILED_CHANNELS queue
//! - Refreshes stale channel emotes (for BTTV/FFZ which have no real-time API)
//! - Refreshes global emotes if stale
//!
//! Note: 7TV has its own EventAPI for real-time updates, so 7TV emotes don't need
//! periodic refresh - they update automatically via WebSocket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval};

use super::{EMOTE_CACHE, get_due_retry_channels};

/// Default refresh interval (30 minutes)
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 30 * 60;

/// Delay between individual channel refreshes to avoid rate limiting
const CHANNEL_REFRESH_STAGGER_MS: u64 = 200;

/// Global flag to track if the scheduler is running
static SCHEDULER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Start the periodic refresh scheduler
///
/// This spawns a background task that runs every 30 minutes (by default) and:
/// 1. Processes channels from the failed retry queue
/// 2. Refreshes stale channel emotes (for BTTV/FFZ)
/// 3. Refreshes global emotes if needed
///
/// Safe to call multiple times - only one scheduler will run.
pub fn start_refresh_scheduler() {
    if SCHEDULER_RUNNING.swap(true, Ordering::SeqCst) {
        crate::log_emotes!(debug, "Refresh scheduler already running");
        return;
    }

    crate::log_emotes!(
        info,
        "Starting emote refresh scheduler (interval: {}s)",
        DEFAULT_REFRESH_INTERVAL_SECS
    );

    super::spawn_runtime_thread("emote-refresh-scheduler", &SCHEDULER_RUNNING, || async {
        let mut ticker = interval(Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await;

        loop {
            ticker.tick().await;

            if let Err(e) = run_refresh_cycle().await {
                crate::log_emotes!(warn, "Refresh cycle error: {}", e);
            }
        }
    });
}

/// Run a single refresh cycle
///
/// This is the core refresh logic, separated out for testability.
async fn run_refresh_cycle() -> Result<(), String> {
    crate::log_emotes!(info, "Starting emote refresh cycle");

    let retry_count = process_failed_channels().await;

    let global_refreshed = refresh_global_if_stale().await;

    let channels_refreshed = refresh_stale_channels().await;

    super::prune_tracking_maps();

    crate::log_emotes!(
        info,
        "Refresh cycle complete: retried={}, global_refreshed={}, channels_refreshed={}, tracking_maps_pruned=true",
        retry_count,
        global_refreshed,
        channels_refreshed
    );

    Ok(())
}

/// Process channels from the failed retry queue
///
/// Returns the number of channels that were retried.
async fn process_failed_channels() -> usize {
    let due_channels = get_due_retry_channels();

    if due_channels.is_empty() {
        return 0;
    }

    crate::log_emotes!(
        info,
        "Processing {} channels from failed retry queue",
        due_channels.len()
    );

    let mut retried = 0;

    for entry in due_channels {
        if retried > 0 {
            tokio::time::sleep(Duration::from_millis(CHANNEL_REFRESH_STAGGER_MS)).await;
        }

        crate::log_emotes!(
            info,
            "Retrying failed channel '{}' (attempt {})",
            entry.channel_login,
            entry.attempts + 1
        );

        let result = EMOTE_CACHE
            .load_channel_emotes(&entry.channel_id, &entry.channel_login)
            .await;

        match result {
            Ok(()) => {
                crate::log_emotes!(
                    info,
                    "Retry successful for channel '{}'",
                    entry.channel_login
                );
            }
            Err(e) => {
                crate::log_emotes!(
                    warn,
                    "Retry failed for channel '{}': {}",
                    entry.channel_login,
                    e
                );
            }
        }

        retried += 1;
    }

    retried
}

/// Refresh global emotes if they're stale
///
/// Returns true if a refresh was performed.
async fn refresh_global_if_stale() -> bool {
    if !EMOTE_CACHE.global_needs_refresh() {
        return false;
    }

    crate::log_emotes!(info, "Refreshing stale global emotes");

    match EMOTE_CACHE.load_global_emotes().await {
        Ok(()) => {
            crate::log_emotes!(info, "Global emotes refresh successful");
            true
        }
        Err(e) => {
            crate::log_emotes!(warn, "Global emotes refresh failed: {}", e);
            false
        }
    }
}

/// Refresh stale channel emotes for all cached channels
///
/// This checks each cached channel and refreshes if the TTL has expired.
/// Only BTTV/FFZ emotes need this - 7TV updates via EventAPI.
///
/// Returns the number of channels that were refreshed.
async fn refresh_stale_channels() -> usize {
    let channels_to_refresh: Vec<(String, String)> = EMOTE_CACHE
        .channels
        .iter()
        .filter(|entry| !crate::backend::irc::is_channel_closed(entry.key()))
        .filter(|entry| super::EmoteCache::needs_refresh(entry.value(), EMOTE_CACHE.ttl))
        .map(|entry| {
            let channel_login = entry.key().clone();
            let channel_id = crate::backend::bridge::get_channel_id_cached(&channel_login)
                .unwrap_or_else(|| channel_login.clone());
            (channel_id, channel_login)
        })
        .collect();

    if channels_to_refresh.is_empty() {
        return 0;
    }

    crate::log_emotes!(
        info,
        "Refreshing {} stale channel emote caches",
        channels_to_refresh.len()
    );

    let mut refreshed = 0;

    for (channel_id, channel_login) in channels_to_refresh {
        if refreshed > 0 {
            tokio::time::sleep(Duration::from_millis(CHANNEL_REFRESH_STAGGER_MS)).await;
        }

        crate::log_emotes!(debug, "Refreshing emotes for channel '{}'", channel_login);

        match EMOTE_CACHE
            .load_channel_emotes(&channel_id, &channel_login)
            .await
        {
            Ok(()) => {
                crate::log_emotes!(debug, "Channel '{}' emotes refreshed", channel_login);
                refreshed += 1;
            }
            Err(e) => {
                crate::log_emotes!(
                    warn,
                    "Failed to refresh channel '{}' emotes: {}",
                    channel_login,
                    e
                );
            }
        }
    }

    refreshed
}
