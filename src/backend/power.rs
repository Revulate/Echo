//! System sleep/wake handling for Echo's long-lived chat sockets.
//!
//! Sockets do not survive a suspend, and without a hint the IRC watchdog only
//! notices up to 90 seconds after wake. Sleep stamps each channel's gap start;
//! wake waits for the network, renews an expiring token, then replaces the IRC,
//! EventSub, and 7TV EventAPI connections so the IRC gap-fill runs immediately.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Longest a wake waits for the network before reconnecting anyway.
const WAKE_NETWORK_WAIT: Duration = Duration::from_secs(30);
const WAKE_NETWORK_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const WAKE_NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// The recent-messages host that the post-wake gap-fill depends on.
const WAKE_NETWORK_PROBE_URL: &str = "https://logs.zonian.dev/";
/// Gap-fill falls back to this host directly, so it also proves the route.
const WAKE_NETWORK_FALLBACK_PROBE_URL: &str = "https://recent-messages.robotty.de/";

/// Latest wake; an older wake's pending reconnect yields to a newer one.
static WAKE_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn handle_system_sleep() {
    if crate::bench::network_lanes_suppressed() {
        return;
    }
    crate::log_irc!(info, "System sleep - recording chat gap start");
    crate::backend::irc::record_system_sleep();
}

pub fn handle_system_wake() {
    if crate::bench::network_lanes_suppressed() || crate::backend::is_shutdown_started() {
        return;
    }
    crate::backend::irc::note_system_wake();
    let generation = WAKE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    crate::backend::bridge::RUNTIME.spawn(async move {
        let waited = wait_for_network().await;
        if WAKE_GENERATION.load(Ordering::Acquire) != generation
            || crate::backend::is_shutdown_started()
        {
            return;
        }
        refresh_token_after_wake().await;
        if WAKE_GENERATION.load(Ordering::Acquire) != generation
            || crate::backend::is_shutdown_started()
        {
            return;
        }
        crate::log_irc!(
            info,
            waited_ms = waited.as_millis(),
            "System wake - reconnecting chat sockets"
        );
        crate::backend::eventsub::reconnect_after_system_wake();
        crate::backend::emotes::seventv_eventapi::restart_after_system_wake();
        crate::backend::irc::reconnect_after_system_wake();
    });
}

/// Renew an access token that expired or nearly expired during sleep before the
/// sockets reconnect with it.
async fn refresh_token_after_wake() {
    let has_token =
        crate::backend::settings::get_token().is_some_and(|token| !token.trim().is_empty());
    if !has_token
        || crate::backend::irc::is_anonymous_mode()
        || !(crate::backend::auth::is_token_expired()
            || crate::backend::auth::is_token_expiring_soon())
    {
        return;
    }
    crate::log_auth!(
        info,
        "System wake - refreshing an expiring token before reconnecting"
    );
    if let Err(error) = crate::backend::auth::refresh_and_save_tokens().await {
        crate::log_auth!(warn, "Token refresh after system wake failed: {}", error);
    }
}

/// Wait until an HTTP round trip succeeds through Echo's proxy-aware client,
/// up to [`WAKE_NETWORK_WAIT`]. Any HTTP status proves the route is back.
async fn wait_for_network() -> Duration {
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + WAKE_NETWORK_WAIT;
    let client = crate::backend::http::client();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return start.elapsed();
        }

        let probe_timeout = WAKE_NETWORK_PROBE_TIMEOUT.min(remaining);
        let primary = client
            .head(WAKE_NETWORK_PROBE_URL)
            .timeout(probe_timeout)
            .send();
        let fallback = client
            .head(WAKE_NETWORK_FALLBACK_PROBE_URL)
            .timeout(probe_timeout)
            .send();
        tokio::pin!(primary);
        tokio::pin!(fallback);

        let route_available = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                primary_result = &mut primary => {
                    primary_result.is_ok() || fallback.await.is_ok()
                }
                fallback_result = &mut fallback => {
                    fallback_result.is_ok() || primary.await.is_ok()
                }
            }
        })
        .await
        .unwrap_or(false);
        if route_available {
            return start.elapsed();
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return start.elapsed();
        }
        tokio::time::sleep(WAKE_NETWORK_PROBE_INTERVAL.min(remaining)).await;
    }
}
