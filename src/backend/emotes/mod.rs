//! Emote provider integrations (7TV, BTTV, FFZ, Twitch)
#![allow(clippy::await_holding_lock)]

use crate::backend::lock_utils::{MutexExt, RwLockExt};
use arc_swap::ArcSwap;
use backon::Retryable;
use dashmap::{DashMap, DashSet};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::backend::constants::CLIENT_ID;
use crate::backend::emote_candidates::{
    clear_channel_pending, is_channel_pending, mark_channel_loading,
};
use crate::backend::retry::{
    EMOTE_MAX_RETRY_AFTER, fetch_with_retry_str, is_reqwest_transient, retry_helix_get,
    retry_helix_get_str, server_retry_wait,
};
use crate::backend::twitch_api::HelixAuthExt;

pub mod personal;
pub mod refresh;
pub mod seventv_eventapi;

static INITIAL_CATALOGS_READY: AtomicBool = AtomicBool::new(false);
static INITIAL_CATALOGS_READY_NOTIFY: LazyLock<tokio::sync::Notify> =
    LazyLock::new(tokio::sync::Notify::new);

/// Which sections of the on-disk emote cache [`EmoteCache::load_from_disk_scoped`]
/// hydrates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiskCacheParts {
    /// Global and channel sets. They belong to no account, so startup hydrates
    /// them while the token is still being validated.
    Shared,
    /// The signed-in account's user emotes. These hydrate only for the
    /// validated owner, so startup loads them after auth initialization.
    User,
}

pub(crate) fn mark_initial_catalogs_ready() {
    INITIAL_CATALOGS_READY.store(true, Ordering::Release);
    INITIAL_CATALOGS_READY_NOTIFY.notify_waiters();
}

pub(crate) async fn wait_for_initial_catalogs_ready() {
    loop {
        // Register before checking the flag, so a `notify_waiters` between the
        // check and the await is not lost.
        let notified = INITIAL_CATALOGS_READY_NOTIFY.notified();
        if INITIAL_CATALOGS_READY.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

static EMOTE_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_emote_http_client));

fn build_emote_http_client() -> reqwest::Client {
    match crate::backend::http::client_builder()
        .timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create emote provider HTTP client: {e}");
            crate::backend::http::timed_client(Duration::from_secs(10))
        }
    }
}

/// Truncate a string to at most `max_bytes` bytes for log previews, backing off
/// to the nearest char boundary so multibyte codepoints (CJK, emoji — common in
/// 7TV response bodies) don't panic a byte-index slice.
fn truncate_for_log(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes)]
}

static SEVENTV_USER_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_seventv_user_http_client));

fn build_seventv_user_http_client() -> reqwest::Client {
    match crate::backend::http::client_builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create 7TV user HTTP client: {e}");
            crate::backend::http::timed_client(Duration::from_secs(20))
        }
    }
}

static SEVENTV_EMOTE_SET_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_seventv_emote_set_http_client));

fn build_seventv_emote_set_http_client() -> reqwest::Client {
    match crate::backend::http::client_builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create 7TV emote-set HTTP client: {e}");
            crate::backend::http::timed_client(Duration::from_secs(25))
        }
    }
}

use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};
use std::num::NonZeroU32;

/// Rate limiter for 7TV API requests (10 req/s - conservative, actual limit higher)
static SEVENTV_LIMITER: LazyLock<
    RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>,
> = LazyLock::new(|| RateLimiter::direct(Quota::per_second(NonZeroU32::new(10).unwrap())));

/// Rate limiter for BTTV API requests (5 req/s - BTTV has stricter limits)
static BTTV_LIMITER: LazyLock<RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>> =
    LazyLock::new(|| RateLimiter::direct(Quota::per_second(NonZeroU32::new(5).unwrap())));

/// Rate limiter for FFZ API requests (5 req/s)
static FFZ_LIMITER: LazyLock<RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>> =
    LazyLock::new(|| RateLimiter::direct(Quota::per_second(NonZeroU32::new(5).unwrap())));
/// Fresh disk-emote entries can skip the normal provider refresh, but FFZ room
/// authority badges are intentionally memory-only. Keep one bounded hydration
/// request per channel in flight so cache reuse cannot leave the badge state
/// empty for the entire session.
static FFZ_ROOM_BADGE_HYDRATIONS: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);

/// TTL for cached emotes on disk (in seconds)
const USER_EMOTES_DISK_TTL_SECS: u64 = 24 * 60 * 60;
const GLOBAL_EMOTES_DISK_TTL_SECS: u64 = 6 * 60 * 60;
/// Maximum age a channel disk cache may have and still be served at startup.
/// Channel emote sets change rarely; serving a stale set immediately and
/// refreshing in the background beats blocking message tokenization on a
/// 2-5s provider round trip every session. Refreshes publish the current
/// catalog for future ingestion, and the 7TV EventAPI covers live changes.
const CHANNEL_EMOTES_DISK_STALE_SERVE_MAX_SECS: u64 = 7 * 24 * 60 * 60;

/// How long after loading from disk before triggering a background refresh (in seconds)
/// This allows fast startup while still getting fresh data soon after
const DISK_CACHE_REFRESH_DELAY_SECS: u64 = 5 * 60;
/// Stale-but-not-expired channel caches are still good enough for first paint.
/// Give active channel startup paths a short grace window so they can display
/// cached emotes immediately instead of stampeding provider APIs.
const CHANNEL_DISK_STALE_REFRESH_GRACE_SECS: u64 = 2 * 60;
const SEVENTV_CHANNEL_SYNC_TIMEOUT_MS: u64 = 1_500;
const SEVENTV_GLOBAL_SYNC_TIMEOUT_MS: u64 = 10_000;
const SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS: u64 = 90;
const SEVENTV_CHANNEL_SYNC_TIMEOUT_COOLDOWN_SECS: u64 = 12;
const SEVENTV_CHANNEL_SYNC_GLOBAL_COOLDOWN_SECS: u64 = 60;
const USER_EMOTES_SYNC_TIMEOUT_MS: u64 = 2500;
const USER_SEVENTV_PERSONAL_SYNC_TIMEOUT_MS: u64 = 1000;
const USER_EMOTES_DEFERRED_FETCH_TIMEOUT_SECS: u64 = 45;
/// Longest single Helix rate-limit wait honoured mid-pagination.
const USER_EMOTES_RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(10);
/// Rate-limit waits must finish by this point of a user emote fetch, well
/// inside the deferred refresh timeout, or pagination stops with a partial set.
const USER_EMOTES_RATE_LIMIT_BUDGET: Duration = Duration::from_secs(30);
const SEVENTV_GLOBAL_FETCH_CACHE_TTL_SECS: u64 = 45;
const SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS: u64 = 30 * 60;
const SEVENTV_CHANNEL_FETCH_CACHE_TTL_SECS: u64 = 20;
const SEVENTV_CHANNEL_FETCH_CACHE_MAX_ENTRIES: usize = 512;
pub(crate) const PERSONAL_SEVENTV_CHANNEL: &str = "personal:7tv";
const CHANNEL_MISS_REFRESH_COOLDOWN_SECS: u64 = 300;
/// Refresh delay after a provider failed with no cached emotes to carry over.
const CHANNEL_PROVIDER_FAILURE_RETRY_SECS: u64 = 60;
/// Default-on bounded self-heal: when a chat token looked like an emote but
/// missed the lookup map, schedule a refresh so a stale/pruned set recovers
/// instead of rendering plain text forever. The 5-minute per-channel cooldown,
/// the mixed-case candidate heuristic (`unresolved_emote_refresh_candidate`),
/// and the once-per-hour per-token gate keep this from stampeding providers
/// on ordinary chat words — 7TV EventAPI remains the primary update path.
/// Opt out with `ECHO_EMOTE_MISS_REFRESH=0` (also accepts `false`/`off`);
/// unset or `1` = on.
static CHANNEL_MISS_REFRESH_ENABLED: LazyLock<bool> = LazyLock::new(|| {
    !matches!(
        std::env::var("ECHO_EMOTE_MISS_REFRESH")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("0") | Some("false") | Some("off")
    )
});

/// How long after a channel open/load the open watchdog waits before checking
/// whether the channel ever gained a lookup entry, and self-healing if not.
const CHANNEL_OPEN_EMOTE_WATCHDOG_SECS: u64 = 10;

/// Stage duration threshold for highlighting slow emote pipeline steps.
const EMOTE_PERF_WARN_MS: u128 = 750;

#[inline]
fn record_emote_stage_duration(stage: &str, elapsed: Duration, items: usize) {
    if !crate::backend::metrics::is_enabled() && !crate::log::enabled("perf") {
        return;
    }

    crate::backend::metrics::record_emote_stage(stage, elapsed, items);

    let elapsed_ms = elapsed.as_millis();
    if elapsed_ms >= EMOTE_PERF_WARN_MS {
        crate::log_perf_emotes!(
            warn,
            stage = stage,
            elapsed_ms,
            items,
            "Slow emote pipeline stage"
        );
    } else {
        crate::log_perf_emotes!(
            debug,
            stage = stage,
            elapsed_ms,
            items,
            "Emote pipeline stage"
        );
    }
}

#[inline]
fn record_emote_stage_timed(stage: &str, started_at: Instant, items: usize) -> Duration {
    let elapsed = started_at.elapsed();
    record_emote_stage_duration(stage, elapsed, items);
    elapsed
}

fn record_7tv_deferred_watchdog(scope: &str, event: &str) {
    let (counter, stage) = match (scope, event) {
        ("global", "scheduled") => (
            &SEVENTV_DEFERRED_GLOBAL_SCHEDULED,
            "emotes.7tv.deferred.global.scheduled",
        ),
        ("global", "started") => (
            &SEVENTV_DEFERRED_GLOBAL_STARTED,
            "emotes.7tv.deferred.global.started",
        ),
        ("global", "completed") => (
            &SEVENTV_DEFERRED_GLOBAL_COMPLETED,
            "emotes.7tv.deferred.global.completed",
        ),
        ("global", "failed") => (
            &SEVENTV_DEFERRED_GLOBAL_FAILED,
            "emotes.7tv.deferred.global.failed",
        ),
        ("global", "timeout") => (
            &SEVENTV_DEFERRED_GLOBAL_TIMEOUT,
            "emotes.7tv.deferred.global.timeout",
        ),
        ("global", "join_failed") => (
            &SEVENTV_DEFERRED_GLOBAL_JOIN_FAILED,
            "emotes.7tv.deferred.global.join_failed",
        ),
        ("channel", "scheduled") => (
            &SEVENTV_DEFERRED_CHANNEL_SCHEDULED,
            "emotes.7tv.deferred.channel.scheduled",
        ),
        ("channel", "started") => (
            &SEVENTV_DEFERRED_CHANNEL_STARTED,
            "emotes.7tv.deferred.channel.started",
        ),
        ("channel", "completed") => (
            &SEVENTV_DEFERRED_CHANNEL_COMPLETED,
            "emotes.7tv.deferred.channel.completed",
        ),
        ("channel", "failed") => (
            &SEVENTV_DEFERRED_CHANNEL_FAILED,
            "emotes.7tv.deferred.channel.failed",
        ),
        ("channel", "timeout") => (
            &SEVENTV_DEFERRED_CHANNEL_TIMEOUT,
            "emotes.7tv.deferred.channel.timeout",
        ),
        ("channel", "join_failed") => (
            &SEVENTV_DEFERRED_CHANNEL_JOIN_FAILED,
            "emotes.7tv.deferred.channel.join_failed",
        ),
        _ => return,
    };

    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    crate::backend::metrics::record_emote_event(stage);
    record_emote_stage_duration(stage, Duration::ZERO, 1);

    let (scheduled, started, completed, failed, timed_out, join_failed) = if scope == "global" {
        (
            SEVENTV_DEFERRED_GLOBAL_SCHEDULED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_GLOBAL_STARTED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_GLOBAL_COMPLETED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_GLOBAL_FAILED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_GLOBAL_TIMEOUT.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_GLOBAL_JOIN_FAILED.load(Ordering::Relaxed),
        )
    } else {
        (
            SEVENTV_DEFERRED_CHANNEL_SCHEDULED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_CHANNEL_STARTED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_CHANNEL_COMPLETED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_CHANNEL_FAILED.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_CHANNEL_TIMEOUT.load(Ordering::Relaxed),
            SEVENTV_DEFERRED_CHANNEL_JOIN_FAILED.load(Ordering::Relaxed),
        )
    };

    crate::log_perf!(
        debug,
        scope = scope,
        event = event,
        count,
        scheduled,
        started,
        completed,
        failed,
        timed_out,
        join_failed,
        "7TV deferred watchdog event"
    );
}

#[derive(Clone)]
struct SevenTVGlobalFetchCacheEntry {
    fetched_at: Instant,
    emotes: SevenTvGlobalFetchResult,
}

type SevenTvGlobalFetchResult = Arc<Vec<Arc<Emote>>>;

#[derive(Clone)]
struct SevenTVChannelFetchCacheEntry {
    fetched_at: Instant,
    result: SevenTvChannelFetchResult,
}

#[derive(Clone)]
pub(crate) enum SevenTvExactSetFetchOutcome {
    Present {
        emotes: Vec<Arc<Emote>>,
        emote_set_id: Option<String>,
    },
    TerminallyAbsent {
        emote_set_id: String,
    },
}

impl SevenTvExactSetFetchOutcome {
    #[inline]
    fn emote_count(&self) -> usize {
        match self {
            Self::Present { emotes, .. } => emotes.len(),
            Self::TerminallyAbsent { .. } => 0,
        }
    }
}

type SevenTvChannelFetchPayload = (
    SevenTvExactSetFetchOutcome,
    Option<seventv_eventapi::SevenTvChannelSubscription>,
);
type SevenTvChannelFetchResult = Arc<SevenTvChannelFetchPayload>;

/// Short-lived 7TV fetch caches used to coalesce bursts from sync/deferred refresh paths.
static SEVENTV_GLOBAL_FETCH_CACHE: LazyLock<RwLock<Option<SevenTVGlobalFetchCacheEntry>>> =
    LazyLock::new(|| RwLock::new(None));
static SEVENTV_GLOBAL_FETCH_IN_FLIGHT: LazyLock<Mutex<Option<Arc<tokio::sync::Notify>>>> =
    LazyLock::new(|| Mutex::new(None));
static SEVENTV_CHANNEL_FETCH_CACHE: LazyLock<DashMap<String, SevenTVChannelFetchCacheEntry>> =
    LazyLock::new(DashMap::new);
static SEVENTV_CHANNEL_FETCH_IN_FLIGHT: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT: LazyLock<DashMap<String, Instant>> =
    LazyLock::new(DashMap::new);
static SEVENTV_CHANNEL_SYNC_TIMEOUT_GLOBAL_LAST_AT: LazyLock<Mutex<Option<Instant>>> =
    LazyLock::new(|| Mutex::new(None));
static SEVENTV_CHANNEL_SYNC_PROBE_CLAIMED: AtomicBool = AtomicBool::new(false);
static CHANNEL_MISS_REFRESH_LAST_AT: LazyLock<DashMap<String, Instant>> =
    LazyLock::new(DashMap::new);
/// (channel, token) pairs that already triggered a heal refresh. One heal per
/// token per channel per hour — if the refresh did not resolve the token, more
/// refreshes will not either (EventAPI delivers genuinely new emotes).
static CHANNEL_MISS_REFRESH_TOKEN_SEEN: LazyLock<DashMap<(String, String), Instant>> =
    LazyLock::new(DashMap::new);

fn seventv_sync_cooldown_remaining(channel: &str, now: Instant) -> Option<Duration> {
    let channel_cooldown = Duration::from_secs(SEVENTV_CHANNEL_SYNC_TIMEOUT_COOLDOWN_SECS);
    let channel_remaining =
        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT
            .get(channel)
            .and_then(|last_timeout| {
                channel_cooldown.checked_sub(now.saturating_duration_since(*last_timeout))
            });
    let global_cooldown = Duration::from_secs(SEVENTV_CHANNEL_SYNC_GLOBAL_COOLDOWN_SECS);
    let global_remaining = SEVENTV_CHANNEL_SYNC_TIMEOUT_GLOBAL_LAST_AT
        .lock_or_recover()
        .and_then(|last_timeout| {
            global_cooldown.checked_sub(now.saturating_duration_since(last_timeout))
        });
    channel_remaining.max(global_remaining)
}

fn record_seventv_sync_timeout(channel: String, now: Instant) {
    SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.insert(channel, now);
    *SEVENTV_CHANNEL_SYNC_TIMEOUT_GLOBAL_LAST_AT.lock_or_recover() = Some(now);
}

fn claim_seventv_sync_probe() -> bool {
    SEVENTV_CHANNEL_SYNC_PROBE_CLAIMED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// The 7TV global emote set ID (discovered from API response, used for EventAPI subscription)
pub static SEVENTV_GLOBAL_SET_ID: LazyLock<RwLock<Option<String>>> =
    LazyLock::new(|| RwLock::new(None));

static SEVENTV_DEFERRED_GLOBAL_SCHEDULED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_GLOBAL_STARTED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_GLOBAL_COMPLETED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_GLOBAL_FAILED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_GLOBAL_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_GLOBAL_JOIN_FAILED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_SCHEDULED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_STARTED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_COMPLETED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_FAILED: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static SEVENTV_DEFERRED_CHANNEL_JOIN_FAILED: AtomicU64 = AtomicU64::new(0);
static USER_EMOTES_DEFERRED_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Entry tracking a failed channel emote load for retry
#[derive(Clone, Debug)]
pub struct FailedChannelEntry {
    pub channel_login: String,
    pub channel_id: String,
    /// Number of retry attempts (after initial backon exhaustion)
    pub attempts: u8,
    /// When the next retry should be attempted
    pub next_retry: Instant,
}

/// Retry schedule for failed channels (after backon's 3 attempts are exhausted)
/// - Attempt 4: Retry after 2 minutes
/// - Attempt 5: Retry after 10 minutes
/// - Attempt 6+: Retry after 30 minutes
impl FailedChannelEntry {
    fn new(channel_login: String, channel_id: String) -> Self {
        Self {
            channel_login,
            channel_id,
            attempts: 1,
            next_retry: Instant::now() + Duration::from_secs(2 * 60),
        }
    }

    /// Schedule next retry with increasing backoff
    fn schedule_next_retry(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        let delay = match self.attempts {
            1..=2 => Duration::from_secs(2 * 60),
            3..=4 => Duration::from_secs(10 * 60),
            _ => Duration::from_secs(30 * 60),
        };
        self.next_retry = Instant::now() + delay;
    }

    /// Check if this channel is due for a retry
    fn is_due(&self) -> bool {
        Instant::now() >= self.next_retry
    }
}

/// Global registry of channels that failed to load emotes
/// These will be retried with extended backoff by the refresh scheduler
/// Public so emote_candidates can check for imminent retries
pub static FAILED_CHANNELS: LazyLock<DashMap<String, FailedChannelEntry>> =
    LazyLock::new(DashMap::new);

/// Add a channel to the failed retry queue
pub fn mark_channel_failed(channel_login: &str, channel_id: &str) {
    let key = channel_login.to_lowercase();
    if crate::backend::irc::is_channel_closed(&key) {
        return;
    }
    FAILED_CHANNELS
        .entry(key.clone())
        .and_modify(|entry| {
            entry.schedule_next_retry();
            crate::log_emotes!(
                warn,
                "Channel '{}' emote load failed again (attempt {}), next retry in {:?}",
                entry.channel_login,
                entry.attempts,
                entry.next_retry.saturating_duration_since(Instant::now())
            );
        })
        .or_insert_with(|| {
            let entry = FailedChannelEntry::new(channel_login.to_string(), channel_id.to_string());
            crate::log_emotes!(
                warn,
                "Channel '{}' emote load failed, queued for retry in {:?}",
                channel_login,
                entry.next_retry.saturating_duration_since(Instant::now())
            );
            entry
        });
}

/// Remove a channel from the failed retry queue (called on successful load)
pub fn clear_channel_failed(channel_login: &str) {
    let key = channel_login.to_lowercase();
    if FAILED_CHANNELS.remove(&key).is_some() {
        crate::log_emotes!(
            info,
            "Channel '{}' emotes loaded successfully, removed from retry queue",
            channel_login
        );
    }
}

/// Get all channels that are due for retry
pub fn get_due_retry_channels() -> Vec<FailedChannelEntry> {
    FAILED_CHANNELS.retain(|key, _| !crate::backend::irc::is_channel_closed(key));
    FAILED_CHANNELS
        .iter()
        .filter(|entry| entry.is_due())
        .map(|entry| entry.value().clone())
        .collect()
}

mod api {
    /// 7TV API endpoints
    pub mod seventv {
        pub const GLOBAL_EMOTES: &str = "https://7tv.io/v3/emote-sets/global";

        pub fn user(twitch_id: &str) -> String {
            format!("https://7tv.io/v3/users/twitch/{twitch_id}")
        }

        pub fn emote_set(set_id: &str) -> String {
            format!("https://7tv.io/v3/emote-sets/{set_id}")
        }
    }

    /// BTTV API endpoints
    pub mod bttv {
        pub const GLOBAL_EMOTES: &str = "https://api.betterttv.net/3/cached/emotes/global";

        pub fn channel(twitch_id: &str) -> String {
            format!("https://api.betterttv.net/3/cached/users/twitch/{twitch_id}")
        }
    }

    /// FFZ API endpoints
    pub mod ffz {
        pub const GLOBAL_EMOTES: &str = "https://api.frankerfacez.com/v1/set/global";

        pub fn room(channel_login: &str) -> String {
            format!("https://api.frankerfacez.com/v1/room/{channel_login}")
        }
    }

    /// Twitch Helix API endpoints
    pub mod twitch {
        pub const GLOBAL_EMOTES: &str = "https://api.twitch.tv/helix/chat/emotes/global";

        pub fn channel_emotes(broadcaster_id: &str) -> String {
            format!("https://api.twitch.tv/helix/chat/emotes?broadcaster_id={broadcaster_id}")
        }

        pub fn user_emotes(user_id: &str) -> String {
            format!("https://api.twitch.tv/helix/chat/emotes/user?user_id={user_id}")
        }

        pub fn users(id_params: &[String]) -> String {
            format!("https://api.twitch.tv/helix/users?{}", id_params.join("&"))
        }
    }
}

mod cache_codec;

use cache_codec::{
    CachedEmoteSet, EMOTE_CACHE_VERSION, EmoteCacheFile, EmoteCacheLoadError, EmoteCacheReadError,
    EmoteCacheReadSource, legacy_emote_cache_path, read_emote_cache_with_legacy_fallback,
    write_current_emote_cache_atomic,
};

fn channel_disk_cache_fetched_at(is_fresh: bool, ttl: Duration) -> Instant {
    if is_fresh {
        let refresh_delay = Duration::from_secs(DISK_CACHE_REFRESH_DELAY_SECS);
        return Instant::now()
            .checked_sub(ttl)
            .unwrap_or_else(Instant::now)
            .checked_add(refresh_delay)
            .unwrap_or_else(Instant::now);
    }

    let grace_secs = std::env::var("ECHO_CHANNEL_STARTUP_REFRESH_GRACE_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(CHANNEL_DISK_STALE_REFRESH_GRACE_SECS);
    let grace = Duration::from_secs(grace_secs).min(ttl.saturating_sub(Duration::from_secs(1)));
    Instant::now()
        .checked_sub(ttl.saturating_sub(grace))
        .unwrap_or_else(Instant::now)
}

/// Global emote cache shared across the application
pub static EMOTE_CACHE: LazyLock<EmoteCache> = LazyLock::new(EmoteCache::new);

/// Cold channel catalogs whose first provider pass published a partial index
/// while a deferred 7TV fetch is still authoritative. The Arc token makes an
/// old deferred task unable to clear a newer open/load generation.
static CHANNEL_CATALOG_PENDING: LazyLock<DashMap<String, Arc<AtomicBool>>> =
    LazyLock::new(DashMap::new);

struct ChannelCatalogPendingGuard {
    key: String,
    token: Arc<AtomicBool>,
}

impl ChannelCatalogPendingGuard {
    fn begin(key: String) -> Self {
        let token = Arc::new(AtomicBool::new(true));
        CHANNEL_CATALOG_PENDING.insert(key.clone(), Arc::clone(&token));
        Self { key, token }
    }
}

impl Drop for ChannelCatalogPendingGuard {
    fn drop(&mut self) {
        self.token.store(false, Ordering::Release);
        CHANNEL_CATALOG_PENDING
            .remove_if(&self.key, |_, current| Arc::ptr_eq(current, &self.token));
        // A pending catalog was left out of any save taken meanwhile.
        mark_emote_cache_dirty();
    }
}

/// Bumped by every change the on-disk emote cache should capture: global and
/// user sets, channel catalogs, and pending catalogs settling. Separate from
/// the render/layout versions so persistence never causes relayout.
static EMOTE_CACHE_PERSIST_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Serializes whole saves (snapshot through file replacement) so an older
/// autosave can never publish over a newer shutdown snapshot.
static EMOTE_CACHE_SAVE_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

fn mark_emote_cache_dirty() {
    EMOTE_CACHE_PERSIST_GENERATION.fetch_add(1, Ordering::AcqRel);
}

pub fn emote_cache_persist_generation() -> u64 {
    EMOTE_CACHE_PERSIST_GENERATION.load(Ordering::Acquire)
}

#[cfg(test)]
mod catalog_pending_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn older_pending_guard_cannot_clear_newer_generation() {
        let key = format!("catalog-pending-{}", std::process::id());
        let older = ChannelCatalogPendingGuard::begin(key.clone());
        let newer = ChannelCatalogPendingGuard::begin(key.clone());
        drop(older);
        assert!(
            CHANNEL_CATALOG_PENDING
                .get(&key)
                .is_some_and(|token| token.load(Ordering::Acquire))
        );
        drop(newer);
        assert!(!CHANNEL_CATALOG_PENDING.contains_key(&key));
    }

    #[test]
    fn pending_guard_clears_when_deferred_owner_settles() {
        let key = format!("catalog-deferred-{}", std::process::id());
        let deferred_owner = ChannelCatalogPendingGuard::begin(key.clone());
        assert!(CHANNEL_CATALOG_PENDING.contains_key(&key));
        drop(deferred_owner);
        assert!(!CHANNEL_CATALOG_PENDING.contains_key(&key));
    }
}

pub use echo_emote_core::{Emote, EmoteProvider};

/// Preferred emote URL for chat rendering quality.
///
/// Chat defaults to the smallest tier with enough physical source resolution
/// for the current DPI and logical emote size. Normal 100%/150% rows use 2x for
/// crisp downsampling while 4x remains reserved for genuinely larger output.
/// Users can explicitly override this to prefer 1x, 2x, or 4x from settings.
#[inline]
pub fn chat_display_url(emote: &Emote) -> &str {
    chat_display_url_for_scale(emote, crate::backend::settings::get_window_scale_factor())
}

/// [`chat_display_url`] for a window with device scale factor `window_scale`.
#[inline]
pub fn chat_display_url_for_scale(emote: &Emote, window_scale: f32) -> &str {
    use crate::backend::emote_quality::EmoteImageScale;
    let mut scale = crate::backend::settings::get_emote_image_scale();
    if scale == EmoteImageScale::Auto {
        scale = crate::backend::emote_quality::resolve_auto_tier(
            window_scale,
            crate::backend::settings::get_font_scale()
                * crate::backend::settings::get_emote_scale(),
        );
    }
    crate::backend::emote_quality::select_emote_display_url(
        emote.url.as_ref(),
        emote.url_1x.as_deref(),
        emote.url_2x.as_deref(),
        emote.url_4x.as_deref(),
        scale,
    )
}

/// Shared ownership of the URL selected by [`chat_display_url`].
///
/// Render planning uses this when the selected URL must outlive the immediate
/// emote visit. Returning one of the emote's existing `Arc<str>` fields avoids
/// copying the URL bytes before visible-request deduplication.
#[inline]
pub fn chat_display_url_shared(emote: &Emote) -> Arc<str> {
    let selected = chat_display_url(emote);
    echo_emote_core::emote_url_shared(emote, selected)
}

/// [`chat_display_url_shared`] for a window with device scale factor `window_scale`.
#[inline]
pub fn chat_display_url_shared_for_scale(emote: &Emote, window_scale: f32) -> Arc<str> {
    let selected = chat_display_url_for_scale(emote, window_scale);
    echo_emote_core::emote_url_shared(emote, selected)
}

/// Build a durable media-source identity for an emote URL variant.
///
/// This is intentionally separate from existing user preference identity keys,
/// which are persisted as `id:Provider` and should not be migrated as part of
/// the source-byte cache.
pub fn media_source_id_for_url(
    emote: &Emote,
    url: &str,
) -> Option<crate::cache::disk_layer::MediaSourceId> {
    if emote.id.is_empty() || url.is_empty() {
        return None;
    }

    let provider = emote.provider.stable_cache_key();
    let variant = crate::cache::disk_layer::MediaVariant::for_emote_url(
        provider,
        echo_emote_core::emote_source_scale(emote, url),
        url,
        emote.is_animated,
    );

    Some(crate::cache::disk_layer::MediaSourceId::new_shared_labels(
        provider,
        Arc::clone(&emote.id),
        variant,
    ))
}

pub fn media_request_for_url(emote: &Emote, url: &str) -> crate::cache::loader::MediaRequest {
    crate::cache::loader::MediaRequest::new(
        echo_emote_core::emote_url_shared(emote, url),
        media_source_id_for_url(emote, url),
    )
}

pub fn media_requests_for_emote_variants(emote: &Emote) -> Vec<crate::cache::loader::MediaRequest> {
    let mut urls = Vec::with_capacity(4);
    urls.push(emote.url.as_ref());
    if let Some(url) = emote.url_1x.as_deref() {
        urls.push(url);
    }
    if let Some(url) = emote.url_2x.as_deref() {
        urls.push(url);
    }
    if let Some(url) = emote.url_4x.as_deref() {
        urls.push(url);
    }
    urls.sort_unstable();
    urls.dedup();
    urls.into_iter()
        .map(|url| media_request_for_url(emote, url))
        .collect()
}

fn collect_emote_source_cache_invalidations(
    emote: &Emote,
    requests: &mut Vec<crate::cache::loader::MediaRequest>,
) {
    requests.extend(media_requests_for_emote_variants(emote));
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SevenTvEmoteSetPatch {
    pub added: Vec<Emote>,
    pub removed: Vec<SevenTvEmoteIdentity>,
    pub updated: Vec<SevenTvEmoteUpdate>,
}

impl SevenTvEmoteSetPatch {
    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
    }

    pub(crate) fn merge(&mut self, mut other: Self) {
        self.added.append(&mut other.added);
        self.removed.append(&mut other.removed);
        self.updated.append(&mut other.updated);
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SevenTvEmoteIdentity {
    pub id: Option<String>,
    pub name: Option<String>,
}

impl SevenTvEmoteIdentity {
    pub(crate) fn is_empty(&self) -> bool {
        self.id.as_deref().unwrap_or_default().is_empty()
            && self.name.as_deref().unwrap_or_default().is_empty()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SevenTvEmoteUpdate {
    pub old: SevenTvEmoteIdentity,
    pub emote: Emote,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SevenTvPatchApplyOutcome {
    pub changed: usize,
    pub unmatched_old_identities: usize,
}

/// Cache entry with TTL tracking
pub type EmoteNameMap = HashMap<Arc<str>, Arc<Emote>>;

/// Every alias of one provider emote id. 7TV sets may hold the same emote
/// under several names, and each alias must stay resolvable.
type EmoteAliases = smallvec::SmallVec<[Arc<Emote>; 1]>;
type ProviderIdentityMap = HashMap<Arc<str>, EmoteAliases>;

#[inline]
fn insert_emote_alias(map: &mut ProviderIdentityMap, emote: Arc<Emote>) {
    let aliases = map.entry(Arc::clone(&emote.id)).or_default();
    if let Some(slot) = aliases.iter_mut().find(|alias| alias.name == emote.name) {
        *slot = emote;
    } else {
        aliases.push(emote);
    }
}

#[inline]
fn find_emote_alias<'a>(
    map: &'a ProviderIdentityMap,
    id: &str,
    name: &str,
) -> Option<&'a Arc<Emote>> {
    map.get(id)?
        .iter()
        .find(|alias| alias.name.as_ref() == name)
}

#[derive(Clone, Default)]
struct EmoteIdentityMap(HashMap<EmoteProvider, ProviderIdentityMap>);

impl EmoteIdentityMap {
    #[inline]
    fn insert(&mut self, emote: Arc<Emote>) {
        insert_emote_alias(self.0.entry(emote.provider).or_default(), emote);
    }

    #[inline]
    fn get(&self, id: &str, provider: EmoteProvider) -> Option<&Arc<Emote>> {
        self.0.get(&provider)?.get(id)?.first()
    }

    #[inline]
    fn get_alias(&self, id: &str, name: &str, provider: EmoteProvider) -> Option<&Arc<Emote>> {
        find_emote_alias(self.0.get(&provider)?, id, name)
    }

    #[inline]
    fn alias_count(&self, id: &str, provider: EmoteProvider) -> usize {
        self.0
            .get(&provider)
            .and_then(|map| map.get(id))
            .map_or(0, |aliases| aliases.len())
    }

    #[inline]
    fn remove_alias(
        &mut self,
        id: &str,
        name: &str,
        provider: EmoteProvider,
    ) -> Option<Arc<Emote>> {
        let provider_map = self.0.get_mut(&provider)?;
        let aliases = provider_map.get_mut(id)?;
        let index = aliases
            .iter()
            .position(|alias| alias.name.as_ref() == name)?;
        let removed = aliases.remove(index);
        if aliases.is_empty() {
            provider_map.remove(id);
        }
        if provider_map.is_empty() {
            self.0.remove(&provider);
        }
        Some(removed)
    }

    #[inline]
    fn remove_provider(&mut self, provider: EmoteProvider) {
        self.0.remove(&provider);
    }

    #[inline]
    fn provider_len(&self, provider: EmoteProvider) -> usize {
        self.0
            .get(&provider)
            .map_or(0, |map| map.values().map(|aliases| aliases.len()).sum())
    }

    #[inline]
    fn values(&self) -> impl Iterator<Item = &Arc<Emote>> {
        self.0.values().flat_map(|map| map.values().flatten())
    }

    #[inline]
    fn len(&self) -> usize {
        self.0
            .values()
            .map(|map| map.values().map(|aliases| aliases.len()).sum::<usize>())
            .sum()
    }
}

impl FromIterator<Arc<Emote>> for EmoteIdentityMap {
    fn from_iter<T: IntoIterator<Item = Arc<Emote>>>(iter: T) -> Self {
        let mut identities = Self::default();
        for emote in iter {
            identities.insert(emote);
        }
        identities
    }
}

/// Re-insert one provider's emotes from the previous cache snapshot after that
/// provider's fetch failed, so an outage keeps stale emotes instead of erasing them.
fn carry_over_provider_emotes(
    previous: &[Arc<Emote>],
    provider: EmoteProvider,
    mut insert: impl FnMut(Arc<Emote>),
) -> usize {
    let mut count = 0;
    for emote in previous.iter().filter(|emote| emote.provider == provider) {
        insert(Arc::clone(emote));
        count += 1;
    }
    count
}

/// Uses Arc<Emote> to avoid cloning on access - major performance optimization
#[derive(Clone)]
struct CacheEntry {
    emotes: EmoteNameMap,
    emotes_by_id: EmoteIdentityMap,
    fetched_at: Instant,
}

const INCREMENTAL_PROVIDER_DIFF_MAX_IDENTITIES: usize = 64;

/// Production-code timing result for the provider-index evaluator.
#[cfg(feature = "provider-index-bench")]
pub struct ProviderIndexBenchResult {
    pub entries: usize,
    pub iterations: usize,
    pub equivalent: bool,
    pub full_median_ns: u64,
    pub incremental_median_ns: u64,
}

#[cfg(feature = "provider-index-bench")]
impl ProviderIndexBenchResult {
    pub fn speedup(&self) -> f64 {
        self.full_median_ns as f64 / self.incremental_median_ns as f64
    }
}

struct ProviderIndexDiff {
    provider: EmoteProvider,
    changed_aliases: HashSet<Arc<str>>,
}

/// Result of a channel emote load, letting callers tell a real load apart from a
/// coalesced no-op (another concurrent load owns the work). The staggered IRC
/// loader relies on this so it never drops its durable retry onto a load that may
/// itself be doomed (e.g. spawned on a preload runtime that has since dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadOutcome {
    /// This call performed the load, or found the cache already fresh; the
    /// channel's lookup maps are current.
    Loaded,
    /// Another concurrent load owned the work; this call did nothing.
    Coalesced,
}

/// Emote cache with global and per-channel emotes
pub struct EmoteCache {
    /// Global emotes from all providers
    global: RwLock<Option<CacheEntry>>,
    /// Per-channel emotes (channel_id -> emotes)
    channels: DashMap<String, CacheEntry>,
    /// User's subscription emotes (from all channels they're subscribed to)
    user_emotes: RwLock<Option<CacheEntry>>,
    /// Twitch user id for the currently cached user-scoped emotes.
    user_emote_owner_id: RwLock<Option<String>>,
    /// Channels currently being loaded (to prevent duplicate concurrent loads)
    loading: Mutex<HashSet<String>>,
    /// User id currently being loaded, used to dedupe identical concurrent loads.
    loading_user_emotes: Mutex<Option<String>>,
    /// Monotonic request id so stale account-refresh tasks cannot overwrite newer caches.
    user_emote_request_generation: AtomicU64,
    /// Serializes the final user-cache publication with clear/revoke work.
    user_emote_commit_lock: Mutex<()>,
    /// Cache TTL (default: 30 minutes)
    ttl: Duration,
    /// Path to the disk cache file (set when app starts)
    cache_path: RwLock<Option<PathBuf>>,
    /// OAuth token for Twitch API (user emotes, etc.)
    token: RwLock<Option<String>>,
    /// Track 7TV emote set IDs per channel for invalidation detection
    /// Key: channel_login (lowercase), Value: emote_set_id
    channel_emote_set_ids: DashMap<String, String>,
}

impl EmoteCache {
    pub fn new() -> Self {
        Self {
            global: RwLock::new(None),
            channels: DashMap::new(),
            user_emotes: RwLock::new(None),
            user_emote_owner_id: RwLock::new(None),
            loading: Mutex::new(HashSet::new()),
            loading_user_emotes: Mutex::new(None),
            user_emote_request_generation: AtomicU64::new(0),
            user_emote_commit_lock: Mutex::new(()),
            ttl: Duration::from_secs(30 * 60),
            cache_path: RwLock::new(None),
            token: RwLock::new(None),
            channel_emote_set_ids: DashMap::new(),
        }
    }
}

impl Default for EmoteCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EmoteCache {
    pub fn is_channel_loading(&self, channel: &str) -> bool {
        self.loading
            .lock_or_recover()
            .contains(&normalize_channel_key(channel))
    }

    #[inline]
    fn insert_emote_name_keys(name_map: &mut EmoteNameMap, emote: Arc<Emote>) {
        name_map.insert(Arc::clone(&emote.name), Arc::clone(&emote));
        let folded = emote.name.to_ascii_lowercase();
        if folded != *emote.name {
            use std::collections::hash_map::Entry;
            match name_map.entry(Arc::from(folded)) {
                Entry::Vacant(slot) => {
                    slot.insert(emote);
                }
                Entry::Occupied(mut slot) => {
                    if slot.get().name.as_ref() != slot.key().as_ref() {
                        slot.insert(emote);
                    }
                }
            }
        }
    }

    #[inline]
    fn insert_emote_into_maps(
        name_map: &mut EmoteNameMap,
        id_map: &mut EmoteIdentityMap,
        emote: Emote,
    ) {
        let emote_arc = Arc::new(emote);
        id_map.insert(Arc::clone(&emote_arc));
        Self::insert_emote_name_keys(name_map, emote_arc);
    }

    #[inline]
    fn insert_emote_arc_into_maps(
        name_map: &mut EmoteNameMap,
        id_map: &mut EmoteIdentityMap,
        emote: Arc<Emote>,
    ) {
        id_map.insert(Arc::clone(&emote));
        Self::insert_emote_name_keys(name_map, emote);
    }

    /// Drop the name keys a removed alias held, handing the folded key to a
    /// remaining alias of the same emote id that still folds to it.
    fn remove_emote_name_keys(
        name_map: &mut EmoteNameMap,
        id_map: &EmoteIdentityMap,
        emote: &Emote,
    ) {
        let is_same = |candidate: &Arc<Emote>| {
            candidate.id == emote.id && candidate.provider == emote.provider
        };
        let folded = emote.name.to_ascii_lowercase();
        if folded != *emote.name && name_map.get(emote.name.as_ref()).is_some_and(is_same) {
            name_map.remove(emote.name.as_ref());
        }
        if !name_map.get(folded.as_str()).is_some_and(is_same) {
            return;
        }
        let siblings = id_map
            .0
            .get(&emote.provider)
            .and_then(|map| map.get(emote.id.as_ref()));
        let heir = siblings.and_then(|aliases| {
            aliases
                .iter()
                .find(|alias| alias.name.as_ref() == folded)
                .or_else(|| {
                    aliases
                        .iter()
                        .find(|alias| alias.name.eq_ignore_ascii_case(&folded))
                })
        });
        match heir {
            Some(heir) => {
                name_map.insert(Arc::from(folded), Arc::clone(heir));
            }
            None => {
                name_map.remove(folded.as_str());
            }
        }
    }

    #[inline]
    fn provider_name_priority(provider: EmoteProvider) -> u8 {
        match provider {
            EmoteProvider::Twitch => 0,
            EmoteProvider::FFZ => 1,
            EmoteProvider::BTTV => 2,
            EmoteProvider::SevenTV => 3,
            EmoteProvider::Emoji => 4,
        }
    }

    /// Rebuild the winner-by-name map from the provider identity inventory.
    /// Later providers intentionally win, matching the full-load merge order.
    fn rebuild_name_map_from_identities(entry: &mut CacheEntry) {
        let mut emotes = entry
            .emotes_by_id
            .values()
            .cloned()
            .collect::<Vec<Arc<Emote>>>();
        emotes.sort_unstable_by(|left, right| {
            Self::provider_name_priority(left.provider)
                .cmp(&Self::provider_name_priority(right.provider))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });

        entry.emotes.clear();
        for emote in emotes {
            Self::insert_emote_name_keys(&mut entry.emotes, emote);
        }
    }

    #[inline]
    fn emote_sort_key(emote: &Emote) -> (u8, &str, &str) {
        (
            Self::provider_name_priority(emote.provider),
            emote.name.as_ref(),
            emote.id.as_ref(),
        )
    }

    fn repair_name_map_aliases(entry: &mut CacheEntry, aliases: &HashSet<Arc<str>>) {
        let mut exact_winners: EmoteNameMap = HashMap::with_capacity(aliases.len());
        let mut folded_winners: EmoteNameMap = HashMap::with_capacity(aliases.len());
        for emote in entry.emotes_by_id.values() {
            if aliases.contains(emote.name.as_ref()) {
                let replace = exact_winners
                    .get(emote.name.as_ref())
                    .is_none_or(|current| {
                        Self::emote_sort_key(current) < Self::emote_sort_key(emote)
                    });
                if replace {
                    exact_winners.insert(Arc::clone(&emote.name), Arc::clone(emote));
                }
            }

            let folded = emote.name.to_ascii_lowercase();
            if folded != *emote.name && aliases.contains(folded.as_str()) {
                let replace = folded_winners.get(folded.as_str()).is_none_or(|current| {
                    Self::emote_sort_key(current) < Self::emote_sort_key(emote)
                });
                if replace {
                    folded_winners.insert(Arc::from(folded), Arc::clone(emote));
                }
            }
        }

        for alias in aliases {
            if let Some(winner) = exact_winners
                .get(alias.as_ref())
                .or_else(|| folded_winners.get(alias.as_ref()))
            {
                entry.emotes.insert(Arc::clone(alias), Arc::clone(winner));
            } else {
                entry.emotes.remove(alias.as_ref());
            }
        }
    }

    fn replace_provider_rows_incrementally<I, E>(
        entry: &mut CacheEntry,
        provider: EmoteProvider,
        emotes: I,
    ) -> Option<ProviderIndexDiff>
    where
        I: IntoIterator<Item = E>,
        E: Into<Arc<Emote>>,
    {
        let previous = entry.emotes_by_id.0.remove(&provider).unwrap_or_default();
        let mut replacement = ProviderIdentityMap::with_capacity(previous.len());
        for emote in emotes {
            let emote = emote.into();
            let emote = find_emote_alias(&previous, &emote.id, &emote.name)
                .filter(|previous| previous.as_ref() == emote.as_ref())
                .cloned()
                .unwrap_or(emote);
            insert_emote_alias(&mut replacement, emote);
        }

        let mut changed_aliases = HashSet::new();
        let mut changed_identity_count = 0usize;
        for old in previous.values().flatten() {
            if find_emote_alias(&replacement, &old.id, &old.name)
                .is_none_or(|new| !Arc::ptr_eq(old, new))
            {
                changed_identity_count += 1;
                changed_aliases.insert(Arc::clone(&old.name));
                let folded = old.name.to_ascii_lowercase();
                if folded != *old.name {
                    changed_aliases.insert(Arc::from(folded));
                }
            }
        }
        for new in replacement.values().flatten() {
            let old = find_emote_alias(&previous, &new.id, &new.name);
            if old.is_none_or(|old| !Arc::ptr_eq(old, new)) {
                if old.is_none() {
                    changed_identity_count += 1;
                }
                changed_aliases.insert(Arc::clone(&new.name));
                let folded = new.name.to_ascii_lowercase();
                if folded != *new.name {
                    changed_aliases.insert(Arc::from(folded));
                }
            }
        }

        entry.emotes_by_id.0.insert(provider, replacement);

        let incremental = changed_identity_count <= INCREMENTAL_PROVIDER_DIFF_MAX_IDENTITIES
            && changed_identity_count.saturating_mul(8) <= entry.emotes_by_id.len().max(1);
        if !incremental {
            Self::rebuild_name_map_from_identities(entry);
            return None;
        }

        Self::repair_name_map_aliases(entry, &changed_aliases);

        Some(ProviderIndexDiff {
            provider,
            changed_aliases,
        })
    }

    fn find_seventv_emote_for_identity(
        entry: &CacheEntry,
        identity: &SevenTvEmoteIdentity,
        match_sibling_alias: bool,
    ) -> Option<Arc<Emote>> {
        let name = identity.name.as_deref().filter(|name| !name.is_empty());
        if let Some(id) = identity.id.as_deref().filter(|id| !id.is_empty()) {
            if let Some(name) = name
                && let Some(emote) = entry
                    .emotes_by_id
                    .get_alias(id, name, EmoteProvider::SevenTV)
            {
                return Some(Arc::clone(emote));
            }
            let aliases = entry.emotes_by_id.alias_count(id, EmoteProvider::SevenTV);
            if match_sibling_alias && aliases > 0 {
                if name.is_some() && aliases > 1 {
                    return None;
                }
                return entry.emotes_by_id.get(id, EmoteProvider::SevenTV).cloned();
            }
        }

        if let Some(name) = identity.name.as_deref().filter(|name| !name.is_empty()) {
            let has_id = identity.id.as_deref().is_some_and(|id| !id.is_empty());
            if let Some(emote) = entry.emotes.get(name)
                && emote.provider == EmoteProvider::SevenTV
                && (!has_id || emote.name.as_ref() == name)
            {
                return Some(Arc::clone(emote));
            }
            let folded = name.to_ascii_lowercase();
            if !has_id
                && folded != name
                && let Some(emote) = entry.emotes.get(folded.as_str())
                && emote.provider == EmoteProvider::SevenTV
            {
                return Some(Arc::clone(emote));
            }
        }

        None
    }

    fn remove_seventv_emote(
        entry: &mut CacheEntry,
        identity: &SevenTvEmoteIdentity,
        match_sibling_alias: bool,
    ) -> Option<Arc<Emote>> {
        let emote = Self::find_seventv_emote_for_identity(entry, identity, match_sibling_alias)?;
        entry
            .emotes_by_id
            .remove_alias(emote.id.as_ref(), emote.name.as_ref(), emote.provider);
        Self::remove_emote_name_keys(&mut entry.emotes, &entry.emotes_by_id, &emote);
        Some(emote)
    }

    fn apply_seventv_patch_to_entry(
        entry: &mut CacheEntry,
        patch: SevenTvEmoteSetPatch,
        source_invalidations: &mut Vec<crate::cache::loader::MediaRequest>,
    ) -> SevenTvPatchApplyOutcome {
        let mut outcome = SevenTvPatchApplyOutcome::default();

        for identity in patch.removed {
            if Self::remove_seventv_emote(entry, &identity, true).is_some() {
                outcome.changed += 1;
            } else {
                outcome.unmatched_old_identities += 1;
            }
        }

        for update in patch.updated {
            let old_identity = if update.old.is_empty() {
                SevenTvEmoteIdentity {
                    id: Some(update.emote.id.to_string()),
                    name: Some(update.emote.name.to_string()),
                }
            } else {
                update.old
            };
            if let Some(old_emote) = Self::remove_seventv_emote(entry, &old_identity, true) {
                collect_emote_source_cache_invalidations(&old_emote, source_invalidations);
                outcome.changed += 1;
            } else {
                outcome.unmatched_old_identities += 1;
            }
            collect_emote_source_cache_invalidations(&update.emote, source_invalidations);
            Self::insert_emote_into_maps(&mut entry.emotes, &mut entry.emotes_by_id, update.emote);
            outcome.changed += 1;
        }

        for emote in patch.added {
            let identity = SevenTvEmoteIdentity {
                id: Some(emote.id.to_string()),
                name: Some(emote.name.to_string()),
            };
            Self::remove_seventv_emote(entry, &identity, false);
            Self::insert_emote_into_maps(&mut entry.emotes, &mut entry.emotes_by_id, emote);
            outcome.changed += 1;
        }

        if outcome.changed > 0 {
            Self::rebuild_name_map_from_identities(entry);
        }
        outcome
    }

    async fn apply_channel_seventv_patch(
        &self,
        channel_login: &str,
        patch: SevenTvEmoteSetPatch,
    ) -> Result<SevenTvPatchApplyOutcome, String> {
        if patch.is_empty() {
            return Ok(SevenTvPatchApplyOutcome::default());
        }

        let cache_key = normalize_channel_key(channel_login);
        let mut source_invalidations = Vec::new();
        let outcome = {
            let Some(mut entry) = self.channels.get_mut(&cache_key) else {
                return Err(format!("no channel emote cache for {}", cache_key));
            };
            Self::apply_seventv_patch_to_entry(&mut entry, patch, &mut source_invalidations)
        };

        if outcome.changed > 0 {
            invalidate_7tv_channel_fetch_cache(&cache_key);
            crate::cache::loader::invalidate_media_sources(source_invalidations).await;
            let rebuild_start = Instant::now();
            rebuild_emote_indices(&cache_key).await;
            record_emote_stage_timed(
                "emotes.channel.apply_7tv_patch.rebuild_index",
                rebuild_start,
                outcome.changed,
            );
            trigger_open_channel_preload(&cache_key);
        }

        Ok(outcome)
    }

    async fn apply_global_seventv_patch(
        &self,
        patch: SevenTvEmoteSetPatch,
    ) -> Result<SevenTvPatchApplyOutcome, String> {
        if patch.is_empty() {
            return Ok(SevenTvPatchApplyOutcome::default());
        }

        let outcome = {
            let mut global = self.global.write_or_recover();
            let Some(entry) = global.as_mut() else {
                return Err("no global emote cache".to_string());
            };
            let mut source_invalidations = Vec::new();
            let outcome =
                Self::apply_seventv_patch_to_entry(entry, patch, &mut source_invalidations);
            (outcome, source_invalidations)
        };

        let (outcome, source_invalidations) = outcome;

        if outcome.changed > 0 {
            invalidate_7tv_global_fetch_cache();
            crate::cache::loader::invalidate_media_sources(source_invalidations).await;
            let rebuild_start = Instant::now();
            rebuild_global_emote_index().await;
            record_emote_stage_timed(
                "emotes.global.apply_7tv_patch.rebuild_index",
                rebuild_start,
                outcome.changed,
            );
        }

        Ok(outcome)
    }

    async fn apply_user_seventv_patch(
        &self,
        patch: SevenTvEmoteSetPatch,
    ) -> Result<SevenTvPatchApplyOutcome, String> {
        if patch.is_empty() {
            return Ok(SevenTvPatchApplyOutcome::default());
        }

        let (outcome, source_invalidations) = {
            let mut user = self.user_emotes.write_or_recover();
            let Some(entry) = user.as_mut() else {
                return Err("no user emote cache".to_string());
            };
            let mut source_invalidations = Vec::new();
            let outcome =
                Self::apply_seventv_patch_to_entry(entry, patch, &mut source_invalidations);
            (outcome, source_invalidations)
        };

        if outcome.changed > 0 {
            crate::cache::loader::invalidate_media_sources(source_invalidations).await;
            let rebuild_start = Instant::now();
            rebuild_user_emote_index().await;
            record_emote_stage_timed(
                "emotes.user.apply_7tv_patch.rebuild_index",
                rebuild_start,
                outcome.changed,
            );
        }

        Ok(outcome)
    }

    /// Replace all emotes from a single provider for a channel and rebuild indices.
    async fn apply_channel_provider_emotes<I, E>(
        &self,
        cache_key: &str,
        channel_login: &str,
        provider: EmoteProvider,
        emotes: I,
        emote_set_id: Option<String>,
    ) -> usize
    where
        I: IntoIterator<Item = E>,
        E: Into<Arc<Emote>>,
    {
        let mut emotes = emotes.into_iter().peekable();
        let preserve_existing_seventv =
            provider == EmoteProvider::SevenTV && emotes.peek().is_none() && emote_set_id.is_none();
        let provider_count = self
            .apply_channel_provider_rows(
                cache_key,
                channel_login,
                provider,
                emotes,
                preserve_existing_seventv,
            )
            .await;

        if provider == EmoteProvider::SevenTV
            && let Some(set_id) = emote_set_id
        {
            self.channel_emote_set_ids
                .insert(cache_key.to_string(), set_id.clone());
            crate::backend::emotes::seventv_eventapi::subscribe_channel_set(cache_key, &set_id);
        }

        provider_count
    }

    async fn apply_channel_provider_rows<I, E>(
        &self,
        cache_key: &str,
        channel_login: &str,
        provider: EmoteProvider,
        emotes: I,
        preserve_existing_provider: bool,
    ) -> usize
    where
        I: IntoIterator<Item = E>,
        E: Into<Arc<Emote>>,
    {
        let emotes = emotes.into_iter();
        let (provider_count, incremental_diff) = {
            match self.channels.get_mut(cache_key) {
                Some(mut entry) => {
                    let mut incremental_diff = None;
                    if preserve_existing_provider {
                        crate::log_emotes!(
                            debug,
                            "Preserving existing 7TV emotes for {} because deferred payload was empty and missing emote_set_id",
                            channel_login
                        );
                        record_emote_stage_duration(
                            "emotes.channel.apply_provider.7tv_preserve_existing",
                            Duration::ZERO,
                            0,
                        );
                    } else {
                        incremental_diff =
                            Self::replace_provider_rows_incrementally(&mut entry, provider, emotes);
                    }
                    (entry.emotes_by_id.provider_len(provider), incremental_diff)
                }
                _ => {
                    let mut emote_map: EmoteNameMap = HashMap::new();
                    let mut emote_map_by_id: EmoteIdentityMap = EmoteIdentityMap::default();
                    for emote in emotes {
                        Self::insert_emote_arc_into_maps(
                            &mut emote_map,
                            &mut emote_map_by_id,
                            emote.into(),
                        );
                    }
                    let count = emote_map_by_id.len();
                    self.channels.insert(
                        cache_key.to_string(),
                        CacheEntry {
                            emotes: emote_map,
                            emotes_by_id: emote_map_by_id,
                            fetched_at: Instant::now()
                                .checked_sub(self.ttl)
                                .unwrap_or_else(Instant::now),
                        },
                    );
                    (count, None)
                }
            }
        };

        let rebuild_start = Instant::now();
        if let Some(diff) = incremental_diff {
            publish_channel_provider_diff(cache_key, diff).await;
        } else {
            rebuild_emote_indices(cache_key).await;
        }
        record_emote_stage_timed(
            "emotes.channel.apply_provider.rebuild_index",
            rebuild_start,
            provider_count,
        );
        trigger_open_channel_preload(cache_key);
        provider_count
    }

    async fn clear_channel_provider_rows(&self, cache_key: &str, provider: EmoteProvider) -> usize {
        let removed_count = match self.channels.get_mut(cache_key) {
            Some(mut entry) => {
                let removed = entry.emotes_by_id.provider_len(provider);
                entry.emotes_by_id.remove_provider(provider);
                Self::rebuild_name_map_from_identities(&mut entry);
                removed
            }
            _ => 0,
        };

        let rebuild_start = Instant::now();
        rebuild_emote_indices(cache_key).await;
        record_emote_stage_timed(
            "emotes.channel.clear_provider.rebuild_index",
            rebuild_start,
            removed_count,
        );
        removed_count
    }

    /// Replace global emotes for one provider and rebuild global indices.
    async fn apply_global_provider_emotes<I, E>(&self, provider: EmoteProvider, emotes: I) -> usize
    where
        I: IntoIterator<Item = E>,
        E: Into<Arc<Emote>>,
    {
        let provider_count = {
            let mut global = self.global.write_or_recover();
            if let Some(entry) = global.as_mut() {
                entry.emotes_by_id.remove_provider(provider);
                for emote in emotes {
                    let emote_arc = emote.into();
                    entry.emotes_by_id.insert(emote_arc);
                }
                Self::rebuild_name_map_from_identities(entry);
                entry.emotes_by_id.provider_len(provider)
            } else {
                let mut emote_map: EmoteNameMap = HashMap::new();
                let mut emote_map_by_id: EmoteIdentityMap = EmoteIdentityMap::default();
                for emote in emotes {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        emote.into(),
                    );
                }
                let count = emote_map_by_id.len();
                *global = Some(CacheEntry {
                    emotes: emote_map,
                    emotes_by_id: emote_map_by_id,
                    fetched_at: Instant::now()
                        .checked_sub(self.ttl)
                        .unwrap_or_else(Instant::now),
                });
                count
            }
        };

        let rebuild_start = Instant::now();
        rebuild_global_emote_index().await;
        record_emote_stage_timed(
            "emotes.global.apply_provider.rebuild_index",
            rebuild_start,
            provider_count,
        );
        provider_count
    }

    /// Replace all emotes from a single provider in the authenticated user's
    /// inventory while preserving every other provider.
    async fn apply_user_provider_emotes<I, E>(&self, provider: EmoteProvider, emotes: I) -> usize
    where
        I: IntoIterator<Item = E>,
        E: Into<Arc<Emote>>,
    {
        let provider_count = {
            let mut user = self.user_emotes.write_or_recover();
            if let Some(entry) = user.as_mut() {
                entry.emotes_by_id.remove_provider(provider);
                for emote in emotes {
                    let emote_arc = emote.into();
                    entry.emotes_by_id.insert(emote_arc);
                }
                Self::rebuild_name_map_from_identities(entry);
                entry.emotes_by_id.provider_len(provider)
            } else {
                let mut emote_map = HashMap::new();
                let mut emote_map_by_id = EmoteIdentityMap::default();
                for emote in emotes {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        emote.into(),
                    );
                }
                let count = emote_map_by_id.len();
                *user = Some(CacheEntry {
                    emotes: emote_map,
                    emotes_by_id: emote_map_by_id,
                    fetched_at: Instant::now(),
                });
                count
            }
        };

        let rebuild_start = Instant::now();
        rebuild_user_emote_index().await;
        record_emote_stage_timed(
            "emotes.user.apply_provider.rebuild_index",
            rebuild_start,
            provider_count,
        );
        provider_count
    }

    /// Set the OAuth token for Twitch API requests
    pub async fn set_token(&self, token: String) {
        *self.token.write_or_recover() = Some(token);
    }

    /// Clear the OAuth token (used when entering anonymous mode)
    pub async fn clear_token(&self) {
        *self.token.write_or_recover() = None;
        crate::log_emotes!(info, "Token cleared");
    }

    /// Set the cache file path (call this when app starts with app_data_dir)
    pub async fn set_cache_path(&self, path: PathBuf) {
        let mut cache_path = self.cache_path.write_or_recover();
        *cache_path = Some(path);
    }

    /// Load one part of the disk cache into memory, optionally hydrating only
    /// a scoped set of channel caches. User emotes hydrate only for the
    /// matching, still-current authenticated owner.
    pub(crate) async fn load_from_disk_scoped(
        &self,
        allowed_channels: Option<HashSet<String>>,
        parts: DiskCacheParts,
    ) -> (bool, bool, usize) {
        let disk_user_request_generation =
            self.user_emote_request_generation.load(Ordering::SeqCst);
        let disk_auth_fence = crate::backend::auth::capture_authenticated_resource_fence();
        let allowed_channels = allowed_channels.map(|channels| {
            channels
                .into_iter()
                .map(|channel| channel.trim().trim_start_matches('#').to_ascii_lowercase())
                .filter(|channel| !channel.is_empty())
                .collect::<HashSet<_>>()
        });

        let Some(path) = self.cache_path.read_or_recover().as_ref().cloned() else {
            crate::log_emotes!(info, "No cache path set, skipping disk load");
            return (false, false, 0);
        };

        let legacy_path = legacy_emote_cache_path(&path);
        let total_start = Instant::now();

        let path_owned = path;
        let legacy_path_owned = legacy_path;
        let blocking_read_start = Instant::now();
        let cache_result = tokio::task::spawn_blocking(move || -> Result<EmoteCacheFile, &'static str> {
            match read_emote_cache_with_legacy_fallback(&path_owned, &legacy_path_owned) {
                Ok(EmoteCacheReadSource::Current(cache_file)) => Ok(cache_file),
                Ok(EmoteCacheReadSource::Legacy(legacy_cache)) => {
                    match write_current_emote_cache_atomic(&path_owned, &legacy_cache) {
                        Ok(size) => crate::log_emotes!(
                            info,
                            "Migrated legacy Bincode emote cache to Postcard at {:?} ({} bytes); preserving {:?}",
                            &path_owned,
                            size,
                            &legacy_path_owned
                        ),
                        Err(error) => crate::log_emotes!(
                            warn,
                            "Loaded legacy Bincode emote cache but could not publish Postcard migration: {}",
                            error
                        ),
                    }
                    Ok(legacy_cache)
                }
                Err(EmoteCacheLoadError::Legacy(EmoteCacheReadError::NotFound)) => {
                    crate::log_emotes!(
                        info,
                        "No current or legacy emote cache found at {:?} / {:?}",
                        &path_owned,
                        &legacy_path_owned
                    );
                    Err("cache_not_found")
                }
                Err(EmoteCacheLoadError::Current(error)) => {
                    crate::log_emotes!(
                        warn,
                        "Refusing legacy fallback because current Postcard cache at {:?} could not be read safely: {}",
                        &path_owned,
                        error
                    );
                    Err("current_cache_unusable")
                }
                Err(EmoteCacheLoadError::Legacy(error)) => {
                    crate::log_emotes!(
                        warn,
                        "Current Postcard emote cache is absent, but legacy Bincode cache at {:?} could not be read safely: {}",
                        &legacy_path_owned,
                        error
                    );
                    Err("legacy_cache_unusable")
                }
            }
        })
        .await;
        record_emote_stage_timed("emotes.disk.load.blocking_read", blocking_read_start, 0);

        let cache_file = match cache_result {
            Ok(Ok(cf)) => cf,
            _ => return (false, false, 0),
        };
        let disk_user_owner_id = cache_file.user_emote_owner_id.clone();
        let (global_emotes, user_emotes, channel_emotes) = match parts {
            DiskCacheParts::Shared => (cache_file.global_emotes, None, cache_file.channel_emotes),
            DiskCacheParts::User => (None, cache_file.user_emotes, Default::default()),
        };

        let mut global_loaded = false;
        let mut user_loaded = false;
        let mut channels_loaded = 0;
        let mut global_emote_count = 0usize;
        let mut user_emote_count = 0usize;
        let mut channel_emote_count = 0usize;
        let scoped_channel_load = allowed_channels.is_some();
        let mut channels_skipped_by_scope = 0usize;

        let calculate_fetched_at = |is_fresh: bool| -> Instant {
            if is_fresh {
                let refresh_delay = Duration::from_secs(DISK_CACHE_REFRESH_DELAY_SECS);
                Instant::now()
                    .checked_sub(self.ttl)
                    .unwrap_or(Instant::now())
                    .checked_add(refresh_delay)
                    .unwrap_or(Instant::now())
            } else {
                Instant::now()
                    .checked_sub(self.ttl + Duration::from_secs(1))
                    .unwrap_or(Instant::now())
            }
        };

        let hydrate_global_start = Instant::now();
        if let Some(cached) = global_emotes {
            if !cached.is_expired(GLOBAL_EMOTES_DISK_TTL_SECS) {
                let is_fresh = cached.is_fresh_enough_to_delay();
                let fetched_at = calculate_fetched_at(is_fresh);
                crate::log_emotes!(
                    info,
                    "Global emotes from disk: {} emotes, cache_age={}s, fresh={}, will_refresh_immediately={}",
                    cached.emotes.len(),
                    crate::utils::unix_now_secs().saturating_sub(cached.saved_at),
                    is_fresh,
                    !is_fresh
                );
                let mut global = self.global.write_or_recover();
                let mut arc_emotes: EmoteNameMap = HashMap::with_capacity(cached.emotes.len() * 2);
                for emote in cached.emotes.into_values() {
                    Self::insert_emote_name_keys(&mut arc_emotes, Arc::new(emote));
                }
                let arc_emotes_by_id: EmoteIdentityMap = arc_emotes.values().cloned().collect();
                *global = Some(CacheEntry {
                    emotes: arc_emotes,
                    emotes_by_id: arc_emotes_by_id,
                    fetched_at,
                });
                global_emote_count = global.as_ref().map(|e| e.emotes_by_id.len()).unwrap_or(0);
                global_loaded = true;
            } else {
                crate::log_emotes!(info, "Global emotes cache expired");
            }
        }
        record_emote_stage_timed(
            "emotes.disk.load.hydrate_global",
            hydrate_global_start,
            global_emote_count,
        );

        let hydrate_user_start = Instant::now();
        if let Some(cached) = user_emotes {
            if !cached.is_expired(USER_EMOTES_DISK_TTL_SECS) {
                let is_fresh = cached.is_fresh_enough_to_delay();
                let fetched_at = calculate_fetched_at(is_fresh);
                crate::log_emotes!(
                    info,
                    "User emotes from disk: {} emotes, cache_age={}s, fresh={}, will_refresh_immediately={}",
                    cached.emotes.len(),
                    crate::utils::unix_now_secs().saturating_sub(cached.saved_at),
                    is_fresh,
                    !is_fresh
                );
                let owner_matches = disk_user_owner_id.as_deref().is_some_and(|owner| {
                    crate::backend::auth::active_authenticated_user_id().as_deref() == Some(owner)
                });
                if owner_matches && disk_auth_fence.is_some() {
                    let mut arc_emotes: EmoteNameMap =
                        HashMap::with_capacity(cached.emotes.len() * 2);
                    for emote in cached.emotes.into_values() {
                        Self::insert_emote_name_keys(&mut arc_emotes, Arc::new(emote));
                    }
                    let arc_emotes_by_id: EmoteIdentityMap = arc_emotes.values().cloned().collect();
                    let published = disk_auth_fence.as_ref().is_some_and(|fence| {
                        fence
                            .commit_if_current(|| {
                                let _user_commit = self.user_emote_commit_lock.lock_or_recover();
                                if self.user_emote_request_generation.load(Ordering::SeqCst)
                                    != disk_user_request_generation
                                {
                                    return false;
                                }
                                let mut user_emotes = self.user_emotes.write_or_recover();
                                *user_emotes = Some(CacheEntry {
                                    emotes: arc_emotes,
                                    emotes_by_id: arc_emotes_by_id,
                                    fetched_at,
                                });
                                *self.user_emote_owner_id.write_or_recover() =
                                    disk_user_owner_id.clone();
                                user_emote_count = user_emotes
                                    .as_ref()
                                    .map(|entry| entry.emotes_by_id.len())
                                    .unwrap_or(0);
                                true
                            })
                            .unwrap_or(false)
                    });
                    if published {
                        user_loaded = true;
                    } else {
                        crate::log_emotes!(
                            info,
                            "Skipped disk user-emote hydration after its account fence changed"
                        );
                    }
                } else {
                    crate::log_emotes!(
                        info,
                        "Skipped disk user-emote hydration for a stale, anonymous, or different account"
                    );
                }
            } else {
                crate::log_emotes!(info, "User emotes cache expired");
            }
        }
        record_emote_stage_timed(
            "emotes.disk.load.hydrate_user",
            hydrate_user_start,
            user_emote_count,
        );

        let mut loaded_channels = Vec::new();
        let hydrate_channels_start = Instant::now();
        for (channel, cached) in channel_emotes {
            let cache_key = channel.to_lowercase();
            if let Some(allowed) = allowed_channels.as_ref()
                && !allowed.contains(&cache_key)
            {
                channels_skipped_by_scope += 1;
                continue;
            }

            if !cached.is_expired(CHANNEL_EMOTES_DISK_STALE_SERVE_MAX_SECS) {
                loaded_channels.push(cache_key.clone());
                let is_fresh = cached.is_fresh_enough_to_delay();
                let fetched_at = channel_disk_cache_fetched_at(is_fresh, self.ttl);
                let mut arc_emotes: EmoteNameMap = HashMap::with_capacity(cached.emotes.len() * 2);
                for emote in cached.emotes.into_values() {
                    Self::insert_emote_name_keys(&mut arc_emotes, Arc::new(emote));
                }
                let arc_emotes_by_id: EmoteIdentityMap = arc_emotes.values().cloned().collect();
                channel_emote_count += arc_emotes_by_id.len();
                self.channels.insert(
                    cache_key,
                    CacheEntry {
                        emotes: arc_emotes,
                        emotes_by_id: arc_emotes_by_id,
                        fetched_at,
                    },
                );
                channels_loaded += 1;
            }
        }
        record_emote_stage_timed(
            "emotes.disk.load.hydrate_channels",
            hydrate_channels_start,
            channel_emote_count,
        );

        let elapsed = total_start.elapsed();
        crate::log_emotes!(
            info,
            "Loaded from disk in {:?}: global={}, user={}, channels={}",
            elapsed,
            global_loaded,
            user_loaded,
            channels_loaded
        );
        if scoped_channel_load {
            crate::log_emotes!(
                info,
                "Scoped channel emote cache load: loaded={}, skipped_by_scope={}",
                channels_loaded,
                channels_skipped_by_scope
            );
        }

        let rebuild_indices_start = Instant::now();
        if global_loaded {
            rebuild_global_emote_index().await;
        }
        if user_loaded {
            rebuild_user_emote_index().await;
        }
        for channel in &loaded_channels {
            rebuild_emote_indices(channel).await;
        }
        record_emote_stage_timed(
            "emotes.disk.load.rebuild_indices",
            rebuild_indices_start,
            loaded_channels.len(),
        );

        let preload_trigger_start = Instant::now();
        let preload_channel_count = loaded_channels.len();
        if !loaded_channels.is_empty() && !scoped_channel_load {
            crate::log_emotes!(
                info,
                "Triggering image preload for {} disk-cached channels",
                loaded_channels.len()
            );
            crate::cache::loader::trigger_channels_preload_batch(loaded_channels);
        } else if !scoped_channel_load && (global_loaded || user_loaded) {
            crate::log_emotes!(
                info,
                "No channels to preload, collecting global/user emote URLs"
            );
            let urls: Vec<Arc<str>> = {
                let mut urls = Vec::new();
                if let Some(entry) = self.global.read_or_recover().as_ref() {
                    for emote in entry.emotes.values() {
                        urls.push(Arc::clone(emote.url_1x.as_ref().unwrap_or(&emote.url)));
                    }
                }
                if let Some(entry) = self.user_emotes.read_or_recover().as_ref() {
                    for emote in entry.emotes.values() {
                        urls.push(Arc::clone(emote.url_1x.as_ref().unwrap_or(&emote.url)));
                    }
                }
                urls
            };
            if !urls.is_empty() {
                crate::log_emotes!(info, "Preloading {} global/user emote images", urls.len());
                crate::cache::loader::trigger_preload_with_source(
                    urls,
                    crate::cache::loader::PreloadSource::DiskCacheLoad,
                );
            }
        }
        record_emote_stage_timed(
            "emotes.disk.load.trigger_preload",
            preload_trigger_start,
            preload_channel_count,
        );
        record_emote_stage_duration(
            "emotes.disk.load.total",
            elapsed,
            global_emote_count + user_emote_count + channel_emote_count,
        );

        (global_loaded, user_loaded, channels_loaded)
    }

    /// Save current in-memory emotes to disk cache
    pub async fn save_to_disk(&self) -> Result<(), String> {
        let _save = EMOTE_CACHE_SAVE_LOCK.lock().await;
        let Some(path) = self.cache_path.read_or_recover().clone() else {
            return Err("No cache path set".to_string());
        };

        let total_start = Instant::now();
        let snapshot_start = Instant::now();

        let global_emotes = {
            let global = self.global.read_or_recover();
            global.as_ref().map(|entry| {
                let emotes: HashMap<String, Emote> = entry
                    .emotes
                    .iter()
                    .filter(|(k, v)| k.as_ref() == v.name.as_ref())
                    .map(|(k, v)| (k.to_string(), (**v).clone()))
                    .collect();
                CachedEmoteSet::new(emotes)
            })
        };
        let global_snapshot_count = global_emotes
            .as_ref()
            .map(|set| set.emotes.len())
            .unwrap_or(0);

        let (user_emotes, user_emote_owner_id) = {
            let user = self.user_emotes.read_or_recover();
            let owner_id = self.user_emote_owner_id.read_or_recover().clone();
            let emotes = user.as_ref().map(|entry| {
                let emotes: HashMap<String, Emote> = entry
                    .emotes
                    .iter()
                    .filter(|(k, v)| k.as_ref() == v.name.as_ref())
                    .map(|(k, v)| (k.to_string(), (**v).clone()))
                    .collect();
                CachedEmoteSet::new(emotes)
            });
            (emotes, owner_id.filter(|_| user.is_some()))
        };
        let user_snapshot_count = user_emotes
            .as_ref()
            .map(|set| set.emotes.len())
            .unwrap_or(0);

        let mut channel_emotes = HashMap::new();
        let mut channel_snapshot_count = 0usize;
        for entry in self.channels.iter() {
            if CHANNEL_CATALOG_PENDING
                .get(entry.key())
                .is_some_and(|token| token.load(Ordering::Acquire))
            {
                continue;
            }
            let emotes: HashMap<String, Emote> = entry
                .value()
                .emotes
                .iter()
                .filter(|(k, v)| k.as_ref() == v.name.as_ref())
                .map(|(k, v)| (k.to_string(), (**v).clone()))
                .collect();
            channel_snapshot_count += emotes.len();
            channel_emotes.insert(entry.key().clone(), CachedEmoteSet::new(emotes));
        }

        let cache_file = EmoteCacheFile {
            version: EMOTE_CACHE_VERSION,
            global_emotes,
            user_emotes,
            user_emote_owner_id,
            channel_emotes,
        };
        record_emote_stage_timed(
            "emotes.disk.save.snapshot",
            snapshot_start,
            global_snapshot_count + user_snapshot_count + channel_snapshot_count,
        );

        let path_owned = path.clone();
        let write_blocking_start = Instant::now();
        let write_result = tokio::task::spawn_blocking(move || {
            write_current_emote_cache_atomic(&path_owned, &cache_file)
        })
        .await;
        record_emote_stage_timed("emotes.disk.save.blocking_write", write_blocking_start, 0);

        let file_size = match write_result {
            Ok(Ok(size)) => size,
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(format!("Task panicked: {}", e)),
        };

        let elapsed = total_start.elapsed();
        crate::log_emotes!(
            info,
            "Saved to disk in {:?}: {} bytes ({} KB)",
            elapsed,
            file_size,
            file_size / 1024
        );
        record_emote_stage_duration(
            "emotes.disk.save.total",
            elapsed,
            global_snapshot_count + user_snapshot_count + channel_snapshot_count,
        );

        Ok(())
    }

    /// Load user's emotes from all providers:
    /// - Twitch subscription emotes (from all channels they're subscribed to)
    /// - 7TV personal emotes (subscriber feature - up to 5 emotes usable globally)
    pub async fn load_user_emotes(&self, user_id: &str, token: &str) -> Result<(), String> {
        self.load_user_emotes_impl(
            user_id,
            token,
            Some(USER_EMOTES_SYNC_TIMEOUT_MS),
            true,
            false,
        )
        .await
    }

    async fn load_user_emotes_impl(
        &self,
        user_id: &str,
        token: &str,
        sync_timeout_ms: Option<u64>,
        allow_deferred: bool,
        force_refresh: bool,
    ) -> Result<(), String> {
        let total_start = Instant::now();
        let Some(auth_fence) = crate::backend::auth::capture_authenticated_resource_fence() else {
            crate::log_emotes!(debug, "Skipping user-emote load while anonymous");
            return Ok(());
        };
        if !auth_fence.matches_token(token) {
            crate::log_emotes!(debug, "Skipping user-emote load for stale credentials");
            return Ok(());
        }

        let mut stale_user_emotes: Vec<Arc<Emote>> = Vec::new();
        let mut had_existing_user_cache = false;
        let cached_owner_id = self.user_emote_owner_id.read_or_recover().clone();
        let cached_owner_matches_user = cached_owner_id.as_deref() == Some(user_id);
        let cached_owner_unknown = cached_owner_id.is_none();
        {
            let user_emotes = self.user_emotes.read_or_recover();
            if let Some(entry) = user_emotes.as_ref() {
                if cached_owner_matches_user || cached_owner_unknown {
                    had_existing_user_cache = true;
                    stale_user_emotes.extend(entry.emotes_by_id.values().cloned());
                    let cached_has_twitch_sub_emotes = entry.emotes_by_id.values().any(|e| {
                        e.provider == EmoteProvider::Twitch
                            && e.channel
                                .as_deref()
                                .map(|ch| ch.starts_with("sub:"))
                                .unwrap_or(false)
                    });
                    if cached_owner_matches_user
                        && !force_refresh
                        && !Self::needs_refresh(entry, self.ttl)
                    {
                        if cached_has_twitch_sub_emotes {
                            record_emote_stage_duration(
                                "emotes.user.load.cache_hit",
                                Duration::ZERO,
                                0,
                            );
                            return Ok(());
                        }
                        crate::log_emotes!(
                            info,
                            "User emote cache hit but no Twitch sub emotes present; forcing refresh"
                        );
                    }
                    if cached_owner_unknown && !stale_user_emotes.is_empty() {
                        crate::log_emotes!(
                            info,
                            "User emote cache owner is unknown; preserving {} cached emotes as timeout fallback while refreshing for authenticated user_id: {}",
                            stale_user_emotes.len(),
                            user_id
                        );
                    }
                } else {
                    crate::log_emotes!(
                        info,
                        "User emote cache owner is unknown or different; refreshing for authenticated user_id: {}",
                        user_id
                    );
                }
            }
        }

        if sync_timeout_ms.is_some() && crate::backend::auth::is_token_expired() {
            record_emote_stage_duration("emotes.user.fetch.auth_pending_skip", Duration::ZERO, 0);
            crate::log_emotes!(
                info,
                "Skipping sync user emote fetch for user_id {}: token expired, awaiting refresh",
                user_id
            );
            return Ok(());
        }

        let request_generation = {
            let mut loading = self.loading_user_emotes.lock_or_recover();
            match loading.as_deref() {
                Some(active_user_id) if active_user_id == user_id => return Ok(()),
                Some(active_user_id) => {
                    crate::log_emotes!(
                        info,
                        "User emote refresh for {} is in flight; starting refresh for new authenticated user {}",
                        active_user_id,
                        user_id
                    );
                }
                None => {}
            }
            *loading = Some(user_id.to_string());
            self.user_emote_request_generation
                .fetch_add(1, Ordering::SeqCst)
                + 1
        };

        // Clears the in-flight marker even when this future is dropped mid-fetch
        // (deferred refresh timeout), so later refreshes are not skipped forever.
        struct UserEmoteLoadingGuard<'a> {
            loading: &'a Mutex<Option<String>>,
            user_id: &'a str,
        }

        impl Drop for UserEmoteLoadingGuard<'_> {
            fn drop(&mut self) {
                let mut loading = self.loading.lock_or_recover();
                if loading.as_deref() == Some(self.user_id) {
                    *loading = None;
                }
            }
        }

        let loading_guard = UserEmoteLoadingGuard {
            loading: &self.loading_user_emotes,
            user_id,
        };

        enum ProviderFetchResult<T> {
            Ready(Result<T, String>),
            TimedOut,
        }

        let fetch_parallel_start = Instant::now();
        let (twitch_result, seventv_result) = tokio::join!(
            async {
                let started_at = Instant::now();
                if let Some(timeout_ms) = sync_timeout_ms {
                    match tokio::time::timeout(
                        Duration::from_millis(timeout_ms),
                        fetch_twitch_user_emotes(user_id, token),
                    )
                    .await
                    {
                        Ok(result) => {
                            let items = result.as_ref().map(|(v, _)| v.len()).unwrap_or(0);
                            record_emote_stage_timed("emotes.user.fetch.twitch", started_at, items);
                            ProviderFetchResult::Ready(result)
                        }
                        Err(_) => {
                            record_emote_stage_timed(
                                "emotes.user.fetch.twitch.timeout",
                                started_at,
                                0,
                            );
                            ProviderFetchResult::TimedOut
                        }
                    }
                } else {
                    let result = fetch_twitch_user_emotes(user_id, token).await;
                    let items = result.as_ref().map(|(v, _)| v.len()).unwrap_or(0);
                    record_emote_stage_timed("emotes.user.fetch.twitch", started_at, items);
                    ProviderFetchResult::Ready(result)
                }
            },
            async {
                let started_at = Instant::now();
                if sync_timeout_ms.is_some() {
                    match tokio::time::timeout(
                        Duration::from_millis(USER_SEVENTV_PERSONAL_SYNC_TIMEOUT_MS),
                        fetch_7tv_personal_emotes(user_id),
                    )
                    .await
                    {
                        Ok(result) => {
                            let items = result
                                .as_ref()
                                .map(SevenTvExactSetFetchOutcome::emote_count)
                                .unwrap_or(0);
                            record_emote_stage_timed(
                                "emotes.user.fetch.7tv_personal",
                                started_at,
                                items,
                            );
                            ProviderFetchResult::Ready(result)
                        }
                        Err(_) => {
                            record_emote_stage_timed(
                                "emotes.user.fetch.7tv_personal.timeout",
                                started_at,
                                0,
                            );
                            ProviderFetchResult::TimedOut
                        }
                    }
                } else {
                    let result = fetch_7tv_personal_emotes(user_id).await;
                    let items = result
                        .as_ref()
                        .map(SevenTvExactSetFetchOutcome::emote_count)
                        .unwrap_or(0);
                    record_emote_stage_timed("emotes.user.fetch.7tv_personal", started_at, items);
                    ProviderFetchResult::Ready(result)
                }
            }
        );
        record_emote_stage_timed("emotes.user.fetch.parallel", fetch_parallel_start, 0);

        let twitch_timed_out = matches!(twitch_result, ProviderFetchResult::TimedOut);
        let seventv_timed_out = matches!(seventv_result, ProviderFetchResult::TimedOut);
        let should_schedule_deferred =
            (twitch_timed_out || seventv_timed_out) && sync_timeout_ms.is_some() && allow_deferred;
        if (twitch_timed_out || seventv_timed_out) && sync_timeout_ms.is_some() {
            record_emote_stage_timed(
                "emotes.user.fetch.parallel.timeout",
                fetch_parallel_start,
                0,
            );
        }

        if self.user_emote_request_generation.load(Ordering::SeqCst) != request_generation {
            crate::log_emotes!(
                info,
                "Discarding stale user emote refresh for user_id {} because a newer account refresh started",
                user_id
            );
            return Ok(());
        }

        drop(loading_guard);
        if should_schedule_deferred {
            self.schedule_deferred_user_emote_refresh(user_id.to_string(), token.to_string());
        }

        let mut emote_map: EmoteNameMap = HashMap::new();
        let mut emote_map_by_id: EmoteIdentityMap = EmoteIdentityMap::default();

        match twitch_result {
            ProviderFetchResult::Ready(Ok((emotes, complete))) => {
                for emote in emotes {
                    Self::insert_emote_into_maps(&mut emote_map, &mut emote_map_by_id, emote);
                }
                if !complete {
                    let mut preserved = 0usize;
                    for emote in stale_user_emotes
                        .iter()
                        .filter(|e| e.provider == EmoteProvider::Twitch)
                    {
                        let name_taken = emote_map
                            .get(emote.name.as_ref())
                            .is_some_and(|fresh| fresh.name == emote.name);
                        if !name_taken
                            && emote_map_by_id
                                .get(&emote.id, EmoteProvider::Twitch)
                                .is_none()
                        {
                            Self::insert_emote_arc_into_maps(
                                &mut emote_map,
                                &mut emote_map_by_id,
                                Arc::clone(emote),
                            );
                            preserved += 1;
                        }
                    }
                    crate::log_emotes!(
                        warn,
                        "Twitch user emote fetch was incomplete; kept {} cached user emotes beside the partial result",
                        preserved
                    );
                }
            }
            ProviderFetchResult::Ready(Err(e)) => {
                crate::log_emotes!(warn, "Failed to load Twitch user emotes: {}", e);
                let mut preserved = 0usize;
                for emote in stale_user_emotes
                    .iter()
                    .filter(|e| e.provider == EmoteProvider::Twitch)
                {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        Arc::clone(emote),
                    );
                    preserved += 1;
                }
                if preserved > 0 {
                    crate::log_emotes!(
                        info,
                        "Preserved {} cached Twitch user emotes after fetch error",
                        preserved
                    );
                }
            }
            ProviderFetchResult::TimedOut => {
                let mut preserved = 0usize;
                for emote in stale_user_emotes
                    .iter()
                    .filter(|e| e.provider == EmoteProvider::Twitch)
                {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        Arc::clone(emote),
                    );
                    preserved += 1;
                }
                crate::log_emotes!(
                    info,
                    "Twitch user emote fetch timed out; preserved {} cached user emotes until deferred refresh",
                    preserved
                );
            }
        }

        match seventv_result {
            ProviderFetchResult::Ready(Ok(SevenTvExactSetFetchOutcome::Present {
                emotes,
                emote_set_id,
            })) => {
                crate::log_emotes!(info, "Loaded {} 7TV personal emotes", emotes.len());
                if let Some(emote_set_id) = emote_set_id {
                    seventv_eventapi::subscribe_channel_set(
                        PERSONAL_SEVENTV_CHANNEL,
                        &emote_set_id,
                    );
                } else {
                    seventv_eventapi::clear_channel_set_subscription(PERSONAL_SEVENTV_CHANNEL);
                }
                for emote in emotes {
                    Self::insert_emote_arc_into_maps(&mut emote_map, &mut emote_map_by_id, emote);
                }
            }
            ProviderFetchResult::Ready(Ok(SevenTvExactSetFetchOutcome::TerminallyAbsent {
                emote_set_id,
            })) => {
                if seventv_eventapi::retire_terminal_channel_fetch(
                    PERSONAL_SEVENTV_CHANNEL,
                    &emote_set_id,
                    None,
                ) {
                    crate::backend::metrics::record_emote_event(
                        "emotes.user.fetch.7tv_personal.terminal_cleared",
                    );
                    crate::log_emotes!(
                        info,
                        "7TV personal set {} is terminally absent; clearing personal provider rows",
                        emote_set_id
                    );
                } else {
                    for emote in stale_user_emotes
                        .iter()
                        .filter(|e| e.provider == EmoteProvider::SevenTV)
                    {
                        Self::insert_emote_arc_into_maps(
                            &mut emote_map,
                            &mut emote_map_by_id,
                            Arc::clone(emote),
                        );
                    }
                    crate::backend::metrics::record_emote_event(
                        "emotes.user.fetch.7tv_personal.terminal_stale",
                    );
                }
            }
            ProviderFetchResult::Ready(Err(e)) => {
                crate::log_emotes!(info, "7TV personal emotes: {}", e);
                let mut preserved = 0usize;
                for emote in stale_user_emotes
                    .iter()
                    .filter(|e| e.provider == EmoteProvider::SevenTV)
                {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        Arc::clone(emote),
                    );
                    preserved += 1;
                }
                if preserved > 0 {
                    crate::log_emotes!(
                        info,
                        "Preserved {} cached 7TV personal emotes after non-terminal fetch error",
                        preserved
                    );
                }
            }
            ProviderFetchResult::TimedOut => {
                let mut preserved = 0usize;
                for emote in stale_user_emotes
                    .iter()
                    .filter(|e| e.provider == EmoteProvider::SevenTV)
                {
                    Self::insert_emote_arc_into_maps(
                        &mut emote_map,
                        &mut emote_map_by_id,
                        Arc::clone(emote),
                    );
                    preserved += 1;
                }
                crate::log_emotes!(
                    info,
                    "7TV personal emote fetch timed out; preserved {} cached personal emotes until deferred refresh",
                    preserved
                );
            }
        }

        let merged_count = emote_map_by_id.len();
        if merged_count == 0 && had_existing_user_cache && (twitch_timed_out || seventv_timed_out) {
            record_emote_stage_timed("emotes.user.load.deferred_preserve", total_start, 0);
            return Ok(());
        }
        let published = auth_fence
            .commit_if_current(|| {
                let _user_commit = self.user_emote_commit_lock.lock_or_recover();
                if self.user_emote_request_generation.load(Ordering::SeqCst) != request_generation {
                    return false;
                }
                let mut user_emotes = self.user_emotes.write_or_recover();
                *user_emotes = Some(CacheEntry {
                    emotes: emote_map,
                    emotes_by_id: emote_map_by_id,
                    fetched_at: Instant::now(),
                });
                *self.user_emote_owner_id.write_or_recover() = Some(user_id.to_string());
                true
            })
            .unwrap_or(false);
        if !published {
            crate::log_emotes!(
                info,
                "Discarding stale user-emote publication for user_id {}",
                user_id
            );
            return Ok(());
        }
        record_emote_stage_timed("emotes.user.load.total", total_start, merged_count);

        Ok(())
    }

    fn schedule_deferred_user_emote_refresh(&self, user_id: String, token: String) {
        if USER_EMOTES_DEFERRED_IN_FLIGHT.swap(true, Ordering::SeqCst) {
            crate::backend::metrics::record_emote_event("emotes.user.deferred.skipped_in_flight");
            record_emote_stage_duration(
                "emotes.user.deferred.skipped_in_flight",
                Duration::ZERO,
                1,
            );
            return;
        }

        crate::backend::metrics::record_emote_event("emotes.user.deferred.scheduled");
        record_emote_stage_duration("emotes.user.deferred.scheduled", Duration::ZERO, 1);

        crate::backend::bridge::get_runtime().spawn(async move {
            record_emote_stage_duration("emotes.user.deferred.started", Duration::ZERO, 1);
            let deferred_start = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(USER_EMOTES_DEFERRED_FETCH_TIMEOUT_SECS),
                EMOTE_CACHE.load_user_emotes_impl(&user_id, &token, None, false, true),
            )
            .await;

            match result {
                Ok(Ok(())) => {
                    record_emote_stage_timed("emotes.user.load.deferred.total", deferred_start, 0);
                    record_emote_stage_duration(
                        "emotes.user.deferred.completed",
                        Duration::ZERO,
                        1,
                    );
                    rebuild_user_emote_index().await;
                }
                Ok(Err(e)) => {
                    record_emote_stage_timed("emotes.user.load.deferred.total", deferred_start, 0);
                    record_emote_stage_duration("emotes.user.deferred.failed", Duration::ZERO, 1);
                    crate::log_emotes!(warn, "Deferred user emote refresh failed: {}", e);
                }
                Err(_) => {
                    record_emote_stage_timed("emotes.user.load.deferred.total", deferred_start, 0);
                    record_emote_stage_duration("emotes.user.deferred.timeout", Duration::ZERO, 1);
                    crate::log_emotes!(
                        warn,
                        "Deferred user emote refresh timed out after {}s",
                        USER_EMOTES_DEFERRED_FETCH_TIMEOUT_SECS
                    );
                }
            }

            USER_EMOTES_DEFERRED_IN_FLIGHT.store(false, Ordering::SeqCst);
        });
    }

    /// Check if cache needs refresh
    fn needs_refresh(entry: &CacheEntry, ttl: Duration) -> bool {
        entry.fetched_at.elapsed() > ttl
    }

    /// Load global emotes from all providers
    pub async fn load_global_emotes(&self) -> Result<(), String> {
        let token = self.token.read_or_recover().clone();
        self.load_global_emotes_with_token(token.as_deref()).await
    }

    /// Load global emotes from all providers, including Twitch if token is provided
    pub async fn load_global_emotes_with_token(&self, token: Option<&str>) -> Result<(), String> {
        let total_start = Instant::now();
        crate::log_emotes!(info, "Loading global emotes from all providers...");
        let mut all_emotes: EmoteNameMap = HashMap::new();
        let mut all_emotes_by_id: EmoteIdentityMap = EmoteIdentityMap::default();
        let mut insert_emote = |emote: Arc<Emote>| {
            Self::insert_emote_arc_into_maps(&mut all_emotes, &mut all_emotes_by_id, emote);
        };
        let previous_global_emotes: Vec<Arc<Emote>> = self
            .global
            .read_or_recover()
            .as_ref()
            .map(|entry| entry.emotes_by_id.values().map(Arc::clone).collect())
            .unwrap_or_default();
        let stale_global_7tv_fallback: Vec<Arc<Emote>> = previous_global_emotes
            .iter()
            .filter(|emote| emote.provider == EmoteProvider::SevenTV)
            .map(Arc::clone)
            .collect();

        enum SevenTvGlobalFetchState {
            Ready(Result<SevenTvGlobalFetchResult, String>),
            TimedOut(tokio::task::JoinHandle<Result<SevenTvGlobalFetchResult, String>>),
        }

        let fetch_parallel_start = Instant::now();
        let (seventv, bttv, ffz, twitch) = tokio::join!(
            async {
                let started_at = Instant::now();
                let mut fetch_task = crate::backend::bridge::RUNTIME.spawn(fetch_7tv_global());
                match tokio::time::timeout(
                    Duration::from_millis(SEVENTV_GLOBAL_SYNC_TIMEOUT_MS),
                    &mut fetch_task,
                )
                .await
                {
                    Ok(join_result) => {
                        let result = match join_result {
                            Ok(value) => value,
                            Err(err) => Err(format!("7TV global fetch task join error: {}", err)),
                        };
                        let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                        record_emote_stage_timed("emotes.global.fetch.7tv", started_at, items);
                        SevenTvGlobalFetchState::Ready(result)
                    }
                    Err(_) => {
                        record_emote_stage_timed("emotes.global.fetch.7tv", started_at, 0);
                        record_emote_stage_timed("emotes.global.fetch.7tv.timeout", started_at, 0);
                        crate::log_perf!(
                            warn,
                            timeout_ms = SEVENTV_GLOBAL_SYNC_TIMEOUT_MS,
                            "7TV global fetch timed out; using fallback and deferring full fetch"
                        );
                        SevenTvGlobalFetchState::TimedOut(fetch_task)
                    }
                }
            },
            async {
                let started_at = Instant::now();
                let result = fetch_bttv_global().await;
                let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                record_emote_stage_timed("emotes.global.fetch.bttv", started_at, items);
                result
            },
            async {
                let started_at = Instant::now();
                let result = fetch_ffz_global().await;
                let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                record_emote_stage_timed("emotes.global.fetch.ffz", started_at, items);
                result
            },
            async {
                let started_at = Instant::now();
                let result = fetch_twitch_global_emotes(token).await;
                let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                record_emote_stage_timed("emotes.global.fetch.twitch", started_at, items);
                result
            }
        );
        record_emote_stage_timed("emotes.global.fetch.parallel", fetch_parallel_start, 0);

        let merge_start = Instant::now();
        let mut defer_global_7tv_refresh = false;
        let mut timed_out_global_fetch: Option<
            tokio::task::JoinHandle<Result<SevenTvGlobalFetchResult, String>>,
        > = None;
        match twitch {
            Ok(emotes) => {
                crate::log_emotes!(info, "Loaded {} global Twitch emotes", emotes.len());
                for emote in emotes {
                    insert_emote(Arc::new(emote));
                }
            }
            Err(e) => {
                let kept = carry_over_provider_emotes(
                    &previous_global_emotes,
                    EmoteProvider::Twitch,
                    &mut insert_emote,
                );
                crate::log_emotes!(
                    warn,
                    "Failed to load global Twitch emotes: {} (kept {} cached emotes)",
                    e,
                    kept
                );
            }
        }
        match ffz {
            Ok(emotes) => {
                crate::log_emotes!(info, "Loaded {} global FFZ emotes", emotes.len());
                for emote in emotes {
                    insert_emote(Arc::new(emote));
                }
            }
            Err(e) => {
                let kept = carry_over_provider_emotes(
                    &previous_global_emotes,
                    EmoteProvider::FFZ,
                    &mut insert_emote,
                );
                crate::log_emotes!(
                    warn,
                    "Failed to load global FFZ emotes: {} (kept {} cached emotes)",
                    e,
                    kept
                );
            }
        }
        match bttv {
            Ok(emotes) => {
                crate::log_emotes!(info, "Loaded {} global BTTV emotes", emotes.len());
                for emote in emotes {
                    insert_emote(Arc::new(emote));
                }
            }
            Err(e) => {
                let kept = carry_over_provider_emotes(
                    &previous_global_emotes,
                    EmoteProvider::BTTV,
                    &mut insert_emote,
                );
                crate::log_emotes!(
                    warn,
                    "Failed to load global BTTV emotes: {} (kept {} cached emotes)",
                    e,
                    kept
                );
            }
        }
        match seventv {
            SevenTvGlobalFetchState::Ready(seventv_result) => match seventv_result {
                Ok(emotes) => {
                    crate::log_emotes!(info, "Loaded {} global 7TV emotes", emotes.len());
                    for emote in emotes.iter().cloned() {
                        insert_emote(emote);
                    }
                }
                Err(e) => {
                    defer_global_7tv_refresh = true;
                    crate::log_emotes!(
                        warn,
                        "Failed to load global 7TV emotes: {} (using cached fallback if available)",
                        e
                    );
                    let fallback_merge_start = Instant::now();
                    for emote in &stale_global_7tv_fallback {
                        insert_emote(Arc::clone(emote));
                    }
                    if !stale_global_7tv_fallback.is_empty() {
                        record_emote_stage_timed(
                            "emotes.global.merge.7tv_fallback",
                            fallback_merge_start,
                            stale_global_7tv_fallback.len(),
                        );
                    }
                }
            },
            SevenTvGlobalFetchState::TimedOut(fetch_task) => {
                defer_global_7tv_refresh = true;
                timed_out_global_fetch = Some(fetch_task);
                let fallback_merge_start = Instant::now();
                for emote in &stale_global_7tv_fallback {
                    insert_emote(Arc::clone(emote));
                }
                if !stale_global_7tv_fallback.is_empty() {
                    record_emote_stage_timed(
                        "emotes.global.merge.7tv_fallback",
                        fallback_merge_start,
                        stale_global_7tv_fallback.len(),
                    );
                }
            }
        }
        record_emote_stage_timed("emotes.global.merge", merge_start, all_emotes_by_id.len());

        crate::log_emotes!(info, "Total global emotes loaded: {}", all_emotes.len());

        let cache_write_start = Instant::now();
        let loaded_count = {
            let mut global = self.global.write_or_recover();
            *global = Some(CacheEntry {
                emotes: all_emotes,
                emotes_by_id: all_emotes_by_id,
                fetched_at: Instant::now(),
            });
            global
                .as_ref()
                .map(|entry| entry.emotes_by_id.len())
                .unwrap_or(0)
        };
        record_emote_stage_timed("emotes.global.cache_write", cache_write_start, loaded_count);

        let rebuild_start = Instant::now();
        rebuild_global_emote_index().await;
        record_emote_stage_timed("emotes.global.rebuild_index", rebuild_start, loaded_count);
        record_emote_stage_timed("emotes.global.load.total", total_start, loaded_count);

        if let Some(mut timed_out_fetch) = timed_out_global_fetch {
            record_7tv_deferred_watchdog("global", "scheduled");
            crate::log_emotes!(
                debug,
                "Scheduling deferred apply from timed-out in-flight 7TV global task"
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                record_7tv_deferred_watchdog("global", "started");
                crate::log_emotes!(debug, "Deferred global 7TV in-flight waiter started");
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    &mut timed_out_fetch,
                )
                .await
                {
                    Ok(Ok(Ok(emotes))) => {
                        let fetched_count = emotes.len();
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            fetched_count,
                        );
                        let apply_start = Instant::now();
                        let provider_count = EMOTE_CACHE
                            .apply_global_provider_emotes(
                                EmoteProvider::SevenTV,
                                emotes.iter().cloned(),
                            )
                            .await;
                        record_emote_stage_timed(
                            "emotes.global.apply.7tv.deferred",
                            apply_start,
                            provider_count,
                        );
                        record_7tv_deferred_watchdog("global", "completed");
                    }
                    Ok(Ok(Err(err))) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("global", "failed");
                        crate::log_emotes!(debug, "Deferred global 7TV fetch failed: {}", err);
                    }
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("global", "join_failed");
                        crate::log_emotes!(debug, "Deferred global 7TV join failed: {}", err);
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        timed_out_fetch.abort();
                        record_7tv_deferred_watchdog("global", "timeout");
                        crate::log_emotes!(
                            debug,
                            "Deferred global 7TV fetch timed out after {}s",
                            SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS
                        );
                    }
                }
            });
        } else if defer_global_7tv_refresh {
            record_7tv_deferred_watchdog("global", "scheduled");
            crate::log_emotes!(
                debug,
                "Scheduling deferred retry with new 7TV global fetch task"
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                record_7tv_deferred_watchdog("global", "started");
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    fetch_7tv_global(),
                )
                .await
                {
                    Ok(Ok(emotes)) => {
                        let fetched_count = emotes.len();
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            fetched_count,
                        );
                        let apply_start = Instant::now();
                        let provider_count = EMOTE_CACHE
                            .apply_global_provider_emotes(
                                EmoteProvider::SevenTV,
                                emotes.iter().cloned(),
                            )
                            .await;
                        record_emote_stage_timed(
                            "emotes.global.apply.7tv.deferred",
                            apply_start,
                            provider_count,
                        );
                        record_7tv_deferred_watchdog("global", "completed");
                    }
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("global", "failed");
                        crate::log_emotes!(debug, "Deferred global 7TV fetch failed: {}", err);
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("global", "timeout");
                        crate::log_emotes!(
                            debug,
                            "Deferred global 7TV fetch timed out after {}s",
                            SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS
                        );
                    }
                }
            });
        }

        Ok(())
    }

    /// Load channel-specific emotes from all providers
    /// channel_id is the numeric Twitch user ID (for API calls)
    /// channel_login is the username (used as cache key for frontend access)
    pub async fn load_channel_emotes(
        &self,
        channel_id: &str,
        channel_login: &str,
    ) -> Result<(), String> {
        self.load_channel_emotes_reporting(channel_id, channel_login, false)
            .await
            .map(|_| ())
    }

    /// Force refresh channel emotes from APIs, bypassing TTL cache-hit short-circuit.
    /// Used for initial active-channel hydration so each visible split gets up-to-date sets.
    pub async fn refresh_channel_emotes(
        &self,
        channel_id: &str,
        channel_login: &str,
    ) -> Result<(), String> {
        self.load_channel_emotes_reporting(channel_id, channel_login, true)
            .await
            .map(|_| ())
    }

    /// Like `load_channel_emotes`/`refresh_channel_emotes`, but surfaces whether the
    /// call actually loaded or coalesced into another in-flight load. Callers that
    /// own a one-shot retry (the staggered IRC loader) use this to avoid discarding
    /// their retry when they merely coalesced into a load that may not complete.
    pub async fn load_channel_emotes_reporting(
        &self,
        channel_id: &str,
        channel_login: &str,
        force_refresh: bool,
    ) -> Result<LoadOutcome, String> {
        let token = self.token.read_or_recover().clone();
        self.load_channel_emotes_with_token_internal(
            channel_id,
            channel_login,
            token.as_deref(),
            force_refresh,
        )
        .await
    }

    async fn load_channel_emotes_with_token_internal(
        &self,
        channel_id: &str,
        channel_login: &str,
        token: Option<&str>,
        force_refresh: bool,
    ) -> Result<LoadOutcome, String> {
        let cache_key = channel_login.to_lowercase();
        let total_start = Instant::now();

        if !force_refresh {
            if let Some(entry) = self.channels.get(&cache_key)
                && !Self::needs_refresh(&entry, self.ttl)
            {
                schedule_ffz_room_badge_hydration(&cache_key);
                record_emote_stage_duration(
                    "emotes.channel.load.cache_hit",
                    Duration::ZERO,
                    entry.emotes_by_id.len(),
                );
                return Ok(LoadOutcome::Loaded);
            }
        } else {
            crate::log_emotes!(
                debug,
                "Forcing channel emote refresh for {} (cache bypass)",
                channel_login
            );
        }

        let mut catalog_pending = None;

        // SAFETY: The contains() check and insert() are ATOMIC because both are
        // within the same lock scope. Two threads cannot both pass the contains check.
        {
            let mut loading = self.loading.lock_or_recover();
            if loading.contains(&cache_key) {
                record_emote_stage_duration("emotes.channel.load.coalesced", Duration::ZERO, 0);
                return Ok(LoadOutcome::Coalesced);
            }
            let cold_channel = !self.channels.contains_key(&cache_key);
            loading.insert(cache_key.clone());
            if cold_channel {
                catalog_pending = Some(ChannelCatalogPendingGuard::begin(cache_key.clone()));
            }
        }

        struct EmoteLoadingGuard<'a> {
            loading: &'a Mutex<HashSet<String>>,
            key: String,
            defused: bool,
        }

        impl Drop for EmoteLoadingGuard<'_> {
            fn drop(&mut self) {
                if !self.defused {
                    self.loading.lock_or_recover().remove(&self.key);
                    crate::log_emotes!(
                        debug,
                        "EmoteLoadingGuard: cleaned up loading flag for '{}' (panic or early return)",
                        self.key
                    );
                }
                clear_channel_pending(&self.key);
            }
        }

        let mut _loading_guard = EmoteLoadingGuard {
            loading: &self.loading,
            key: cache_key.clone(),
            defused: false,
        };

        mark_channel_loading(&cache_key);

        let mut all_emotes: EmoteNameMap = HashMap::new();
        let mut all_emotes_by_id: EmoteIdentityMap = EmoteIdentityMap::default();
        let mut insert_emote = |emote: Arc<Emote>| {
            Self::insert_emote_arc_into_maps(&mut all_emotes, &mut all_emotes_by_id, emote);
        };
        let previous_channel_emotes: Vec<Arc<Emote>> = self
            .channels
            .get(&cache_key)
            .map(|entry| entry.emotes_by_id.values().map(Arc::clone).collect())
            .unwrap_or_default();
        let stale_7tv_fallback: Vec<Arc<Emote>> = previous_channel_emotes
            .iter()
            .filter(|emote| emote.provider == EmoteProvider::SevenTV)
            .map(Arc::clone)
            .collect();
        let stale_7tv_fallback_count = stale_7tv_fallback.len();

        let is_numeric_id =
            channel_id.chars().all(|c| c.is_ascii_digit()) && !channel_id.is_empty();

        let (ffz_bootstrap, actual_id) = if is_numeric_id {
            (None, channel_id.to_string())
        } else {
            let ffz_bootstrap_start = Instant::now();
            match fetch_ffz_channel_with_id(channel_login).await {
                Ok((emotes, twitch_id)) => {
                    let id = twitch_id.unwrap_or_else(|| channel_id.to_string());
                    record_emote_stage_timed(
                        "emotes.channel.fetch.ffz_bootstrap",
                        ffz_bootstrap_start,
                        emotes.len(),
                    );
                    (Some(emotes), id)
                }
                Err(e) => {
                    mark_channel_failed(channel_login, channel_id);
                    record_emote_stage_timed(
                        "emotes.channel.fetch.ffz_bootstrap",
                        ffz_bootstrap_start,
                        0,
                    );
                    record_emote_stage_timed("emotes.channel.load.total", total_start, 0);
                    return Err(e);
                }
            }
        };

        type SevenTvFetchHandle =
            tokio::task::JoinHandle<Result<SevenTvChannelFetchResult, String>>;

        enum SevenTvChannelFetchState {
            Ready(Result<SevenTvChannelFetchResult, String>),
            TimedOut(SevenTvFetchHandle),
        }

        let fetch_parallel_start = Instant::now();
        let (seventv, bttv, twitch, ffz_emotes) = tokio::join!(
            async {
                let started_at = Instant::now();
                let timeout_key = channel_login.to_ascii_lowercase();
                if let Some(remaining) =
                    seventv_sync_cooldown_remaining(&timeout_key, Instant::now())
                {
                    crate::log_perf!(
                        debug,
                        channel = channel_login,
                        cooldown_remaining_ms = remaining.as_millis(),
                        "Skipping sync 7TV wait after recent provider timeout; deferring immediately"
                    );
                    record_emote_stage_timed(
                        "emotes.channel.fetch.7tv.cooldown_skip",
                        started_at,
                        0,
                    );
                    let seven_tv_channel_id = actual_id.clone();
                    let seven_tv_channel_login = channel_login.to_string();
                    let fetch_task = crate::backend::bridge::RUNTIME.spawn(async move {
                        fetch_7tv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
                    });
                    return SevenTvChannelFetchState::TimedOut(fetch_task);
                }

                if !claim_seventv_sync_probe() {
                    crate::log_perf!(
                        debug,
                        channel = channel_login,
                        "Skipping additional sync 7TV probe; deferring immediately"
                    );
                    record_emote_stage_timed(
                        "emotes.channel.fetch.7tv.concurrent_skip",
                        started_at,
                        0,
                    );
                    let seven_tv_channel_id = actual_id.clone();
                    let seven_tv_channel_login = channel_login.to_string();
                    let fetch_task = crate::backend::bridge::RUNTIME.spawn(async move {
                        fetch_7tv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
                    });
                    return SevenTvChannelFetchState::TimedOut(fetch_task);
                }

                let seven_tv_channel_id = actual_id.clone();
                let seven_tv_channel_login = channel_login.to_string();
                let mut fetch_task = crate::backend::bridge::RUNTIME.spawn(async move {
                    fetch_7tv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
                });
                match tokio::time::timeout(
                    Duration::from_millis(SEVENTV_CHANNEL_SYNC_TIMEOUT_MS),
                    &mut fetch_task,
                )
                .await
                {
                    Ok(join_result) => {
                        let result = match join_result {
                            Ok(value) => value,
                            Err(err) => Err(format!("7TV channel fetch task join error: {}", err)),
                        };
                        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&timeout_key);
                        let items = result
                            .as_ref()
                            .map(|result| result.0.emote_count())
                            .unwrap_or(0);
                        record_emote_stage_timed("emotes.channel.fetch.7tv", started_at, items);
                        SevenTvChannelFetchState::Ready(result)
                    }
                    Err(_) => {
                        record_seventv_sync_timeout(timeout_key, Instant::now());
                        record_emote_stage_timed("emotes.channel.fetch.7tv", started_at, 0);
                        record_emote_stage_timed("emotes.channel.fetch.7tv.timeout", started_at, 0);
                        crate::log_perf!(
                            warn,
                            channel = channel_login,
                            timeout_ms = SEVENTV_CHANNEL_SYNC_TIMEOUT_MS,
                            "7TV channel fetch timed out; using fallback and deferring full fetch"
                        );
                        SevenTvChannelFetchState::TimedOut(fetch_task)
                    }
                }
            },
            async {
                let started_at = Instant::now();
                let result = fetch_bttv_channel(&actual_id, channel_login).await;
                let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                record_emote_stage_timed("emotes.channel.fetch.bttv", started_at, items);
                result
            },
            async {
                let started_at = Instant::now();
                let result = fetch_twitch_channel_emotes(&actual_id, token, channel_login).await;
                let items = result.as_ref().map(|v| v.len()).unwrap_or(0);
                record_emote_stage_timed("emotes.channel.fetch.twitch", started_at, items);
                result
            },
            async {
                if let Some(emotes) = ffz_bootstrap {
                    return Ok(emotes);
                }
                let started_at = Instant::now();
                let result = fetch_ffz_channel(channel_login).await;
                let items = result.as_ref().map(Vec::len).unwrap_or(0);
                record_emote_stage_timed("emotes.channel.fetch.ffz", started_at, items);
                result
            }
        );
        record_emote_stage_timed("emotes.channel.fetch.parallel", fetch_parallel_start, 0);

        let merge_start = Instant::now();
        let mut defer_seventv_refresh = false;
        let mut timed_out_seventv_fetch: Option<SevenTvFetchHandle> = None;
        // A provider failed with nothing cached to carry over: retry soon
        // instead of leaving the channel without those emotes for a full TTL.
        let mut provider_gap = false;
        let mut merge_provider =
            |provider: EmoteProvider, label: &str, result: Result<Vec<Emote>, String>| match result
            {
                Ok(emotes) => {
                    crate::log_emotes!(
                        info,
                        "Loaded {} {} channel emotes for {}",
                        emotes.len(),
                        label,
                        channel_login
                    );
                    for emote in emotes {
                        insert_emote(Arc::new(emote));
                    }
                }
                Err(err) => {
                    let kept = carry_over_provider_emotes(
                        &previous_channel_emotes,
                        provider,
                        &mut insert_emote,
                    );
                    provider_gap |= kept == 0;
                    crate::log_emotes!(
                        warn,
                        "{} channel emote fetch failed for {}: {} (kept {} cached emotes)",
                        label,
                        channel_login,
                        err,
                        kept
                    );
                }
            };
        merge_provider(EmoteProvider::Twitch, "Twitch", twitch);
        merge_provider(EmoteProvider::FFZ, "FFZ", ffz_emotes);
        merge_provider(EmoteProvider::BTTV, "BTTV", bttv);

        match seventv {
            SevenTvChannelFetchState::Ready(seventv_result) => match seventv_result {
                Ok(shared) => match shared.as_ref() {
                    (
                        SevenTvExactSetFetchOutcome::Present {
                            emotes,
                            emote_set_id,
                        },
                        subscription,
                    ) => {
                        if emotes.is_empty()
                            && emote_set_id.is_none()
                            && stale_7tv_fallback_count > 0
                        {
                            defer_seventv_refresh = true;
                            crate::log_emotes!(
                                warn,
                                "7TV returned empty data without emote_set_id for {} - keeping {} cached 7TV emotes and scheduling deferred retry",
                                channel_login,
                                stale_7tv_fallback_count
                            );
                            let fallback_merge_start = Instant::now();
                            for emote in stale_7tv_fallback {
                                insert_emote(emote);
                            }
                            record_emote_stage_timed(
                                "emotes.channel.merge.7tv_fallback",
                                fallback_merge_start,
                                stale_7tv_fallback_count,
                            );
                            record_emote_stage_duration(
                                "emotes.channel.fetch.7tv.empty_without_set_id",
                                Duration::ZERO,
                                0,
                            );
                        } else {
                            crate::log_emotes!(
                                info,
                                "Loaded {} 7TV channel emotes for {} (set_id: {:?})",
                                emotes.len(),
                                channel_login,
                                emote_set_id
                            );

                            if let Some(new_set_id) = emote_set_id.as_ref() {
                                let old_set_id = self.channel_emote_set_ids.get(&cache_key);
                                if let Some(old_id) = old_set_id
                                    && old_id.value() != new_set_id
                                {
                                    crate::log_emotes!(
                                        info,
                                        "7TV emote set ID changed for {}: {} -> {}",
                                        channel_login,
                                        old_id.value(),
                                        new_set_id
                                    );
                                }
                                self.channel_emote_set_ids
                                    .insert(cache_key.clone(), new_set_id.clone());
                                if let Some(subscription) = subscription.clone() {
                                    seventv_eventapi::subscribe_channel_details(
                                        &cache_key,
                                        subscription,
                                    );
                                } else {
                                    seventv_eventapi::subscribe_channel_set(&cache_key, new_set_id);
                                }
                            }
                            for emote in emotes.iter().cloned() {
                                insert_emote(emote);
                            }
                        }
                    }
                    (
                        SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    ) => {
                        if seventv_eventapi::retire_terminal_channel_fetch(
                            &cache_key,
                            emote_set_id,
                            subscription.clone(),
                        ) {
                            self.channel_emote_set_ids
                                .remove_if(&cache_key, |_, current| current == emote_set_id);
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.terminal_cleared",
                            );
                            crate::log_emotes!(
                                info,
                                "7TV set {} is terminally absent for {}; clearing channel 7TV rows while retaining user.update ownership",
                                emote_set_id,
                                channel_login
                            );
                        } else {
                            defer_seventv_refresh = true;
                            for emote in stale_7tv_fallback {
                                insert_emote(emote);
                            }
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.terminal_stale",
                            );
                            crate::log_emotes!(
                                info,
                                "Dropped stale terminal 7TV set {} result for {}; a newer set owner is registered",
                                emote_set_id,
                                channel_login
                            );
                        }
                    }
                },
                Err(err) => {
                    defer_seventv_refresh = true;
                    crate::log_emotes!(
                        warn,
                        "7TV channel fetch failed for {}: {} (using cached fallback if available)",
                        channel_login,
                        err
                    );
                    let fallback_merge_start = Instant::now();
                    for emote in stale_7tv_fallback {
                        insert_emote(emote);
                    }
                    if stale_7tv_fallback_count > 0 {
                        record_emote_stage_timed(
                            "emotes.channel.merge.7tv_fallback",
                            fallback_merge_start,
                            stale_7tv_fallback_count,
                        );
                    }
                }
            },
            SevenTvChannelFetchState::TimedOut(fetch_task) => {
                defer_seventv_refresh = true;
                timed_out_seventv_fetch = Some(fetch_task);
                let fallback_merge_start = Instant::now();
                for emote in stale_7tv_fallback {
                    insert_emote(emote);
                }
                if stale_7tv_fallback_count > 0 {
                    record_emote_stage_timed(
                        "emotes.channel.merge.7tv_fallback",
                        fallback_merge_start,
                        stale_7tv_fallback_count,
                    );
                }
            }
        }
        record_emote_stage_timed("emotes.channel.merge", merge_start, all_emotes_by_id.len());

        crate::log_emotes!(
            info,
            "Total channel emotes loaded for {}: {}",
            channel_login,
            all_emotes.len()
        );

        let cache_write_start = Instant::now();
        self.channels.insert(
            cache_key.clone(),
            CacheEntry {
                emotes: all_emotes,
                emotes_by_id: all_emotes_by_id,
                fetched_at: {
                    let retry_in = if defer_seventv_refresh {
                        Some(Duration::from_secs(10))
                    } else if provider_gap {
                        Some(Duration::from_secs(CHANNEL_PROVIDER_FAILURE_RETRY_SECS))
                    } else {
                        None
                    };
                    retry_in
                        .and_then(|retry_in| {
                            Instant::now().checked_sub(self.ttl.saturating_sub(retry_in))
                        })
                        .unwrap_or_else(Instant::now)
                },
            },
        );
        let channel_loaded_count = self
            .channels
            .get(&cache_key)
            .map(|entry| entry.emotes_by_id.len())
            .unwrap_or(0);
        record_emote_stage_timed(
            "emotes.channel.cache_write",
            cache_write_start,
            channel_loaded_count,
        );

        let rebuild_start = Instant::now();
        rebuild_emote_indices(&cache_key).await;
        record_emote_stage_timed(
            "emotes.channel.rebuild_index",
            rebuild_start,
            channel_loaded_count,
        );

        _loading_guard.defused = true;
        {
            let mut loading = self.loading.lock_or_recover();
            loading.remove(&cache_key);
        }

        let preload_trigger_start = Instant::now();
        trigger_open_channel_preload(&cache_key);
        record_emote_stage_timed(
            "emotes.channel.trigger_preload",
            preload_trigger_start,
            channel_loaded_count,
        );

        clear_channel_failed(channel_login);
        record_emote_stage_timed(
            "emotes.channel.load.total",
            total_start,
            channel_loaded_count,
        );

        if let Some(mut timed_out_fetch) = timed_out_seventv_fetch {
            let deferred_channel_login = channel_login.to_string();
            let deferred_cache_key = cache_key.clone();
            let deferred_timeout_key = channel_login.to_ascii_lowercase();
            let deferred_catalog_pending = catalog_pending.take();
            record_7tv_deferred_watchdog("channel", "scheduled");
            crate::log_emotes!(
                debug,
                "Scheduling deferred apply from timed-out in-flight 7TV task for {}",
                deferred_channel_login
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                let _catalog_pending = deferred_catalog_pending;
                record_7tv_deferred_watchdog("channel", "started");
                crate::log_emotes!(
                    debug,
                    "Deferred in-flight 7TV waiter started for {}",
                    deferred_channel_login
                );
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    &mut timed_out_fetch,
                )
                .await
                {
                    Ok(Ok(Ok(shared))) => match shared.as_ref() {
                    (
                        SevenTvExactSetFetchOutcome::Present {
                            emotes,
                            emote_set_id,
                        },
                        subscription,
                    ) => {
                        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&deferred_timeout_key);
                        let fetched_count = emotes.len();
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            fetched_count,
                        );
                        let apply_start = Instant::now();
                        let provider_count = EMOTE_CACHE
                            .apply_channel_provider_emotes(
                                &deferred_cache_key,
                                &deferred_channel_login,
                                EmoteProvider::SevenTV,
                                emotes.iter().cloned(),
                                emote_set_id.clone(),
                            )
                            .await;
                        if let Some(subscription) = subscription.clone() {
                            seventv_eventapi::subscribe_channel_details(
                                &deferred_cache_key,
                                subscription,
                            );
                        }
                        record_emote_stage_timed(
                            "emotes.channel.apply.7tv.deferred",
                            apply_start,
                            provider_count,
                        );
                        record_7tv_deferred_watchdog("channel", "completed");
                    }
                    (
                        SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    ) => {
                        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&deferred_timeout_key);
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        if seventv_eventapi::retire_terminal_channel_fetch(
                            &deferred_cache_key,
                            emote_set_id,
                            subscription.clone(),
                        ) {
                            let removed = EMOTE_CACHE
                                .clear_channel_provider_rows(
                                    &deferred_cache_key,
                                    EmoteProvider::SevenTV,
                                )
                                .await;
                            EMOTE_CACHE.channel_emote_set_ids.remove_if(
                                &deferred_cache_key,
                                |_, current| current == emote_set_id,
                            );
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.deferred_terminal_cleared",
                            );
                            crate::log_emotes!(
                                info,
                                "Deferred 7TV fetch confirmed terminal set {} for {}; cleared {} provider rows",
                                emote_set_id,
                                deferred_channel_login,
                                removed
                            );
                        } else {
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.deferred_terminal_stale",
                            );
                        }
                        record_7tv_deferred_watchdog("channel", "completed");
                    }
                    },
                    Ok(Ok(Err(err))) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("channel", "failed");
                        crate::log_emotes!(
                            debug,
                            "Deferred 7TV fetch still failed for {}: {}",
                            deferred_channel_login,
                            err
                        );
                    }
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("channel", "join_failed");
                        crate::log_emotes!(
                            debug,
                            "Deferred 7TV join failed for {}: {}",
                            deferred_channel_login,
                            err
                        );
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        timed_out_fetch.abort();
                        record_7tv_deferred_watchdog("channel", "timeout");
                        crate::log_emotes!(
                            debug,
                            "Deferred 7TV fetch timed out for {} after {}s",
                            deferred_channel_login,
                            SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS
                        );
                    }
                }
            });
        } else if defer_seventv_refresh {
            let deferred_channel_login = channel_login.to_string();
            let deferred_cache_key = cache_key.clone();
            let deferred_timeout_key = channel_login.to_ascii_lowercase();
            let deferred_actual_id = actual_id.clone();
            let deferred_catalog_pending = catalog_pending.take();
            record_7tv_deferred_watchdog("channel", "scheduled");
            crate::log_emotes!(
                debug,
                "Scheduling deferred retry with new 7TV fetch task for {}",
                deferred_channel_login
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                let _catalog_pending = deferred_catalog_pending;
                record_7tv_deferred_watchdog("channel", "started");
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    fetch_7tv_channel(&deferred_actual_id, &deferred_channel_login),
                )
                .await
                {
                    Ok(Ok(shared)) => match shared.as_ref() {
                    (
                        SevenTvExactSetFetchOutcome::Present {
                            emotes,
                            emote_set_id,
                        },
                        subscription,
                    ) => {
                        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&deferred_timeout_key);
                        let fetched_count = emotes.len();
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            fetched_count,
                        );
                        let apply_start = Instant::now();
                        let provider_count = EMOTE_CACHE
                            .apply_channel_provider_emotes(
                                &deferred_cache_key,
                                &deferred_channel_login,
                                EmoteProvider::SevenTV,
                                emotes.iter().cloned(),
                                emote_set_id.clone(),
                            )
                            .await;
                        if let Some(subscription) = subscription.clone() {
                            seventv_eventapi::subscribe_channel_details(
                                &deferred_cache_key,
                                subscription,
                            );
                        }
                        record_emote_stage_timed(
                            "emotes.channel.apply.7tv.deferred",
                            apply_start,
                            provider_count,
                        );
                        record_7tv_deferred_watchdog("channel", "completed");
                    }
                    (
                        SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    ) => {
                        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&deferred_timeout_key);
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        if seventv_eventapi::retire_terminal_channel_fetch(
                            &deferred_cache_key,
                            emote_set_id,
                            subscription.clone(),
                        ) {
                            let removed = EMOTE_CACHE
                                .clear_channel_provider_rows(
                                    &deferred_cache_key,
                                    EmoteProvider::SevenTV,
                                )
                                .await;
                            EMOTE_CACHE.channel_emote_set_ids.remove_if(
                                &deferred_cache_key,
                                |_, current| current == emote_set_id,
                            );
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.deferred_terminal_cleared",
                            );
                            crate::log_emotes!(
                                info,
                                "Deferred 7TV retry confirmed terminal set {} for {}; cleared {} provider rows",
                                emote_set_id,
                                deferred_channel_login,
                                removed
                            );
                        } else {
                            crate::backend::metrics::record_emote_event(
                                "emotes.channel.fetch.7tv.deferred_terminal_stale",
                            );
                        }
                        record_7tv_deferred_watchdog("channel", "completed");
                    }
                    },
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("channel", "failed");
                        crate::log_emotes!(
                            debug,
                            "Deferred 7TV fetch still failed for {}: {}",
                            deferred_channel_login,
                            err
                        );
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_7tv_deferred_watchdog("channel", "timeout");
                        crate::log_emotes!(
                            debug,
                            "Deferred 7TV fetch timed out for {} after {}s",
                            deferred_channel_login,
                            SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS
                        );
                    }
                }
            });
        }

        Ok(LoadOutcome::Loaded)
    }

    /// Check if global emotes need refresh
    pub async fn global_needs_refresh(&self) -> bool {
        let global = self.global.read_or_recover();
        match global.as_ref() {
            Some(entry) => Self::needs_refresh(entry, self.ttl),
            None => true,
        }
    }

    /// Return true when a channel cache is missing or stale enough to justify a provider fetch.
    pub fn channel_emotes_need_refresh(&self, channel_login: &str) -> bool {
        let cache_key = normalize_channel_key(channel_login);
        match self.channels.get(&cache_key) {
            Some(entry) => Self::needs_refresh(&entry, self.ttl),
            None => true,
        }
    }

    /// Return the current cached channel emote count without cloning emote payloads.
    pub fn channel_emote_count(&self, channel_login: &str) -> usize {
        let cache_key = normalize_channel_key(channel_login);
        self.channels
            .get(&cache_key)
            .map(|entry| entry.emotes_by_id.len())
            .unwrap_or(0)
    }

    /// Get all emotes for a channel (channel-specific + user subscriptions + global)
    /// Returns Arc<Emote> references to avoid cloning ~1-2MB of emote data
    pub fn get_all_emotes(&self, channel_id: &str) -> Vec<Arc<Emote>> {
        let cache_key = normalized_channel_key(channel_id);

        let mut emote_map: HashMap<Arc<str>, Arc<Emote>> = HashMap::new();

        let global = self.global.read_or_recover();
        if let Some(entry) = global.as_ref() {
            for emote in entry.emotes.values() {
                if emote_visible(emote) {
                    emote_map.insert(Arc::clone(&emote.name), Arc::clone(emote));
                }
            }
        }
        drop(global);

        let user_emotes = self.user_emotes.read_or_recover();
        if let Some(entry) = user_emotes.as_ref() {
            for emote in entry.emotes.values() {
                if emote_visible(emote) {
                    emote_map.insert(Arc::clone(&emote.name), Arc::clone(emote));
                }
            }
        }
        drop(user_emotes);

        if let Some(entry) = self.channels.get(cache_key.as_ref()) {
            for emote in entry.emotes.values() {
                if emote_visible(emote) {
                    emote_map.insert(Arc::clone(&emote.name), Arc::clone(emote));
                }
            }
        }

        emote_map.into_values().collect()
    }

    /// Get all emotes for picker/category UIs with collision-safe identity deduplication.
    ///
    /// Unlike `get_all_emotes`, this preserves emotes that share the same display name but have
    /// different provider/id pairs. This avoids accidental collisions in picker tabs/history.
    pub async fn get_all_emotes_unique(&self, channel_id: &str) -> Vec<Arc<Emote>> {
        let started_at = Instant::now();
        let cache_key = normalized_channel_key(channel_id);
        let global = self.global.read_or_recover();
        let user_emotes = self.user_emotes.read_or_recover();
        let channel_emotes = self.channels.get(cache_key.as_ref());

        let estimated_len = global
            .as_ref()
            .map(|entry| entry.emotes_by_id.len())
            .unwrap_or(0)
            .saturating_add(
                user_emotes
                    .as_ref()
                    .map(|entry| entry.emotes_by_id.len())
                    .unwrap_or(0),
            )
            .saturating_add(
                channel_emotes
                    .as_ref()
                    .map(|entry| entry.emotes_by_id.len())
                    .unwrap_or(0),
            );
        let mut merged = Vec::with_capacity(estimated_len);

        if let Some(entry) = global.as_ref() {
            for emote in entry.emotes_by_id.values() {
                if !emote_visible(emote) {
                    continue;
                }
                let selected = channel_emotes
                    .as_ref()
                    .and_then(|channel| {
                        channel.emotes_by_id.get_alias(
                            emote.id.as_ref(),
                            emote.name.as_ref(),
                            emote.provider,
                        )
                    })
                    .or_else(|| {
                        user_emotes.as_ref().and_then(|user| {
                            user.emotes_by_id.get_alias(
                                emote.id.as_ref(),
                                emote.name.as_ref(),
                                emote.provider,
                            )
                        })
                    })
                    .unwrap_or(emote);
                merged.push(Arc::clone(selected));
            }
        }

        if let Some(entry) = user_emotes.as_ref() {
            for emote in entry.emotes_by_id.values() {
                if !emote_visible(emote)
                    || global.as_ref().is_some_and(|global| {
                        global
                            .emotes_by_id
                            .get_alias(emote.id.as_ref(), emote.name.as_ref(), emote.provider)
                            .is_some()
                    })
                {
                    continue;
                }
                let selected = channel_emotes
                    .as_ref()
                    .and_then(|channel| {
                        channel.emotes_by_id.get_alias(
                            emote.id.as_ref(),
                            emote.name.as_ref(),
                            emote.provider,
                        )
                    })
                    .unwrap_or(emote);
                merged.push(Arc::clone(selected));
            }
        }

        if let Some(entry) = channel_emotes.as_ref() {
            for emote in entry.emotes_by_id.values() {
                let exists_in_lower_source = global.as_ref().is_some_and(|global| {
                    global
                        .emotes_by_id
                        .get_alias(emote.id.as_ref(), emote.name.as_ref(), emote.provider)
                        .is_some()
                }) || user_emotes.as_ref().is_some_and(|user| {
                    user.emotes_by_id
                        .get_alias(emote.id.as_ref(), emote.name.as_ref(), emote.provider)
                        .is_some()
                });
                if emote_visible(emote) && !exists_in_lower_source {
                    merged.push(Arc::clone(emote));
                }
            }
        }

        record_emote_stage_timed("emotes.merge.unique_all", started_at, merged.len());
        merged
    }

    /// Get only global emotes (Arc references to avoid cloning)
    pub async fn get_global_emotes(&self) -> Vec<Arc<Emote>> {
        let global = self.global.read_or_recover();
        if let Some(entry) = global.as_ref() {
            entry
                .emotes
                .iter()
                .filter(|(key, emote)| key.as_ref() == emote.name.as_ref())
                .map(|(_, emote)| emote)
                .filter(|emote| emote_visible(emote))
                .map(Arc::clone)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get only channel-specific emotes (Arc references to avoid cloning)
    pub fn get_channel_emotes(&self, channel_login: &str) -> Vec<Arc<Emote>> {
        let cache_key = channel_login.to_lowercase();
        match self.channels.get(&cache_key) {
            Some(entry) => entry
                .emotes
                .iter()
                .filter(|(key, emote)| key.as_ref() == emote.name.as_ref())
                .map(|(_, emote)| emote)
                .filter(|emote| emote_visible(emote))
                .map(Arc::clone)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Snapshot the channel indexes for renderer/tokenizer publication.
    ///
    /// When every exact emote is visible, the cache entry's existing alias map
    /// is already authoritative, including collision-safe ASCII-folded keys.
    /// If any provider row is hidden, rebuild from the visible exact records so
    /// a hidden folded winner cannot suppress a visible case-variant fallback.
    fn get_channel_index_snapshot(&self, channel_login: &str) -> (Vec<Arc<Emote>>, EmoteNameMap) {
        let cache_key = normalized_channel_key(channel_login);
        let Some(entry) = self.channels.get(cache_key.as_ref()) else {
            return (Vec::new(), HashMap::new());
        };

        let twitch_visible = emote_provider_enabled(EmoteProvider::Twitch);
        let seventv_visible = emote_provider_enabled(EmoteProvider::SevenTV);
        let bttv_visible = emote_provider_enabled(EmoteProvider::BTTV);
        let ffz_visible = emote_provider_enabled(EmoteProvider::FFZ);
        let emoji_visible = emote_provider_enabled(EmoteProvider::Emoji);
        let provider_visible = |provider| match provider {
            EmoteProvider::Twitch => twitch_visible,
            EmoteProvider::SevenTV => seventv_visible,
            EmoteProvider::BTTV => bttv_visible,
            EmoteProvider::FFZ => ffz_visible,
            EmoteProvider::Emoji => emoji_visible,
        };

        let mut visible_emotes = Vec::with_capacity(entry.emotes_by_id.len());
        let mut filtered_provider = false;
        for (key, emote) in &entry.emotes {
            if key.as_ref() != emote.name.as_ref() {
                continue;
            }
            if provider_visible(emote.provider) {
                visible_emotes.push(Arc::clone(emote));
            } else {
                filtered_provider = true;
            }
        }

        let lookup = if filtered_provider {
            let mut visible_lookup = HashMap::with_capacity(visible_emotes.len() * 2);
            for emote in &visible_emotes {
                Self::insert_emote_name_keys(&mut visible_lookup, Arc::clone(emote));
            }
            visible_lookup
        } else {
            entry.emotes.clone()
        };

        (visible_emotes, lookup)
    }

    /// Get only user's personal/subscription emotes (Arc references to avoid cloning)
    pub async fn get_user_emotes(&self) -> Vec<Arc<Emote>> {
        let user = self.user_emotes.read_or_recover();
        if let Some(entry) = user.as_ref() {
            entry
                .emotes
                .iter()
                .filter(|(key, emote)| key.as_ref() == emote.name.as_ref())
                .map(|(_, emote)| emote)
                .filter(|emote| emote_visible(emote))
                .map(Arc::clone)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Clear authenticated user-scoped emotes when Echo leaves authenticated mode.
    pub async fn clear_user_emotes(&self) {
        self.revoke_user_emotes_residency();
    }

    /// Synchronously revoke account-private emote lookup/cache ownership at an
    /// auth identity boundary. Existing bounded message slots keep the public
    /// Emote records captured when those rows entered Echo.
    pub fn revoke_user_emotes_residency(&self) -> bool {
        let _user_commit = self.user_emote_commit_lock.lock_or_recover();
        self.user_emote_request_generation
            .fetch_add(1, Ordering::SeqCst);
        let cached = {
            let mut user = self.user_emotes.write_or_recover();
            user.take().is_some()
        };
        let owner = self.user_emote_owner_id.write_or_recover().take().is_some();
        let loading = self.loading_user_emotes.lock_or_recover().take().is_some();
        let previous_lookup = USER_EMOTES_LOOKUP.load_full();
        let indexed = !previous_lookup.is_empty();
        let empty_lookup = Arc::new(HashMap::new());
        USER_EMOTES_LOOKUP.store(empty_lookup);
        drop(_user_commit);

        let changed = cached || owner || loading || indexed;
        if changed {
            crate::state::VersionRegistry::bump_user_emote_version();
            publish_emote_metadata_change();
        }
        changed
    }
}

#[derive(Deserialize)]
struct SevenTVEmoteSet {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emotes: Vec<SevenTVEmote>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SevenTVEmoteSetRefOrObject {
    Full(SevenTVEmoteSet),
    Id(String),
}

#[derive(Deserialize)]
struct SevenTVEmote {
    id: String,
    name: String,
    data: SevenTVEmoteData,
}

#[derive(Deserialize)]
struct SevenTVEmoteData {
    name: Option<String>,
    host: SevenTVHost,
    owner: Option<SevenTVOwner>,
    #[serde(default)]
    animated: bool,
    #[serde(default)]
    pub flags: u32,
}

#[derive(Deserialize)]
struct SevenTVOwner {
    display_name: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct SevenTVHost {
    url: String,
    files: Vec<SevenTVFile>,
}

#[derive(Deserialize)]
struct SevenTVFile {
    name: String,
    format: String,
    #[serde(default)]
    frame_count: u32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

#[derive(Deserialize)]
struct SevenTVUser {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emote_set: Option<SevenTVEmoteSetRefOrObject>,
    #[serde(default)]
    emote_set_id: Option<String>,
    #[serde(default)]
    user: Option<SevenTVUserProfile>,
}

/// Reduced 7TV user shape for personal-set discovery. Unknown channel-set and
/// identity fields are intentionally ignored by serde.
#[derive(Deserialize)]
struct SevenTVUserWithPersonalEmotes {
    user: Option<SevenTVUserProfile>,
}

/// Reference to an emote set in user.emote_sets array
#[derive(Deserialize)]
struct SevenTVEmoteSetRef {
    id: String,
    #[serde(default)]
    flags: u32,
}

/// User profile containing emote_sets array
#[derive(Deserialize)]
struct SevenTVUserProfile {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emote_sets: Vec<SevenTVEmoteSetRef>,
    #[serde(default)]
    connections: Vec<SevenTVConnection>,
}

#[derive(Deserialize)]
struct SevenTVConnection {
    #[serde(default)]
    id: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    emote_set_id: Option<String>,
}

/// Normalize 7TV host URL once (often schemeless like "//cdn.7tv.app/...").
/// Returns the base URL with trailing slash stripped for efficient appending.
fn normalize_7tv_host(host_url: &str) -> String {
    let trimmed = host_url.trim();
    let base = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else {
        format!("https://{}", trimmed)
    };
    base.trim_end_matches('/').to_string()
}

/// Build 7TV image URL from pre-normalized base (avoids repeated normalization).
#[inline]
fn build_7tv_url_from_base(base: &str, file_name: &str) -> String {
    let file = file_name.trim_start_matches('/');
    let mut url = String::with_capacity(base.len() + 1 + file.len());
    url.push_str(base);
    url.push('/');
    url.push_str(file);
    url
}

/// Find 7TV resolution files (1x, 2x, 4x) from a host's file list.
/// Returns (file_1x, file_2x, file_4x) - each may be None if not found.
/// Prefers WebP (animated decode support), then PNG, then a supported partial match.
fn find_7tv_resolution_files(
    files: &[SevenTVFile],
) -> (
    Option<&SevenTVFile>,
    Option<&SevenTVFile>,
    Option<&SevenTVFile>,
) {
    let find_res = |scale: &str| {
        files
            .iter()
            .find(|f| f.name == format!("{scale}.webp"))
            .or_else(|| files.iter().find(|f| f.name == format!("{scale}.png")))
            .or_else(|| {
                files.iter().find(|f| {
                    f.name.contains(scale)
                        && matches!(
                            f.format.to_ascii_uppercase().as_str(),
                            "WEBP" | "PNG" | "GIF"
                        )
                })
            })
    };
    (
        find_res("1x"),
        find_res("2x"),
        find_res("4x").or_else(|| find_res("3x")),
    )
}

#[inline]
fn clone_cached_7tv_global(ttl: Duration) -> Option<SevenTvGlobalFetchResult> {
    let cache = SEVENTV_GLOBAL_FETCH_CACHE.read_or_recover();
    cache
        .as_ref()
        .filter(|entry| entry.fetched_at.elapsed() <= ttl)
        .map(|entry| Arc::clone(&entry.emotes))
}

#[inline]
fn clone_cached_7tv_channel(cache_key: &str, ttl: Duration) -> Option<SevenTvChannelFetchResult> {
    SEVENTV_CHANNEL_FETCH_CACHE
        .get(cache_key)
        .filter(|entry| entry.fetched_at.elapsed() <= ttl)
        .map(|entry| Arc::clone(&entry.result))
}

fn prune_7tv_channel_fetch_cache() {
    let max_ttl = Duration::from_secs(SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS);
    SEVENTV_CHANNEL_FETCH_CACHE.retain(|_, entry| entry.fetched_at.elapsed() <= max_ttl);
    let len = SEVENTV_CHANNEL_FETCH_CACHE.len();
    if len <= SEVENTV_CHANNEL_FETCH_CACHE_MAX_ENTRIES {
        return;
    }

    let overflow = len - SEVENTV_CHANNEL_FETCH_CACHE_MAX_ENTRIES;
    let keys_to_remove: Vec<String> = SEVENTV_CHANNEL_FETCH_CACHE
        .iter()
        .take(overflow)
        .map(|entry| entry.key().clone())
        .collect();
    for key in keys_to_remove {
        SEVENTV_CHANNEL_FETCH_CACHE.remove(&key);
    }
}

fn invalidate_7tv_global_fetch_cache() {
    *SEVENTV_GLOBAL_FETCH_CACHE.write_or_recover() = None;
}

fn invalidate_7tv_channel_fetch_cache(channel_login: &str) {
    let channel_key = normalize_channel_key(channel_login);
    let suffix = format!(":{}", channel_key);
    SEVENTV_CHANNEL_FETCH_CACHE.retain(|key, _| !key.ends_with(&suffix));
    SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&channel_key);
}

pub(crate) fn invalidate_7tv_fetch_cache_for_event(channel_login: Option<&str>, is_global: bool) {
    if is_global {
        invalidate_7tv_global_fetch_cache();
    } else if let Some(channel) = channel_login {
        invalidate_7tv_channel_fetch_cache(channel);
    }
}

/// Channels whose in-progress manual reload already saw the 7TV fetch fail.
static SEVENTV_MANUAL_RELOAD_FAILED: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);

/// How long a manual reload waits for a concurrent channel load to finish
/// before it applies its fresh 7TV result directly.
const MANUAL_RELOAD_COALESCE_WAIT: Duration = Duration::from_secs(10);

/// F5 "reload channel emotes": refetch every provider for one
/// channel while bypassing the channel TTL and the EventAPI-window 7TV fetch
/// cache, re-register the channel's live 7TV set, and ask the EventAPI to
/// re-establish that subscription. Returns the chat status line.
pub(crate) async fn manual_reload_channel_emotes(
    channel_id: &str,
    channel_login: &str,
) -> (String, bool) {
    let cache_key = normalize_channel_key(channel_login);
    invalidate_7tv_channel_fetch_cache(&cache_key);

    let seventv = match tokio::time::timeout(
        Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
        fetch_7tv_channel(channel_id, &cache_key),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "timed out after {SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS}s"
        )),
    };

    // A failed direct fetch means 7TV is down: the forced load keeps the
    // cached 7TV rows instead of spending a second request on it.
    struct SuppressedSevenTvFetch(Option<String>);
    impl Drop for SuppressedSevenTvFetch {
        fn drop(&mut self) {
            if let Some(key) = self.0.take() {
                SEVENTV_MANUAL_RELOAD_FAILED.remove(&key);
            }
        }
    }
    let _suppressed = SuppressedSevenTvFetch(seventv.is_err().then(|| {
        SEVENTV_MANUAL_RELOAD_FAILED.insert(cache_key.clone());
        cache_key.clone()
    }));

    let wait_started = Instant::now();
    let load = loop {
        match EMOTE_CACHE
            .load_channel_emotes_reporting(channel_id, &cache_key, true)
            .await
        {
            Ok(LoadOutcome::Coalesced) if wait_started.elapsed() < MANUAL_RELOAD_COALESCE_WAIT => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            other => break other,
        }
    };

    if matches!(load, Ok(LoadOutcome::Coalesced))
        && let Ok(shared) = &seventv
        && let (
            SevenTvExactSetFetchOutcome::Present {
                emotes,
                emote_set_id,
            },
            subscription,
        ) = shared.as_ref()
    {
        EMOTE_CACHE
            .apply_channel_provider_emotes(
                &cache_key,
                &cache_key,
                EmoteProvider::SevenTV,
                emotes.iter().cloned(),
                emote_set_id.clone(),
            )
            .await;
        if let Some(subscription) = subscription.clone() {
            seventv_eventapi::subscribe_channel_details(&cache_key, subscription);
        }
    }
    seventv_eventapi::resubscribe_channel(&cache_key);

    if let Err(error) = load {
        crate::log_emotes!(
            warn,
            "Manual emote reload failed for {}: {}",
            cache_key,
            error
        );
        return (format!("Failed to reload channel emotes: {error}"), false);
    }

    let provider_count = |provider: EmoteProvider| {
        EMOTE_CACHE.channels.get(&cache_key).map_or(0, |entry| {
            entry
                .emotes_by_id
                .values()
                .filter(|emote| emote.provider == provider)
                .count()
        })
    };
    let seventv_status = match &seventv {
        Ok(shared) => match &shared.0 {
            SevenTvExactSetFetchOutcome::Present {
                emotes,
                emote_set_id: None,
            } if emotes.is_empty() => Ok(None),
            SevenTvExactSetFetchOutcome::TerminallyAbsent { .. } => Ok(None),
            SevenTvExactSetFetchOutcome::Present { .. } => {
                Ok(Some(provider_count(EmoteProvider::SevenTV)))
            }
        },
        Err(error) => Err(error.as_str()),
    };
    crate::log_emotes!(
        info,
        "Manual emote reload for {} finished (7TV: {:?})",
        cache_key,
        seventv_status
    );
    let ok = seventv_status.is_ok();
    (
        format_manual_reload_status(
            seventv_status,
            provider_count(EmoteProvider::BTTV),
            provider_count(EmoteProvider::FFZ),
        ),
        ok,
    )
}

fn format_manual_reload_status(
    seventv: Result<Option<usize>, &str>,
    bttv: usize,
    ffz: usize,
) -> String {
    let seventv_part = match seventv {
        Ok(Some(count)) => format!("7TV {count}, "),
        Ok(None) => "7TV none, ".to_string(),
        Err(_) => String::new(),
    };
    let summary =
        format!("Channel emotes reloaded ({seventv_part}BetterTTV {bttv}, FrankerFaceZ {ffz}).");
    match seventv {
        Err(error) => format!("{summary} Failed to fetch 7TV channel emotes: {error}"),
        Ok(_) => summary,
    }
}

pub(crate) fn seven_tv_dispatch_emote(
    value: &serde_json::Value,
    channel: Option<&str>,
) -> Option<Emote> {
    serde_json::from_value::<SevenTVEmote>(value.clone())
        .ok()
        .and_then(|emote| convert_7tv_emote(emote, channel))
}

pub(crate) async fn apply_7tv_channel_emote_patch(
    channel_login: &str,
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE
        .apply_channel_seventv_patch(channel_login, patch)
        .await
}

pub(crate) async fn apply_7tv_global_emote_patch(
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE.apply_global_seventv_patch(patch).await
}

pub(crate) async fn apply_7tv_user_emote_patch(
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE.apply_user_seventv_patch(patch).await
}

pub(crate) async fn clear_7tv_channel_emote_rows_if_set(
    channel_login: &str,
    expected_set_id: &str,
) -> Option<usize> {
    let cache_key = normalize_channel_key(channel_login);
    if EMOTE_CACHE
        .channel_emote_set_ids
        .get(&cache_key)
        .is_some_and(|current| current.value() != expected_set_id)
    {
        return None;
    }
    EMOTE_CACHE
        .channel_emote_set_ids
        .remove_if(&cache_key, |_, current| current == expected_set_id);
    Some(
        EMOTE_CACHE
            .clear_channel_provider_rows(&cache_key, EmoteProvider::SevenTV)
            .await,
    )
}

pub(crate) async fn force_refresh_user_emotes(user_id: &str, token: &str) -> Result<(), String> {
    EMOTE_CACHE
        .load_user_emotes_impl(user_id, token, None, false, true)
        .await?;
    rebuild_user_emote_index().await;
    Ok(())
}

pub(crate) async fn apply_7tv_channel_emote_rows(
    channel_login: &str,
    emotes: Vec<Arc<Emote>>,
    emote_set_id: &str,
) -> usize {
    let cache_key = normalize_channel_key(channel_login);
    let count = EMOTE_CACHE
        .apply_channel_provider_rows(
            &cache_key,
            channel_login,
            EmoteProvider::SevenTV,
            emotes,
            false,
        )
        .await;
    EMOTE_CACHE
        .channel_emote_set_ids
        .insert(cache_key, emote_set_id.to_string());
    count
}

pub(crate) async fn apply_7tv_global_emote_set(emotes: Vec<Arc<Emote>>) -> usize {
    EMOTE_CACHE
        .apply_global_provider_emotes(EmoteProvider::SevenTV, emotes)
        .await
}

pub(crate) async fn clear_7tv_global_emote_set_if_matches(expected_set_id: &str) -> Option<usize> {
    {
        let mut current = SEVENTV_GLOBAL_SET_ID.write_or_recover();
        if current
            .as_deref()
            .is_some_and(|set_id| set_id != expected_set_id)
        {
            return None;
        }
        if current.as_deref() == Some(expected_set_id) {
            current.take();
        }
    }
    Some(apply_7tv_global_emote_set(Vec::new()).await)
}

pub(crate) async fn apply_7tv_user_emote_set(emotes: Vec<Arc<Emote>>) -> usize {
    EMOTE_CACHE
        .apply_user_provider_emotes(EmoteProvider::SevenTV, emotes)
        .await
}

async fn fetch_7tv_global() -> Result<SevenTvGlobalFetchResult, String> {
    struct GlobalFetchInFlightGuard {
        notify: Option<Arc<tokio::sync::Notify>>,
    }

    impl Drop for GlobalFetchInFlightGuard {
        fn drop(&mut self) {
            if let Some(notify) = self.notify.take() {
                let mut in_flight = SEVENTV_GLOBAL_FETCH_IN_FLIGHT.lock_or_recover();
                in_flight.take();
                notify.notify_waiters();
            }
        }
    }

    let ttl = Duration::from_secs(SEVENTV_GLOBAL_FETCH_CACHE_TTL_SECS);

    loop {
        if let Some(cached) = clone_cached_7tv_global(ttl) {
            record_emote_stage_duration(
                "emotes.7tv.global.cache_hit",
                Duration::ZERO,
                cached.len(),
            );
            return Ok(cached);
        }

        let mut leader_guard: Option<GlobalFetchInFlightGuard> = None;
        let existing_request = {
            let mut in_flight = SEVENTV_GLOBAL_FETCH_IN_FLIGHT.lock_or_recover();
            if let Some(existing) = in_flight.as_ref() {
                // Created under the lock: `notify_waiters` stores no permit, so
                // a future created after the leader finishes would never wake.
                Some(Arc::clone(existing).notified_owned())
            } else {
                let notify = Arc::new(tokio::sync::Notify::new());
                *in_flight = Some(Arc::clone(&notify));
                leader_guard = Some(GlobalFetchInFlightGuard {
                    notify: Some(notify),
                });
                None
            }
        };

        if let Some(wait_for) = existing_request {
            let wait_start = Instant::now();
            wait_for.await;
            record_emote_stage_timed("emotes.7tv.global.wait_inflight", wait_start, 0);
            continue;
        }

        let fetch_start = Instant::now();
        let result = fetch_7tv_global_raw().await.map(Arc::new);
        let item_count = result.as_ref().map(|emotes| emotes.len()).unwrap_or(0);
        record_emote_stage_timed("emotes.7tv.global.network", fetch_start, item_count);

        if let Ok(emotes) = &result {
            let cache_store_start = Instant::now();
            *SEVENTV_GLOBAL_FETCH_CACHE.write_or_recover() = Some(SevenTVGlobalFetchCacheEntry {
                fetched_at: Instant::now(),
                emotes: Arc::clone(emotes),
            });
            record_emote_stage_timed(
                "emotes.7tv.global.cache_store",
                cache_store_start,
                emotes.len(),
            );
        }
        drop(leader_guard);

        return result;
    }
}

async fn fetch_7tv_global_raw() -> Result<Vec<Arc<Emote>>, String> {
    SEVENTV_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &SEVENTV_EMOTE_SET_HTTP_CLIENT,
        api::seventv::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(format!(
            "7TV global emotes returned status: {}",
            resp.status()
        ));
    }
    let set: SevenTVEmoteSet = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(ref set_id) = set.id {
        *SEVENTV_GLOBAL_SET_ID.write_or_recover() = Some(set_id.clone());
        seventv_eventapi::subscribe_global_set(set_id);
    }
    Ok(convert_7tv_emotes(set.emotes, None)
        .into_iter()
        .map(Arc::new)
        .collect())
}

pub(crate) async fn fetch_7tv_emote_set_by_id(
    set_id: &str,
    channel_login: Option<&str>,
) -> Result<SevenTvExactSetFetchOutcome, String> {
    SEVENTV_LIMITER.until_ready().await;

    let set_url = api::seventv::emote_set(set_id);
    let set_resp = fetch_with_retry_str(
        &SEVENTV_EMOTE_SET_HTTP_CLIENT,
        &set_url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await?;
    let status = set_resp.status();
    if status.as_u16() == 410 {
        crate::backend::metrics::record_emote_event("7tv.set_fetch.terminal_absent");
        crate::log_emotes!(
            info,
            "7TV emote set {} is terminally absent (HTTP {})",
            set_id,
            status
        );
        return Ok(SevenTvExactSetFetchOutcome::TerminallyAbsent {
            emote_set_id: set_id.to_string(),
        });
    }
    if !status.is_success() {
        crate::backend::metrics::record_emote_event("7tv.set_fetch.http_preserved");
        let error = format!(
            "7TV emote set {} returned HTTP {}; preserving existing provider state",
            set_id, status
        );
        crate::log_emotes!(warn, "{}", error);
        return Err(error);
    }

    let set: SevenTVEmoteSet = set_resp.json().await.map_err(|e| e.to_string())?;
    let emotes = convert_7tv_emotes(set.emotes, channel_login)
        .into_iter()
        .map(Arc::new)
        .collect();
    Ok(SevenTvExactSetFetchOutcome::Present {
        emotes,
        emote_set_id: set.id.or_else(|| Some(set_id.to_string())),
    })
}

/// Fetches 7TV channel emotes and returns (emotes, emote_set_id)
/// The emote_set_id is used for tracking changes and EventAPI subscriptions
async fn fetch_7tv_channel(
    channel_id: &str,
    channel_login: &str,
) -> Result<SevenTvChannelFetchResult, String> {
    struct ChannelFetchInFlightGuard {
        key: String,
        notify: Option<Arc<tokio::sync::Notify>>,
    }

    impl Drop for ChannelFetchInFlightGuard {
        fn drop(&mut self) {
            if let Some(notify) = self.notify.take() {
                let mut in_flight = SEVENTV_CHANNEL_FETCH_IN_FLIGHT.lock_or_recover();
                in_flight.remove(&self.key);
                notify.notify_waiters();
            }
        }
    }

    if SEVENTV_MANUAL_RELOAD_FAILED.contains(&normalize_channel_key(channel_login)) {
        return Err("7TV fetch already failed during this manual reload".to_string());
    }
    let eventapi_updates_live = seventv_eventapi::channel_updates_are_live(channel_login);
    let ttl = Duration::from_secs(if eventapi_updates_live {
        SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS
    } else {
        SEVENTV_CHANNEL_FETCH_CACHE_TTL_SECS
    });
    let cache_key = format!("{}:{}", channel_id, channel_login.to_ascii_lowercase());

    loop {
        if let Some(cached) = clone_cached_7tv_channel(&cache_key, ttl) {
            record_emote_stage_duration(
                if eventapi_updates_live {
                    "emotes.7tv.channel.cache_hit.eventapi_live"
                } else {
                    "emotes.7tv.channel.cache_hit.fallback_polling"
                },
                Duration::ZERO,
                cached.0.emote_count(),
            );
            return Ok(cached);
        }

        let mut leader_guard: Option<ChannelFetchInFlightGuard> = None;
        let existing_request = {
            let mut in_flight = SEVENTV_CHANNEL_FETCH_IN_FLIGHT.lock_or_recover();
            if let Some(existing) = in_flight.get(&cache_key) {
                // Created under the lock; see the global fetch above.
                Some(Arc::clone(existing).notified_owned())
            } else {
                let notify = Arc::new(tokio::sync::Notify::new());
                in_flight.insert(cache_key.clone(), Arc::clone(&notify));
                leader_guard = Some(ChannelFetchInFlightGuard {
                    key: cache_key.clone(),
                    notify: Some(notify),
                });
                None
            }
        };

        if let Some(wait_for) = existing_request {
            let wait_start = Instant::now();
            wait_for.await;
            record_emote_stage_timed("emotes.7tv.channel.wait_inflight", wait_start, 0);
            continue;
        }

        let fetch_start = Instant::now();
        let result = fetch_7tv_channel_raw(channel_id, channel_login)
            .await
            .map(Arc::new);
        let item_count = result
            .as_ref()
            .map(|result| result.0.emote_count())
            .unwrap_or(0);
        record_emote_stage_timed("emotes.7tv.channel.network", fetch_start, item_count);

        if let Ok(shared_result) = &result {
            let cache_store_start = Instant::now();
            SEVENTV_CHANNEL_FETCH_CACHE.insert(
                cache_key.clone(),
                SevenTVChannelFetchCacheEntry {
                    fetched_at: Instant::now(),
                    result: Arc::clone(shared_result),
                },
            );
            prune_7tv_channel_fetch_cache();
            record_emote_stage_timed(
                "emotes.7tv.channel.cache_store",
                cache_store_start,
                shared_result.0.emote_count(),
            );
        }
        drop(leader_guard);

        return result;
    }
}

async fn fetch_7tv_channel_raw(
    channel_id: &str,
    channel_login: &str,
) -> Result<SevenTvChannelFetchPayload, String> {
    SEVENTV_LIMITER.until_ready().await;

    let url = api::seventv::user(channel_id);
    let resp = fetch_with_retry_str(
        &SEVENTV_USER_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await
    .map_err(|e| {
        crate::log_emotes!(
            debug,
            "7TV channel emotes unavailable for {} ({}): {} - will retry on next refresh",
            channel_login,
            channel_id,
            e
        );
        e
    })?;

    if !resp.status().is_success() {
        return Ok((
            SevenTvExactSetFetchOutcome::Present {
                emotes: vec![],
                emote_set_id: None,
            },
            None,
        ));
    }

    let user: SevenTVUser = resp.json().await.map_err(|e| e.to_string())?;
    let subscription_identity = seven_tv_channel_subscription_identity(&user, channel_id);

    match user.emote_set {
        Some(SevenTVEmoteSetRefOrObject::Full(set)) => {
            if set.emotes.is_empty()
                && let Some(set_id) = set.id.clone().or_else(|| user.emote_set_id.clone())
            {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} had empty emote_set object, fetching set {} separately",
                    channel_login,
                    set_id
                );
                let outcome = fetch_7tv_emote_set_by_id(&set_id, Some(channel_login)).await?;
                let subscription =
                    seven_tv_channel_subscription_from_identity(&subscription_identity, &set_id);
                return Ok((outcome, subscription));
            }

            let emote_set_id = set.id.clone().or(user.emote_set_id.clone());
            let subscription = emote_set_id.as_deref().and_then(|set_id| {
                seven_tv_channel_subscription_from_identity(&subscription_identity, set_id)
            });
            let emotes = convert_7tv_emotes(set.emotes, Some(channel_login))
                .into_iter()
                .map(Arc::new)
                .collect();
            Ok((
                SevenTvExactSetFetchOutcome::Present {
                    emotes,
                    emote_set_id,
                },
                subscription,
            ))
        }
        Some(SevenTVEmoteSetRefOrObject::Id(set_id)) => {
            crate::log_emotes!(
                debug,
                "7TV user payload for {} returned emote_set as id {}, fetching separately",
                channel_login,
                set_id
            );
            let outcome = fetch_7tv_emote_set_by_id(&set_id, Some(channel_login)).await?;
            let subscription =
                seven_tv_channel_subscription_from_identity(&subscription_identity, &set_id);
            Ok((outcome, subscription))
        }
        None => {
            if let Some(set_id) = user.emote_set_id {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} returned emote_set_id {}, fetching separately",
                    channel_login,
                    set_id
                );
                let outcome = fetch_7tv_emote_set_by_id(&set_id, Some(channel_login)).await?;
                let subscription =
                    seven_tv_channel_subscription_from_identity(&subscription_identity, &set_id);
                Ok((outcome, subscription))
            } else {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} had no emote_set or emote_set_id",
                    channel_login
                );
                Ok((
                    SevenTvExactSetFetchOutcome::Present {
                        emotes: vec![],
                        emote_set_id: None,
                    },
                    None,
                ))
            }
        }
    }
}

fn seven_tv_channel_subscription_identity(
    user: &SevenTVUser,
    channel_id: &str,
) -> Option<(String, usize)> {
    let profile = user.user.as_ref()?;
    let user_id = profile
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .or_else(|| user.id.as_deref().filter(|id| !id.is_empty()))?;
    let connection_index = seven_tv_twitch_connection_index(user, profile, channel_id)?;
    Some((user_id.to_string(), connection_index))
}

fn seven_tv_twitch_connection_index(
    user: &SevenTVUser,
    profile: &SevenTVUserProfile,
    channel_id: &str,
) -> Option<usize> {
    let twitch_connections: Vec<_> = profile
        .connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| connection.platform.eq_ignore_ascii_case("TWITCH"))
        .collect();

    if let Some((index, _)) = twitch_connections
        .iter()
        .find(|(_, connection)| connection.id == channel_id)
    {
        return Some(*index);
    }

    let target_set_id = user
        .emote_set
        .as_ref()
        .and_then(seven_tv_emote_set_ref_id)
        .or(user.emote_set_id.as_deref())
        .filter(|set_id| !set_id.is_empty());

    if let Some(target_set_id) = target_set_id
        && let Some((index, _)) = twitch_connections.iter().find(|(_, connection)| {
            connection
                .emote_set_id
                .as_deref()
                .is_some_and(|set_id| set_id == target_set_id)
        })
    {
        return Some(*index);
    }

    if twitch_connections.len() == 1 {
        return Some(twitch_connections[0].0);
    }

    None
}

fn seven_tv_emote_set_ref_id(value: &SevenTVEmoteSetRefOrObject) -> Option<&str> {
    match value {
        SevenTVEmoteSetRefOrObject::Full(set) => set.id.as_deref(),
        SevenTVEmoteSetRefOrObject::Id(set_id) if !set_id.is_empty() => Some(set_id.as_str()),
        SevenTVEmoteSetRefOrObject::Id(_) => None,
    }
}

fn seven_tv_channel_subscription_from_identity(
    identity: &Option<(String, usize)>,
    emote_set_id: &str,
) -> Option<seventv_eventapi::SevenTvChannelSubscription> {
    if emote_set_id.is_empty() {
        return None;
    }
    let (user_id, connection_index) = identity.as_ref()?;
    Some(seventv_eventapi::SevenTvChannelSubscription {
        user_id: user_id.clone(),
        emote_set_id: emote_set_id.to_string(),
        connection_index: *connection_index,
    })
}

/// Fetch 7TV personal emotes for a user
/// Personal emotes are a 7TV subscriber feature that allows up to 5 emotes to be used globally
/// They are stored in a separate emote set with flags == 4 (Personal flag)
async fn fetch_7tv_personal_emotes(
    twitch_user_id: &str,
) -> Result<SevenTvExactSetFetchOutcome, String> {
    crate::log_emotes!(
        info,
        "Fetching 7TV personal emotes for Twitch user: {}",
        twitch_user_id
    );

    let url = api::seventv::user(twitch_user_id);
    let resp = fetch_with_retry_str(
        &SEVENTV_USER_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await
    .map_err(|e| {
        crate::log_emotes!(info, "7TV personal emotes unavailable: {}", e);
        e
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err("User not found on 7TV".to_string());
        }
        return Err(format!("7TV API returned status: {}", status));
    }

    let user_data: SevenTVUserWithPersonalEmotes = resp.json().await.map_err(|e| {
        crate::log_emotes!(info, "Failed to parse 7TV user response: {}", e);
        e.to_string()
    })?;

    let personal_set_id = user_data
        .user
        .and_then(|u| u.emote_sets.into_iter().find(|s| s.flags == 4))
        .map(|s| s.id);

    let Some(set_id) = personal_set_id else {
        crate::log_emotes!(
            info,
            "No personal emote set found for user {}",
            twitch_user_id
        );
        return Ok(SevenTvExactSetFetchOutcome::Present {
            emotes: vec![],
            emote_set_id: None,
        });
    };

    crate::log_emotes!(
        info,
        "Found personal emote set ID: {} for user {}",
        set_id,
        twitch_user_id
    );

    fetch_7tv_emote_set_by_id(&set_id, Some(PERSONAL_SEVENTV_CHANNEL)).await
}

fn convert_7tv_emotes(emotes: Vec<SevenTVEmote>, channel: Option<&str>) -> Vec<Emote> {
    emotes
        .into_iter()
        .filter_map(|emote| convert_7tv_emote(emote, channel))
        .collect()
}

fn convert_7tv_emote(e: SevenTVEmote, channel: Option<&str>) -> Option<Emote> {
    let (file_1x, file_2x, file_4x) = find_7tv_resolution_files(&e.data.host.files);

    let default_file = file_1x
        .or(file_2x)
        .or(file_4x)
        .or_else(|| e.data.host.files.first())?;

    let original_name = e.data.name.filter(|orig| orig != &e.name);

    let creator_name = e
        .data
        .owner
        .and_then(|owner| owner.display_name.or(owner.username));

    let base_url = normalize_7tv_host(&e.data.host.url);
    let default_url = build_7tv_url_from_base(&base_url, &default_file.name);

    // Prefer the smallest available source for logical dimensions, but fall
    // through when that file has missing/invalid geometry. Keep width/height
    // paired: a partial provider dimension is not authoritative.
    let dimension_candidates = [
        file_1x.map(|file| (file, 1u32)),
        file_2x.map(|file| (file, 2u32)),
        file_4x.map(|file| {
            (
                file,
                if file.name.starts_with("3x.") || file.name.starts_with("3x_") {
                    3
                } else {
                    4
                },
            )
        }),
    ];
    let (width, height) = dimension_candidates
        .into_iter()
        .flatten()
        .chain(e.data.host.files.iter().filter_map(|file| {
            let (tier, suffix) = file.name.split_once('x')?;
            if !suffix.starts_with('.') && !suffix.starts_with('_') {
                return None;
            }
            let scale = match tier {
                "1" => 1,
                "2" => 2,
                "3" => 3,
                "4" => 4,
                _ => return None,
            };
            Some((file, scale))
        }))
        .find_map(|(file, scale)| {
            (file.width > 0 && file.height > 0).then(|| {
                (
                    (file.width as f32 / scale as f32).round().max(1.0) as u32,
                    (file.height as f32 / scale as f32).round().max(1.0) as u32,
                )
            })
        })
        .map_or((None, None), |(width, height)| (Some(width), Some(height)));
    let is_animated = e.data.animated
        || (e.data.flags & 1) != 0
        || default_file.frame_count > 1
        || default_file.format.eq_ignore_ascii_case("GIF");

    Some(Emote {
        id: Arc::from(e.id.clone()),
        name: Arc::from(e.name),
        url: Arc::from(default_url),
        provider: EmoteProvider::SevenTV,
        channel: channel.map(Arc::from),
        original_name: original_name.map(Arc::from),
        creator_name: creator_name.map(Arc::from),
        twitch_owner_login: None,
        twitch_owner_id: None,
        twitch_emote_type: None,
        url_1x: file_1x.map(|f| Arc::from(build_7tv_url_from_base(&base_url, &f.name))),
        url_2x: file_2x.map(|f| Arc::from(build_7tv_url_from_base(&base_url, &f.name))),
        url_4x: file_4x.map(|f| Arc::from(build_7tv_url_from_base(&base_url, &f.name))),
        zero_width: (e.data.flags & 256) != 0,
        is_animated,
        width,
        height,
    })
}

#[derive(Deserialize)]
struct BTTVEmote {
    id: String,
    code: String,
    #[serde(default)]
    animated: bool,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Deserialize)]
struct BTTVChannelResponse {
    #[serde(rename = "channelEmotes")]
    channel_emotes: Vec<BTTVEmote>,
    #[serde(rename = "sharedEmotes")]
    shared_emotes: Vec<BTTVEmote>,
}

async fn fetch_bttv_global() -> Result<Vec<Emote>, String> {
    BTTV_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        api::bttv::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&BTTV_LIMITER),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(format!(
            "BTTV global emotes returned status: {}",
            resp.status()
        ));
    }
    let emotes: Vec<BTTVEmote> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(convert_bttv_emotes(emotes, None))
}

async fn fetch_bttv_channel(channel_id: &str, channel_login: &str) -> Result<Vec<Emote>, String> {
    BTTV_LIMITER.until_ready().await;

    let url = api::bttv::channel(channel_id);
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&BTTV_LIMITER),
    )
    .await?;

    // BTTV answers 404 for a channel that has never set up BTTV emotes.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    if !resp.status().is_success() {
        return Err(format!(
            "BTTV channel API returned status: {}",
            resp.status()
        ));
    }

    let response: BTTVChannelResponse = resp.json().await.map_err(|e| e.to_string())?;

    let mut emotes = convert_bttv_emotes(response.channel_emotes, Some(channel_login));
    emotes.extend(convert_bttv_emotes(
        response.shared_emotes,
        Some(channel_login),
    ));
    Ok(emotes)
}

/// Build BTTV emote CDN URL for a given scale (1x, 2x, 3x).
#[inline]
fn bttv_emote_url(id: &str, scale: &str) -> String {
    const BTTV_CDN: &str = "https://cdn.betterttv.net/emote";
    format!("{BTTV_CDN}/{id}/{scale}")
}

fn convert_bttv_emotes(emotes: Vec<BTTVEmote>, channel: Option<&str>) -> Vec<Emote> {
    emotes
        .into_iter()
        .map(|e| {
            let is_animated = e.animated;
            let (width, height) = match (e.width, e.height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    (Some(width), Some(height))
                }
                _ => (None, None),
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.code),
                url: Arc::from(bttv_emote_url(&e.id, "1x")),
                provider: EmoteProvider::BTTV,
                channel: channel.map(Arc::from),
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: Some(Arc::from(bttv_emote_url(&e.id, "1x"))),
                url_2x: Some(Arc::from(bttv_emote_url(&e.id, "2x"))),
                url_4x: Some(Arc::from(bttv_emote_url(&e.id, "3x"))),
                zero_width: false,
                is_animated,
                width,
                height,
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct FFZGlobalResponse {
    #[serde(default)]
    default_sets: Vec<i64>,
    #[serde(default)]
    sets: HashMap<String, FFZEmoteSet>,
}

#[derive(Deserialize)]
struct FFZRoomResponse {
    room: Option<FFZRoom>,
    sets: HashMap<String, FFZEmoteSet>,
}

/// FFZ sends `null` (not an omitted key) for a room without custom badge
/// artwork, e.g. `"mod_urls": null` beside a custom `vip_badge`.
#[derive(Deserialize)]
struct FFZRoom {
    twitch_id: i64,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    mod_urls: HashMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    vip_badge: HashMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    user_badge_ids: HashMap<String, Vec<serde_json::Value>>,
}

fn deserialize_null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct FFZEmoteSet {
    #[serde(default)]
    emoticons: Vec<FFZEmote>,
}

#[derive(Deserialize)]
struct FFZEmote {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    urls: HashMap<String, String>,
    /// FFZ exposes animated variants in a parallel URL map. When present,
    /// these are the actual animated assets; the regular `urls` map may still
    /// contain static fallbacks.
    #[serde(default, deserialize_with = "deserialize_ffz_animated_urls")]
    animated: HashMap<String, String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

async fn fetch_ffz_global() -> Result<Vec<Emote>, String> {
    FFZ_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        api::ffz::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&FFZ_LIMITER),
    )
    .await?;

    if !resp.status().is_success() {
        return Err(format!("FFZ API returned status: {}", resp.status()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read FFZ response: {}", e))?;

    let response: FFZGlobalResponse = serde_json::from_str(&body).map_err(|e| {
        crate::log_emotes!(
            debug,
            "FFZ parse error: {}. Response preview: {}",
            e,
            truncate_for_log(&body, 500)
        );
        format!("Failed to parse FFZ response: {}", e)
    })?;

    let total_emotes: usize = response
        .default_sets
        .iter()
        .filter_map(|id| response.sets.get(&id.to_string()))
        .map(|set| set.emoticons.len())
        .sum();
    let mut emotes = Vec::with_capacity(total_emotes);
    for set_id in &response.default_sets {
        if let Some(set) = response.sets.get(&set_id.to_string()) {
            emotes.extend(convert_ffz_emotes(&set.emoticons, None));
        }
    }
    crate::log_emotes!(info, "Loaded {} global FFZ emotes", emotes.len());
    Ok(emotes)
}

/// Fetch FFZ channel data and return both emotes and Twitch user ID
async fn fetch_ffz_channel_with_id(
    channel_login: &str,
) -> Result<(Vec<Emote>, Option<String>), String> {
    let Some(response) = fetch_ffz_room(channel_login).await? else {
        publish_ffz_room_badges(channel_login, None);
        return Ok((vec![], None));
    };

    publish_ffz_room_badges(channel_login, response.room.as_ref());
    let twitch_id = response
        .room
        .as_ref()
        .map(|room| room.twitch_id.to_string());

    let total_emotes: usize = response.sets.values().map(|set| set.emoticons.len()).sum();
    let mut emotes = Vec::with_capacity(total_emotes);
    for set in response.sets.values() {
        emotes.extend(convert_ffz_emotes(&set.emoticons, Some(channel_login)));
    }
    Ok((emotes, twitch_id))
}

async fn fetch_ffz_room(channel_login: &str) -> Result<Option<FFZRoomResponse>, String> {
    FFZ_LIMITER.until_ready().await;

    let url = api::ffz::room(channel_login);
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&FFZ_LIMITER),
    )
    .await?;

    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        return Err(format!("FFZ room API returned status: {}", resp.status()));
    }

    let response: FFZRoomResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(Some(response))
}

fn publish_ffz_room_badges(channel_login: &str, room: Option<&FFZRoom>) {
    let moderator_url = room.and_then(|room| preferred_ffz_room_badge_url(&room.mod_urls));
    let vip_url = room.and_then(|room| preferred_ffz_room_badge_url(&room.vip_badge));
    let badges_by_user_id = room
        .map(|room| invert_ffz_channel_badge_users(&room.user_badge_ids))
        .unwrap_or_default();

    crate::backend::badges::TWITCH_BADGE_CACHE.publish_ffz_channel_badges(
        channel_login,
        moderator_url,
        vip_url,
        badges_by_user_id,
    );
}

fn schedule_ffz_room_badge_hydration(channel_login: &str) {
    if !crate::backend::settings::get_show_ffz_badges()
        || crate::backend::badges::TWITCH_BADGE_CACHE
            .has_ffz_channel_badge_state_sync(channel_login)
    {
        return;
    }

    let channel_login = channel_login
        .trim()
        .trim_start_matches('#')
        .to_ascii_lowercase();
    if channel_login.is_empty() || !FFZ_ROOM_BADGE_HYDRATIONS.insert(channel_login.clone()) {
        return;
    }

    crate::backend::bridge::RUNTIME.spawn(async move {
        match fetch_ffz_room(&channel_login).await {
            Ok(response) => {
                publish_ffz_room_badges(
                    &channel_login,
                    response
                        .as_ref()
                        .and_then(|response| response.room.as_ref()),
                );
            }
            Err(error) => {
                crate::log_cache!(
                    warn,
                    channel = channel_login,
                    %error,
                    "Failed to hydrate FFZ room badges beside fresh channel emote cache"
                );
            }
        }
        FFZ_ROOM_BADGE_HYDRATIONS.remove(&channel_login);
    });
}

async fn fetch_ffz_channel(channel_login: &str) -> Result<Vec<Emote>, String> {
    let (emotes, _) = fetch_ffz_channel_with_id(channel_login).await?;
    Ok(emotes)
}

/// Normalize FFZ URL (often protocol-relative like "//cdn.frankerfacez.com/...").
fn normalize_ffz_url(urls: &HashMap<String, String>, key: &str) -> Option<String> {
    urls.get(key).map(|u| {
        if u.starts_with("//") {
            format!("https:{}", u)
        } else {
            u.clone()
        }
    })
}

fn preferred_ffz_room_badge_url(urls: &HashMap<String, String>) -> Option<String> {
    normalize_ffz_url(urls, "4")
        .or_else(|| normalize_ffz_url(urls, "2"))
        .or_else(|| normalize_ffz_url(urls, "1"))
}

fn invert_ffz_channel_badge_users(
    assignments: &HashMap<String, Vec<serde_json::Value>>,
) -> HashMap<String, Vec<u64>> {
    let mut by_user_id: HashMap<String, Vec<u64>> = HashMap::new();
    for (badge_id, users) in assignments {
        let Ok(badge_id) = badge_id.parse::<u64>() else {
            continue;
        };
        for user in users {
            let user_id = user
                .as_str()
                .map(str::to_string)
                .or_else(|| user.as_u64().map(|id| id.to_string()));
            let Some(user_id) = user_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            let badges = by_user_id.entry(user_id).or_default();
            if !badges.contains(&badge_id) {
                badges.push(badge_id);
            }
        }
    }
    for badges in by_user_id.values_mut() {
        badges.sort_unstable();
    }
    by_user_id
}

#[inline]
fn url_looks_animated(url: &str) -> bool {
    #[inline]
    fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    }

    let bytes = url.as_bytes();
    bytes
        .get(bytes.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".gif"))
        || contains_ascii_case_insensitive(bytes, b"/animated/")
        || contains_ascii_case_insensitive(bytes, b"_animated.")
        || contains_ascii_case_insensitive(bytes, b"format=gif")
}

fn deserialize_ffz_animated_urls<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|url| (key.clone(), url.to_owned())))
                .collect()
        })
        .unwrap_or_default())
}

fn convert_ffz_emotes(emotes: &[FFZEmote], channel: Option<&str>) -> Vec<Emote> {
    emotes
        .iter()
        .filter_map(|e| {
            let has_animated_map = !e.animated.is_empty();
            let animated_variant = |key: &str| {
                normalize_ffz_url(&e.animated, key)
                    .or_else(|| normalize_ffz_url(&e.animated, "2"))
                    .or_else(|| normalize_ffz_url(&e.animated, "1"))
                    .or_else(|| normalize_ffz_url(&e.animated, "4"))
            };
            let url_1x = animated_variant("1").or_else(|| normalize_ffz_url(&e.urls, "1"));
            let url_2x = animated_variant("2").or_else(|| normalize_ffz_url(&e.urls, "2"));
            let url_4x = animated_variant("4").or_else(|| normalize_ffz_url(&e.urls, "4"));

            let default_url = url_1x.clone().or(url_2x.clone()).or(url_4x.clone())?;

            let is_animated = has_animated_map
                || [url_1x.as_deref(), url_2x.as_deref(), url_4x.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(url_looks_animated)
                || url_looks_animated(&default_url);
            let (width, height) = match (e.width, e.height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    (Some(width), Some(height))
                }
                _ => (None, None),
            };
            Some(Emote {
                id: Arc::from(e.id.to_string()),
                name: Arc::from(e.name.clone()),
                url: Arc::from(default_url),
                provider: EmoteProvider::FFZ,
                channel: channel.map(Arc::from),
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: url_1x.map(Arc::from),
                url_2x: url_2x.map(Arc::from),
                url_4x: url_4x.map(Arc::from),
                zero_width: false,
                is_animated,
                width,
                height,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct TwitchEmotesResponse {
    data: Vec<TwitchEmote>,
}

#[derive(Deserialize)]
struct TwitchEmote {
    id: String,
    name: String,
    images: TwitchEmoteImages,
    #[serde(default)]
    emote_type: String,
    #[serde(default)]
    format: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct TwitchEmoteImages {
    #[serde(default)]
    url_1x: String,
    url_2x: String,
    #[serde(default)]
    url_4x: String,
}

const TWITCH_EMOTE_CDN_TEMPLATE: &str =
    "https://static-cdn.jtvnw.net/emoticons/v2/{{id}}/{{format}}/{{theme_mode}}/{{scale}}";

fn twitch_emote_variant_url(template: &str, id: &str, is_animated: bool, scale: &str) -> String {
    template
        .replace("{{id}}", id)
        .replace(
            "{{format}}",
            if is_animated { "animated" } else { "static" },
        )
        .replace("{{theme_mode}}", "dark")
        .replace("{{scale}}", scale)
}

/// Fetch Twitch channel emotes (subscriber emotes) using Helix API
/// Requires an OAuth token with user:read:emotes or channel:read:emotes scope
pub async fn fetch_twitch_channel_emotes(
    channel_id: &str,
    token: Option<&str>,
    channel_login: &str,
) -> Result<Vec<Emote>, String> {
    let Some(initial_token) = token else {
        return Ok(vec![]);
    };

    let url = api::twitch::channel_emotes(channel_id);
    let mut token_to_use = initial_token.to_string();

    let mut resp = retry_helix_get_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        CLIENT_ID,
        &token_to_use,
        EMOTE_MAX_RETRY_AFTER,
    )
    .await?;

    if matches!(resp.status().as_u16(), 401 | 403) {
        crate::log_emotes!(
            warn,
            "Twitch channel emotes unauthorized for {} (status {}), attempting token refresh",
            channel_login,
            resp.status()
        );
        match crate::backend::auth::refresh_and_save_tokens().await {
            Ok(new_token) => {
                token_to_use = new_token;
                resp = retry_helix_get_str(
                    &EMOTE_HTTP_CLIENT,
                    &url,
                    CLIENT_ID,
                    &token_to_use,
                    EMOTE_MAX_RETRY_AFTER,
                )
                .await?;
            }
            Err(err) => {
                return Err(format!(
                    "token refresh failed after status {}: {}",
                    resp.status(),
                    err
                ));
            }
        }
    }

    if !resp.status().is_success() {
        return Err(format!(
            "Helix channel emotes returned status: {}",
            resp.status()
        ));
    }

    let response: TwitchEmotesResponse = resp.json().await.map_err(|e| e.to_string())?;

    let emotes: Vec<Emote> = response
        .data
        .into_iter()
        .map(|e| {
            let is_animated = e.format.iter().any(|f| f == "animated");
            let (url_1x, url_2x, url_4x) = if is_animated {
                (
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "1.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "2.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "3.0"),
                )
            } else {
                (e.images.url_1x, e.images.url_2x, e.images.url_4x)
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.name),
                url: Arc::from(url_1x.clone()),
                provider: EmoteProvider::Twitch,
                channel: Some(Arc::from(channel_login)),
                original_name: None,
                creator_name: None,
                twitch_owner_login: Some(Arc::from(channel_login)),
                twitch_owner_id: Some(Arc::from(channel_id)),
                twitch_emote_type: Some(Arc::from(if e.emote_type.is_empty() {
                    "subscriptions"
                } else {
                    e.emote_type.as_str()
                })),
                url_1x: Some(Arc::from(url_1x)),
                url_2x: Some(Arc::from(url_2x)),
                url_4x: Some(Arc::from(url_4x)),
                zero_width: false,
                is_animated,
                width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
                height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
            }
        })
        .collect();

    Ok(emotes)
}

#[derive(Deserialize, Debug)]
struct TwitchUserEmote {
    id: String,
    name: String,
    #[serde(default)]
    owner_id: String,
    #[serde(default)]
    emote_type: String,
    #[serde(default)]
    format: Vec<String>,
}

/// Fetch all emotes the user has access to (subscriptions, bits, follower emotes)
/// Fetch the user's Twitch emotes. The flag is `false` when pagination stopped
/// early after errors, so the caller can keep its previous set alongside the
/// partial result instead of replacing it.
pub async fn fetch_twitch_user_emotes(
    user_id: &str,
    token: &str,
) -> Result<(Vec<Emote>, bool), String> {
    crate::log_emotes!(debug, "Fetching user emotes for user_id: {}", user_id);
    let mut complete = true;
    let mut all_raw_emotes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut template = String::new();
    let mut page_num = 0;

    let pagination_backoff = || {
        backon::ExponentialBuilder::default()
            .with_min_delay(std::time::Duration::from_millis(1000))
            .with_max_delay(std::time::Duration::from_secs(30))
            .with_max_times(7)
            .with_jitter()
    };

    let mut rate_limit_remaining: Option<u32> = None;
    let mut consecutive_errors = 0;
    let fetch_started = Instant::now();

    loop {
        if page_num > 0 {
            let adaptive_delay = if consecutive_errors > 0 {
                Some(std::time::Duration::from_millis(
                    2000 * (consecutive_errors as u64),
                ))
            } else if rate_limit_remaining.map(|r| r < 50).unwrap_or(false) {
                Some(std::time::Duration::from_millis(1000))
            } else {
                None
            };
            if let Some(delay) = adaptive_delay {
                tokio::time::sleep(delay).await;
            }
        }
        page_num += 1;

        let mut url = api::twitch::user_emotes(user_id);
        if let Some(ref c) = cursor {
            url.push_str(&format!("&after={}", urlencoding::encode(c)));
        }

        crate::log_emotes!(trace, "User emotes URL (page {}): {}", page_num, url);

        let resp = (|| {
            let url = url.clone();
            let token = token.to_string();
            async move { EMOTE_HTTP_CLIENT.get(&url).helix_auth(&token).send().await }
        })
        .retry(pagination_backoff())
        .when(is_reqwest_transient)
        .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                consecutive_errors += 1;
                crate::log_emotes!(
                    warn,
                    "Request error on page {} (attempt {}): {}",
                    page_num,
                    consecutive_errors,
                    e
                );
                if cursor.is_none() {
                    return Err(e.to_string());
                }
                if consecutive_errors < 3 {
                    crate::log_emotes!(info, "Retrying page {} after delay...", page_num);
                    page_num -= 1;
                    continue;
                }
                crate::log_emotes!(
                    warn,
                    "Pagination failed on page {} after {} errors, continuing with {} emotes from previous pages",
                    page_num,
                    consecutive_errors,
                    all_raw_emotes.len()
                );
                complete = false;
                break;
            }
        };

        if let Some(remaining) = resp.headers().get("ratelimit-remaining")
            && let Ok(val) = remaining.to_str().unwrap_or("").parse::<u32>()
        {
            rate_limit_remaining = Some(val);
            if val < 100 {
                crate::log_emotes!(debug, "Rate limit remaining: {} (page {})", val, page_num);
            }
        }

        let status = resp.status();
        crate::log_emotes!(trace, "User emotes response status: {}", status);

        if status.as_u16() == 429 {
            // Page 1 fails fast: the caller keeps the previous set.
            if cursor.is_none() {
                return Err(format!("Twitch API rate limited ({})", status));
            }
            consecutive_errors += 1;
            let wait = server_retry_wait(resp.headers()).unwrap_or(Duration::from_secs(5));
            let within_budget = wait <= USER_EMOTES_RATE_LIMIT_MAX_WAIT
                && fetch_started.elapsed() + wait <= USER_EMOTES_RATE_LIMIT_BUDGET;
            if consecutive_errors < 3 && within_budget {
                crate::log_emotes!(
                    warn,
                    "Rate limited on page {}, waiting {:?} before retry",
                    page_num,
                    wait
                );
                tokio::time::sleep(wait).await;
                page_num -= 1;
                continue;
            }
            crate::log_emotes!(
                warn,
                "Rate limited on page {} (wait {:?}), continuing with {} emotes",
                page_num,
                wait,
                all_raw_emotes.len()
            );
            complete = false;
            break;
        }

        if !status.is_success() {
            let body = crate::backend::http::error_body(resp).await;
            crate::log_emotes!(warn, "Failed to fetch user emotes: {} - {}", status, body);
            if cursor.is_none() {
                return Err(format!("Twitch API error: {} - {}", status, body));
            }
            complete = false;
            break;
        }

        consecutive_errors = 0;

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                crate::log_emotes!(warn, "Failed to read response body: {}", e);
                if cursor.is_none() {
                    return Err(e.to_string());
                }
                complete = false;
                break;
            }
        };

        if cursor.is_none() {
            crate::log_emotes!(
                trace,
                "Raw response sample: {}",
                truncate_for_log(&body, 800)
            );
        }

        #[derive(Deserialize)]
        struct PaginatedResponse {
            data: Vec<TwitchUserEmote>,
            #[serde(default)]
            template: String,
            #[serde(default)]
            pagination: Pagination,
        }
        #[derive(Deserialize, Default)]
        struct Pagination {
            cursor: Option<String>,
        }

        let response: PaginatedResponse = serde_json::from_str(&body).map_err(|e| {
            crate::log_emotes!(warn, "Failed to parse user emotes response: {}", e);
            crate::log_emotes!(debug, "Response body: {}", truncate_for_log(&body, 500));
            e.to_string()
        })?;

        crate::log_emotes!(
            trace,
            "Received {} user emotes in this page",
            response.data.len()
        );

        if cursor.is_none() && !response.data.is_empty() {
            let first = &response.data[0];
            crate::log_emotes!(
                trace,
                "First emote parsed: id={}, name={}, owner_id='{}'",
                first.id,
                first.name,
                first.owner_id
            );
        }

        if template.is_empty() && !response.template.is_empty() {
            template = response.template;
        }

        all_raw_emotes.extend(response.data);

        cursor = response.pagination.cursor;
        if cursor.is_none() {
            break;
        }
    }

    let emote_types: HashSet<&str> = all_raw_emotes
        .iter()
        .map(|e| e.emote_type.as_str())
        .collect();
    crate::log_emotes!(
        info,
        "User emotes API returned {} emotes from {} pages with types: {:?}",
        all_raw_emotes.len(),
        page_num,
        emote_types
    );

    let owner_ids: HashSet<String> = all_raw_emotes
        .iter()
        .filter(|e| {
            let is_channel_emote = matches!(
                e.emote_type.as_str(),
                "subscriptions" | "follower" | "bitstier"
            );
            is_channel_emote
                && !e.owner_id.is_empty()
                && e.owner_id != "0"
                && e.owner_id.chars().all(|c| c.is_ascii_digit())
        })
        .map(|e| e.owner_id.clone())
        .collect();

    let channel_emote_count = all_raw_emotes
        .iter()
        .filter(|e| {
            matches!(
                e.emote_type.as_str(),
                "subscriptions" | "follower" | "bitstier"
            )
        })
        .count();

    crate::log_emotes!(
        info,
        "Found {} channel-based emotes (sub/follower/bits) out of {} total, {} unique channel IDs to resolve",
        channel_emote_count,
        all_raw_emotes.len(),
        owner_ids.len()
    );

    if owner_ids.is_empty() && channel_emote_count > 0 {
        let sample: Vec<_> = all_raw_emotes
            .iter()
            .filter(|e| {
                matches!(
                    e.emote_type.as_str(),
                    "subscriptions" | "follower" | "bitstier"
                )
            })
            .take(3)
            .collect();
        for e in sample {
            crate::log_emotes!(
                debug,
                "Sample channel emote: name={}, owner_id='{}', emote_type={}",
                e.name,
                e.owner_id,
                e.emote_type
            );
        }
    }

    let owner_names = resolve_user_ids_to_names(&owner_ids, token).await;

    let page_template = if template.is_empty() {
        TWITCH_EMOTE_CDN_TEMPLATE
    } else {
        &template
    };

    let mut all_emotes = Vec::new();
    for e in all_raw_emotes {
        let is_animated = e.format.iter().any(|format| format == "animated");
        let url_1x = twitch_emote_variant_url(page_template, &e.id, is_animated, "1.0");
        let url_2x = twitch_emote_variant_url(page_template, &e.id, is_animated, "2.0");
        let url_4x = twitch_emote_variant_url(page_template, &e.id, is_animated, "3.0");

        let channel_value = match e.emote_type.as_str() {
            "subscriptions" | "follower" | "bitstier" => {
                let channel_name = if e.owner_id.is_empty() {
                    crate::log_emotes!(
                        debug,
                        "Keeping emote '{}' with missing Twitch owner id",
                        e.name
                    );
                    "unresolved".to_string()
                } else if e.owner_id.chars().all(|c| c.is_ascii_digit()) {
                    owner_names.get(&e.owner_id).cloned().unwrap_or_else(|| {
                        crate::log_emotes!(
                            debug,
                            "Keeping emote '{}' with unresolved Twitch owner id '{}'",
                            e.name,
                            e.owner_id
                        );
                        format!("unresolved:{}", e.owner_id)
                    })
                } else {
                    e.owner_id.clone()
                };

                format!("sub:{}", channel_name)
            }
            "prime" => "twitch:prime".to_string(),
            "limitedtime" => "twitch:limited".to_string(),
            "hypetrain" => "twitch:hypetrain".to_string(),
            "rewards" => "twitch:rewards".to_string(),
            "turbo" => "twitch:turbo".to_string(),
            _ => continue,
        };

        all_emotes.push(Emote {
            id: Arc::from(e.id.clone()),
            name: Arc::from(e.name),
            url: Arc::from(url_1x.clone()),
            provider: EmoteProvider::Twitch,
            channel: Some(Arc::from(channel_value)),
            original_name: None,
            creator_name: None,
            twitch_owner_login: match e.emote_type.as_str() {
                "subscriptions" | "follower" | "bitstier" => {
                    if e.owner_id.chars().all(|c| c.is_ascii_digit()) {
                        owner_names.get(&e.owner_id).cloned().map(Arc::from)
                    } else if e.owner_id.is_empty() {
                        None
                    } else {
                        Some(Arc::from(e.owner_id.clone()))
                    }
                }
                _ => None,
            },
            twitch_owner_id: if e.owner_id.is_empty() {
                None
            } else {
                Some(Arc::from(e.owner_id.clone()))
            },
            twitch_emote_type: if e.emote_type.is_empty() {
                None
            } else {
                Some(Arc::from(e.emote_type.clone()))
            },
            url_1x: Some(Arc::from(url_1x)),
            url_2x: Some(Arc::from(url_2x)),
            url_4x: Some(Arc::from(url_4x)),
            zero_width: false,
            is_animated,
            width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
            height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
        });
    }

    crate::log_emotes!(
        info,
        "Total user (subscription) emotes after filtering: {}",
        all_emotes.len()
    );
    Ok((all_emotes, complete))
}

/// Resolve Twitch user IDs to display names
async fn resolve_user_ids_to_names(ids: &HashSet<String>, token: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();

    if ids.is_empty() {
        return result;
    }

    let ids_vec: Vec<&String> = ids.iter().collect();
    for chunk in ids_vec.chunks(100) {
        let id_params: Vec<String> = chunk.iter().map(|id| format!("id={}", id)).collect();
        let url = api::twitch::users(&id_params);

        crate::log_emotes!(
            trace,
            "Resolving {} user IDs, URL: {}",
            chunk.len(),
            truncate_for_log(&url, 200)
        );

        let resp = retry_helix_get(
            &EMOTE_HTTP_CLIENT,
            &url,
            CLIENT_ID,
            token,
            EMOTE_MAX_RETRY_AFTER,
        )
        .await;

        match resp {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    #[derive(Deserialize)]
                    struct UsersResponse {
                        data: Vec<UserInfo>,
                    }
                    #[derive(Deserialize)]
                    struct UserInfo {
                        id: String,
                        display_name: String,
                    }

                    match resp.json::<UsersResponse>().await {
                        Ok(users_resp) => {
                            crate::log_emotes!(
                                info,
                                "Got {} users from API",
                                users_resp.data.len()
                            );
                            for user in users_resp.data {
                                result.insert(user.id, user.display_name);
                            }
                        }
                        Err(e) => {
                            crate::log_emotes!(warn, "Failed to parse users response: {}", e);
                        }
                    }
                } else {
                    let body = crate::backend::http::error_body(resp).await;
                    crate::log_emotes!(
                        warn,
                        "Users API failed: {} - {}",
                        status,
                        truncate_for_log(&body, 200)
                    );
                }
            }
            Err(e) => {
                crate::log_emotes!(warn, "Failed to fetch users: {}", e);
            }
        }
    }

    crate::log_emotes!(info, "Resolved {} user IDs to names", result.len());
    result
}

/// Fetch Twitch global emotes using Helix API
pub async fn fetch_twitch_global_emotes(token: Option<&str>) -> Result<Vec<Emote>, String> {
    let Some(token) = token else {
        return Ok(vec![]);
    };

    let resp = retry_helix_get_str(
        &EMOTE_HTTP_CLIENT,
        api::twitch::GLOBAL_EMOTES,
        CLIENT_ID,
        token,
        EMOTE_MAX_RETRY_AFTER,
    )
    .await?;

    if !resp.status().is_success() {
        return Err(format!(
            "Helix global emotes returned status: {}",
            resp.status()
        ));
    }

    let response: TwitchEmotesResponse = resp.json().await.map_err(|e| e.to_string())?;

    let emotes: Vec<Emote> = response
        .data
        .into_iter()
        .map(|e| {
            let is_animated = e.format.iter().any(|f| f == "animated");
            let (url_1x, url_2x, url_4x) = if is_animated {
                (
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "1.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "2.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "3.0"),
                )
            } else {
                (e.images.url_1x, e.images.url_2x, e.images.url_4x)
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.name),
                url: Arc::from(url_1x.clone()),
                provider: EmoteProvider::Twitch,
                channel: None,
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: if e.emote_type.is_empty() {
                    None
                } else {
                    Some(Arc::from(e.emote_type))
                },
                url_1x: Some(Arc::from(url_1x)),
                url_2x: Some(Arc::from(url_2x)),
                url_4x: Some(Arc::from(url_4x)),
                zero_width: false,
                is_animated,
                width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
                height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
            }
        })
        .collect();

    Ok(emotes)
}

/// Emotes indexed by channel for fast iteration (channel_login -> Vec<Arc<Emote>>)
pub static EMOTE_BY_CHANNEL: LazyLock<RwLock<HashMap<String, Vec<Arc<Emote>>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static CHANNEL_EMOTE_CATALOG_VERSIONS: LazyLock<DashMap<String, u32>> = LazyLock::new(DashMap::new);
static PROVIDER_VISIBILITY_GENERATION: AtomicU64 = AtomicU64::new(0);
static PROVIDER_VISIBILITY_APPLY_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Stable per-channel derived-index publication slot.
///
/// The registry below is only copied when a channel receives its first slot.
/// Every subsequent replacement (including an empty removal tombstone) swaps
/// this slot's shared inner lookup in place. Readers never touch the writer
/// epoch or commit lock.
pub struct ChannelEmotePublication {
    epoch: AtomicU64,
    tombstoned: AtomicBool,
    commit_lock: Mutex<()>,
    lookup: ArcSwap<EmoteNameMap>,
}

impl ChannelEmotePublication {
    fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            tombstoned: AtomicBool::new(false),
            commit_lock: Mutex::new(()),
            lookup: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    #[inline]
    fn begin_rebuild(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }

    #[inline]
    fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    #[inline]
    pub(crate) fn lookup_snapshot(&self) -> Arc<EmoteNameMap> {
        self.lookup.load_full()
    }
}

/// Mark a channel publication active at the explicit connection boundary.
pub fn activate_channel_emotes(channel: &str) {
    let cache_key = normalize_channel_key(channel);
    let publication = channel_emote_publication(&cache_key);
    let _commit = publication.commit_lock.lock_or_recover();
    publication.invalidate();
    publication.tombstoned.store(false, Ordering::Release);
}

/// Lock-free channel lookup registry. Entries intentionally remain after a
/// removal as empty tombstone slots so an already-running rebuild cannot
/// recreate a closed channel's derived indices. The outer map is cloned only
/// when a channel is registered for the first time; all ordinary publications
/// update the slot's inner [`ArcSwap`] instead.
pub static EMOTE_LOOKUP_MAPS: LazyLock<ArcSwap<HashMap<String, Arc<ChannelEmotePublication>>>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

#[inline]
fn channel_emote_publication(cache_key: &str) -> Arc<ChannelEmotePublication> {
    if let Some(publication) = EMOTE_LOOKUP_MAPS.load().get(cache_key) {
        return Arc::clone(publication);
    }

    let candidate = Arc::new(ChannelEmotePublication::new());
    EMOTE_LOOKUP_MAPS.rcu(|publications| {
        if publications.contains_key(cache_key) {
            return Arc::clone(publications);
        }
        let mut updated = (**publications).clone();
        updated.insert(cache_key.to_string(), Arc::clone(&candidate));
        Arc::new(updated)
    });

    EMOTE_LOOKUP_MAPS
        .load()
        .get(cache_key)
        .cloned()
        .expect("channel publication slot registered")
}

#[inline]
fn channel_emote_publication_if_registered(
    cache_key: &str,
) -> Option<Arc<ChannelEmotePublication>> {
    EMOTE_LOOKUP_MAPS.load().get(cache_key).cloned()
}

/// Drop a closed channel's renderer vector and replace its lookup with an empty
/// tombstone. Safe to call even if the channel is reopened later — the stable
/// slot receives a newer epoch and publishes normally on the next provider load.
pub fn remove_channel_emotes(channel: &str) {
    let cache_key = normalize_channel_key(channel);
    FAILED_CHANNELS.remove(&cache_key);
    if CHANNEL_CATALOG_PENDING.remove(&cache_key).is_some() {
        EMOTE_CACHE.channels.remove(&cache_key);
        EMOTE_CACHE.channel_emote_set_ids.remove(&cache_key);
    } else {
        retain_closed_channel_catalog(&cache_key);
    }
    let publication = channel_emote_publication(&cache_key);
    publication.invalidate();
    let _commit = publication.commit_lock.lock_or_recover();
    publication.tombstoned.store(true, Ordering::Release);
    EMOTE_BY_CHANNEL.write_or_recover().remove(&cache_key);

    publication.lookup.store(Arc::new(HashMap::new()));
}

const MAX_CLOSED_CHANNEL_CATALOGS: usize = 32;

static CLOSED_CHANNEL_CATALOGS: LazyLock<Mutex<std::collections::VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(std::collections::VecDeque::new()));

fn retain_closed_channel_catalog(cache_key: &str) {
    let evicted: Vec<String> = {
        let mut closed = CLOSED_CHANNEL_CATALOGS.lock_or_recover();
        closed.retain(|key| key != cache_key && crate::backend::irc::is_channel_closed(key));
        closed.push_back(cache_key.to_string());
        let excess = closed.len().saturating_sub(MAX_CLOSED_CHANNEL_CATALOGS);
        closed.drain(..excess).collect()
    };
    for key in evicted {
        if crate::backend::irc::is_channel_closed(&key) {
            EMOTE_CACHE.channels.remove(&key);
            EMOTE_CACHE.channel_emote_set_ids.remove(&key);
        }
    }
}

fn trigger_open_channel_preload(cache_key: &str) {
    if !crate::backend::irc::is_channel_closed(cache_key) {
        crate::cache::loader::trigger_channel_preload(cache_key);
    }
}

/// True when the channel has a non-empty fast-lookup entry, i.e. tokenization can
/// currently resolve its emotes. Used by the recovery paths (staggered-load retry,
/// open watchdog) to decide whether a lookup gap still needs healing.
pub fn channel_has_lookup_entry(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    channel_emote_publication_if_registered(&cache_key)
        .is_some_and(|publication| !publication.lookup_snapshot().is_empty())
}

/// Whether a channel has a catalog safe to use for one-time history parsing.
/// A non-empty partial FFZ/BTTV lookup is insufficient while a cold channel's
/// deferred 7TV fetch remains pending; an authoritative empty cache entry is
/// safe and prevents waiting forever for a channel with no provider emotes.
pub fn channel_catalog_ready_for_history(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    let Some(entry) = EMOTE_CACHE.channels.get(&cache_key) else {
        return false;
    };
    if CHANNEL_CATALOG_PENDING
        .get(&cache_key)
        .is_some_and(|token| token.load(Ordering::Acquire))
    {
        return false;
    }
    entry.emotes_by_id.len() == 0 || channel_has_lookup_entry(&cache_key)
}

/// True while a cold channel's initial provider publication is incomplete.
/// History uses this to await the existing owner instead of restarting a
/// partial catalog load after its short retry backdate.
pub fn channel_initial_catalog_pending(channel: &str) -> bool {
    let cache_key = normalize_channel_key(channel);
    CHANNEL_CATALOG_PENDING
        .get(&cache_key)
        .is_some_and(|token| token.load(Ordering::Acquire))
}

/// One-shot open watchdog: roughly [`CHANNEL_OPEN_EMOTE_WATCHDOG_SECS`] after a
/// channel open/load is kicked off, if the channel still has no lookup entry,
/// self-heal. Rebuilds the index from cached metadata when it is resident (the
/// common reopen case, where the entry was pruned on close) and only re-triggers
/// a full network load when the cache is empty too. Runs on the durable runtime.
pub fn spawn_channel_open_emote_watchdog(channel: &str) {
    let channel_key = normalize_channel_key(channel);
    if channel_key.is_empty()
        || channel_key == "whisper"
        || crate::benchmark_ui_fixture::is_fixture_channel(&channel_key)
    {
        return;
    }
    crate::backend::bridge::get_runtime().spawn(async move {
        tokio::time::sleep(Duration::from_secs(CHANNEL_OPEN_EMOTE_WATCHDOG_SECS)).await;
        if channel_has_lookup_entry(&channel_key) {
            return;
        }
        crate::log_emotes!(
            warn,
            "[HEAL] {} has no emote lookup map {}s after open; self-healing",
            channel_key,
            CHANNEL_OPEN_EMOTE_WATCHDOG_SECS
        );
        if EMOTE_CACHE.channel_emote_count(&channel_key) > 0 {
            rebuild_emote_indices(&channel_key).await;
        } else {
            crate::backend::bridge::preload_channel_emotes_predictive(&channel_key);
        }
    });
}

/// Global emotes list for easy iteration
pub static GLOBAL_EMOTES_LIST: LazyLock<RwLock<Vec<Arc<Emote>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Fast lookup map for global emotes (name -> Arc<Emote>)
pub static GLOBAL_EMOTES_LOOKUP: LazyLock<ArcSwap<EmoteNameMap>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Fast lookup map for user emotes (personal 7TV, subscriptions) (name -> Arc<Emote>)
pub static USER_EMOTES_LOOKUP: LazyLock<ArcSwap<EmoteNameMap>> =
    LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Normalize channel login/key for emote lookups.
/// Accepts values like "#ChannelName" and returns lowercase login form.
pub use echo_emote_core::{normalize_channel_key, normalized_channel_key};

#[inline]
pub fn emote_provider_enabled(provider: EmoteProvider) -> bool {
    use crate::backend::emote_provider_visibility::{
        EmoteProviderVisibility, EmoteProviderVisibilityKey, emote_provider_visible,
    };

    let key = match provider {
        EmoteProvider::Twitch => EmoteProviderVisibilityKey::Twitch,
        EmoteProvider::SevenTV => EmoteProviderVisibilityKey::SevenTv,
        EmoteProvider::BTTV => EmoteProviderVisibilityKey::Bttv,
        EmoteProvider::FFZ => EmoteProviderVisibilityKey::Ffz,
        EmoteProvider::Emoji => EmoteProviderVisibilityKey::Emoji,
    };
    if matches!(
        key,
        EmoteProviderVisibilityKey::Twitch | EmoteProviderVisibilityKey::Emoji
    ) {
        return emote_provider_visible(key, EmoteProviderVisibility::default());
    }

    emote_provider_visible(
        key,
        EmoteProviderVisibility {
            seventv: crate::backend::settings::get_enable_seventv_emotes(),
            bttv: crate::backend::settings::get_enable_bttv_emotes(),
            ffz: crate::backend::settings::get_enable_ffz_emotes(),
        },
    )
}

#[inline]
fn emote_visible(emote: &Emote) -> bool {
    emote_provider_enabled(emote.provider)
}

/// Prune tracking DashMaps that accumulate entries over time.
/// Called periodically from the refresh scheduler.
pub fn prune_tracking_maps() {
    let Some(cutoff) = echo_core::instant_ago(Duration::from_secs(60 * 60)) else {
        return;
    };

    let mut pruned = 0usize;

    SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.retain(|_, v| {
        let keep = *v > cutoff;
        if !keep {
            pruned += 1;
        }
        keep
    });
    {
        let mut global_timeout = SEVENTV_CHANNEL_SYNC_TIMEOUT_GLOBAL_LAST_AT.lock_or_recover();
        if global_timeout.is_some_and(|last_timeout| last_timeout <= cutoff) {
            *global_timeout = None;
            pruned += 1;
        }
    }

    CHANNEL_MISS_REFRESH_LAST_AT.retain(|_, v| {
        let keep = *v > cutoff;
        if !keep {
            pruned += 1;
        }
        keep
    });

    CHANNEL_MISS_REFRESH_TOKEN_SEEN.retain(|_, v| {
        let keep = *v > cutoff;
        if !keep {
            pruned += 1;
        }
        keep
    });

    personal::prune();

    if pruned > 0 {
        crate::log_emotes!(debug, "Pruned {} stale tracking map entries", pruned);
    }
}

/// Best-effort self-healing refresh trigger when a token looked like an emote but missed.
/// Keeps 7TV EventAPI as primary source of truth, and only acts as fallback when updates
/// are delayed or missed. Enabled by default (bounded by a 5-minute per-channel cooldown,
/// a per-token hourly gate, and the mixed-case candidate heuristic); disable with
/// `ECHO_EMOTE_MISS_REFRESH=0`.
pub fn schedule_channel_refresh_on_unresolved(channel: &str, token: &str) {
    if !*CHANNEL_MISS_REFRESH_ENABLED {
        return;
    }

    let channel_key = normalize_channel_key(channel);
    if channel_key.is_empty()
        || channel_key == "whisper"
        || crate::benchmark_ui_fixture::is_fixture_channel(&channel_key)
    {
        return;
    }
    if is_channel_pending(&channel_key) {
        return;
    }

    let Some(token) = unresolved_emote_refresh_candidate(token) else {
        return;
    };

    let now = Instant::now();
    let seen_key = (channel_key.clone(), token.to_string());
    if let Some(seen) = CHANNEL_MISS_REFRESH_TOKEN_SEEN.get(&seen_key)
        && now.duration_since(*seen.value()) < Duration::from_secs(60 * 60)
    {
        return;
    }
    if let Some(last) = CHANNEL_MISS_REFRESH_LAST_AT.get(&channel_key)
        && now.duration_since(*last.value())
            < Duration::from_secs(CHANNEL_MISS_REFRESH_COOLDOWN_SECS)
    {
        return;
    }
    CHANNEL_MISS_REFRESH_TOKEN_SEEN.insert(seen_key, now);
    CHANNEL_MISS_REFRESH_LAST_AT.insert(channel_key.clone(), now);

    crate::log_emotes!(
        debug,
        "[HEAL] unresolved token '{}' in {} -> scheduling refresh",
        token,
        channel_key
    );

    crate::backend::bridge::get_runtime().spawn(async move {
        let channel_id =
            if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(&channel_key) {
                Some(id)
            } else if let Some(token) = crate::backend::settings::get_token() {
                match crate::backend::twitch_api::get_user_id_helix(&channel_key, &token).await {
                    Ok(id) => {
                        crate::backend::twitch_api::cache_channel_id(&channel_key, &id);
                        Some(id)
                    }
                    Err(e) => {
                        crate::log_emotes!(
                            debug,
                            "[HEAL] failed to resolve channel id for {}: {}",
                            channel_key,
                            e
                        );
                        None
                    }
                }
            } else {
                None
            };

        let Some(channel_id) = channel_id else {
            return;
        };

        if let Err(e) = EMOTE_CACHE
            .refresh_channel_emotes(&channel_id, &channel_key)
            .await
        {
            crate::log_emotes!(
                debug,
                "[HEAL] refresh failed for {} after unresolved token: {}",
                channel_key,
                e
            );
        }
    });
}

fn unresolved_emote_refresh_candidate(token: &str) -> Option<&str> {
    fn is_affix_char(c: char) -> bool {
        matches!(
            c,
            '.' | ','
                | '!'
                | '?'
                | ':'
                | ';'
                | '"'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
        )
    }

    let token = token.trim_matches(is_affix_char);
    if token.len() < 3 || token.len() > 48 {
        return None;
    }
    if token.starts_with('@') || token.contains("://") {
        return None;
    }
    if !token
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }

    let has_separator = token.bytes().any(|b| b == b'_');
    let has_lower = token.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = token.bytes().any(|b| b.is_ascii_uppercase());

    if has_lower && has_upper && (has_camel_hump(token) || has_separator) {
        return Some(token);
    }

    None
}

fn has_camel_hump(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.iter().enumerate().any(|(idx, byte)| {
        idx > 0
            && idx + 1 < bytes.len()
            && byte.is_ascii_uppercase()
            && bytes[..idx].iter().any(u8::is_ascii_lowercase)
            && bytes[idx + 1..].iter().any(u8::is_ascii_lowercase)
    })
}

/// Update the dual-index caches from EmoteCache
pub async fn rebuild_emote_indices(channel_login: &str) {
    rebuild_emote_indices_inner(channel_login).await;
}

/// Publish a channel snapshot while holding the channel writer lock.
///
/// This is the uncommon race fallback: building the authoritative snapshot
/// inside the serialized writer section guarantees that a stale incremental or
/// full rebuild cannot leave a cache mutation absent from the published index.
fn publish_authoritative_channel_snapshot(cache_key: &str) -> usize {
    publish_authoritative_channel_snapshot_with_hook(cache_key, |_| {})
}

fn publish_authoritative_channel_snapshot_with_hook(
    cache_key: &str,
    after_begin: impl FnOnce(&Arc<ChannelEmotePublication>),
) -> usize {
    let publication = channel_emote_publication(cache_key);
    let _commit = publication.commit_lock.lock_or_recover();
    publication.begin_rebuild();
    after_begin(&publication);
    if publication.tombstoned.load(Ordering::Acquire) {
        EMOTE_BY_CHANNEL.write_or_recover().remove(cache_key);
        publication.lookup.store(Arc::new(HashMap::new()));
        clear_channel_pending(cache_key);
        publication.invalidate();
        return 0;
    }
    let (emotes, lookup) = EMOTE_CACHE.get_channel_index_snapshot(cache_key);
    let emote_name_count = lookup.len();
    EMOTE_BY_CHANNEL
        .write_or_recover()
        .insert(cache_key.to_string(), emotes);
    publication.lookup.store(Arc::new(lookup));
    clear_channel_pending(cache_key);
    publication.invalidate();
    emote_name_count
}

async fn publish_channel_provider_diff(channel_login: &str, diff: ProviderIndexDiff) {
    if [
        EmoteProvider::Twitch,
        EmoteProvider::SevenTV,
        EmoteProvider::BTTV,
        EmoteProvider::FFZ,
        EmoteProvider::Emoji,
    ]
    .into_iter()
    .any(|provider| !emote_provider_enabled(provider))
    {
        rebuild_emote_indices(channel_login).await;
        return;
    }

    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();
    let mut lookup = (*publication.lookup_snapshot()).clone();
    let provider_emotes = if let Some(entry) = EMOTE_CACHE.channels.get(&cache_key) {
        for alias in &diff.changed_aliases {
            if let Some(emote) = entry.emotes.get(alias.as_ref()) {
                lookup.insert(Arc::clone(alias), Arc::clone(emote));
            } else {
                lookup.remove(alias.as_ref());
            }
        }
        entry
            .emotes_by_id
            .0
            .get(&diff.provider)
            .into_iter()
            .flat_map(|map| map.values().flatten())
            .cloned()
            .collect::<Vec<_>>()
    } else {
        rebuild_emote_indices(channel_login).await;
        return;
    };

    let published = {
        let _commit = publication.commit_lock.lock_or_recover();
        if publication.epoch.load(Ordering::Acquire) != publication_epoch
            || publication.tombstoned.load(Ordering::Acquire)
        {
            false
        } else {
            let mut by_channel = EMOTE_BY_CHANNEL.write_or_recover();
            let emotes = by_channel.entry(cache_key.clone()).or_default();
            emotes.retain(|emote| emote.provider != diff.provider);
            emotes.extend(provider_emotes);
            publication.lookup.store(Arc::new(lookup));
            clear_channel_pending(&cache_key);
            true
        }
    };

    if !published {
        let emote_name_count = publish_authoritative_channel_snapshot(&cache_key);
        crate::log_emotes!(
            debug,
            "Rebuilt stale incremental channel emote publication for {} (epoch {}, names={})",
            cache_key,
            publication_epoch,
            emote_name_count
        );
    }

    increment_channel_catalog_version(&cache_key);
}

async fn rebuild_emote_indices_inner(channel_login: &str) {
    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();

    let (emotes, lookup) = EMOTE_CACHE.get_channel_index_snapshot(channel_login);
    let emote_name_count = lookup.len();

    let audit_channel = std::env::var("ECHO_AUDIT_CHANNEL").ok();
    let audit_tokens = std::env::var("ECHO_AUDIT_EMOTES").ok();
    if audit_channel
        .as_deref()
        .map(normalize_channel_key)
        .as_deref()
        == Some(cache_key.as_str())
        && let Some(tokens_csv) = audit_tokens
    {
        for token in tokens_csv
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            match lookup.get(token) {
                Some(emote) => crate::log_emotes!(
                    info,
                    "[AUDIT] channel={} token='{}' -> HIT provider={:?} id={} url={}",
                    cache_key,
                    token,
                    emote.provider,
                    emote.id,
                    emote.url
                ),
                None => crate::log_emotes!(
                    info,
                    "[AUDIT] channel={} token='{}' -> MISS",
                    cache_key,
                    token
                ),
            }
        }
    }

    let published = {
        let _commit = publication.commit_lock.lock_or_recover();
        if publication.epoch.load(Ordering::Acquire) != publication_epoch
            || publication.tombstoned.load(Ordering::Acquire)
        {
            crate::log_emotes!(
                debug,
                "Discarding stale channel emote index publication for {} (epoch {})",
                cache_key,
                publication_epoch
            );
            false
        } else {
            EMOTE_BY_CHANNEL
                .write_or_recover()
                .insert(cache_key.clone(), emotes);

            publication.lookup.store(Arc::new(lookup));

            clear_channel_pending(&cache_key);
            true
        }
    };

    if !published {
        let recovered_name_count = publish_authoritative_channel_snapshot(&cache_key);
        crate::log_emotes!(
            debug,
            "Recovered stale channel emote index publication for {} (epoch {}, names={})",
            cache_key,
            publication_epoch,
            recovered_name_count
        );
    }

    crate::log_emotes!(
        info,
        "rebuild_emote_indices for {}: {} emotes loaded, preserving resident rows",
        cache_key,
        emote_name_count
    );
    increment_channel_catalog_version(&cache_key);
}

fn emote_lookup_changed(previous: &EmoteNameMap, replacement: &EmoteNameMap) -> bool {
    previous.len() != replacement.len()
        || previous.iter().any(|(name, previous_emote)| {
            replacement
                .get(name.as_ref())
                .is_none_or(|replacement_emote| !Arc::ptr_eq(previous_emote, replacement_emote))
        })
}

/// Update global emote indices
pub async fn rebuild_global_emote_index() {
    let emotes = EMOTE_CACHE.get_global_emotes().await;

    let mut lookup: EmoteNameMap = HashMap::with_capacity(emotes.len() * 2);
    for emote in &emotes {
        EmoteCache::insert_emote_name_keys(&mut lookup, Arc::clone(emote));
    }

    let emote_count = lookup.len();

    crate::log_emotes!(
        info,
        "Global emote index rebuilt: {} emotes registered",
        emote_count
    );

    let previous_lookup = GLOBAL_EMOTES_LOOKUP.load_full();
    let replacement_lookup = Arc::new(lookup);
    let effective_lookup_changed = emote_lookup_changed(&previous_lookup, &replacement_lookup);
    *GLOBAL_EMOTES_LIST.write_or_recover() = emotes;
    GLOBAL_EMOTES_LOOKUP.store(replacement_lookup);

    if effective_lookup_changed {
        crate::state::VersionRegistry::bump_global_emote_version();
        publish_emote_metadata_change();
    }
}

/// Update user emote indices (personal 7TV, subscriptions)
pub async fn rebuild_user_emote_index() {
    let emotes = EMOTE_CACHE.get_user_emotes().await;

    let mut lookup: EmoteNameMap = HashMap::with_capacity(emotes.len() * 2);
    for emote in &emotes {
        EmoteCache::insert_emote_name_keys(&mut lookup, Arc::clone(emote));
    }

    let previous_lookup = USER_EMOTES_LOOKUP.load_full();
    let replacement_lookup = Arc::new(lookup);
    let effective_lookup_changed = emote_lookup_changed(&previous_lookup, &replacement_lookup);
    USER_EMOTES_LOOKUP.store(replacement_lookup);

    if effective_lookup_changed {
        crate::state::VersionRegistry::bump_user_emote_version();
        publish_emote_metadata_change();
    }
}

/// The provider catalogs used to tokenize one message. Capturing these once
/// keeps token resolution internally consistent and avoids an additional
/// registry/slot atomic load for every word in a chat message.
pub struct EmoteLookupSnapshot {
    /// The sender's 7TV personal emotes, for other chatters' messages.
    personal: Option<Arc<EmoteNameMap>>,
    channel: Option<Arc<EmoteNameMap>>,
    /// The signed-in account's personal and subscription emotes, for its own text.
    user: Option<Arc<EmoteNameMap>>,
    global: Arc<EmoteNameMap>,
}

/// Author of the text being tokenized. A sender's 7TV personal emotes apply
/// only to that sender's messages, and the signed-in account's own personal
/// and subscription emotes only to its own messages.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmoteSender<'a> {
    pub user_id: Option<&'a str>,
    pub login: Option<&'a str>,
}

impl<'a> EmoteSender<'a> {
    /// Text with no chatter author, such as system rows.
    pub const NONE: Self = Self {
        user_id: None,
        login: None,
    };

    pub fn new(user_id: Option<&'a str>, login: &'a str) -> Self {
        Self {
            user_id: user_id.filter(|id| !id.is_empty()),
            login: Some(login).filter(|login| !login.is_empty()),
        }
    }

    fn is_signed_in_account(&self) -> bool {
        self.login
            .is_some_and(crate::backend::settings::is_signed_in_username)
            || self
                .user_id
                .is_some_and(crate::backend::auth::is_active_authenticated_user_id)
    }
}

/// Capture the lookup catalogs for composer resolution, which is always the
/// signed-in account's own text. The channel key is normalized here so the
/// snapshot preserves the defensive behavior of the public lookup wrappers.
pub fn snapshot_emote_lookups(channel: &str) -> EmoteLookupSnapshot {
    let mut snapshot = snapshot_emote_lookups_for_sender(channel, EmoteSender::NONE);
    snapshot.user = Some(USER_EMOTES_LOOKUP.load_full());
    snapshot
}

/// Capture the lookup catalogs for one message written by `sender`.
pub fn snapshot_emote_lookups_for_sender(
    channel: &str,
    sender: EmoteSender<'_>,
) -> EmoteLookupSnapshot {
    let channel_key = normalize_channel_key(channel);
    let channel = channel_emote_publication_if_registered(&channel_key)
        .map(|publication| publication.lookup_snapshot());
    let (personal, user) = if sender.user_id.is_none() && sender.login.is_none() {
        (None, None)
    } else if sender.is_signed_in_account() {
        (None, Some(USER_EMOTES_LOOKUP.load_full()))
    } else {
        (sender.user_id.and_then(personal::lookup_for_user), None)
    };
    EmoteLookupSnapshot {
        personal,
        channel,
        user,
        global: GLOBAL_EMOTES_LOOKUP.load_full(),
    }
}

/// Get emote by name from a previously captured lookup snapshot, permitting a
/// case-folded fallback for non-tokenizer callers.
pub fn get_emote_fast_lowercased_from_snapshot(
    snapshot: &EmoteLookupSnapshot,
    name: &str,
) -> Option<Arc<Emote>> {
    let folded_name = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(name.to_ascii_lowercase())
    } else {
        None
    };

    if let Some(lookup) = snapshot.channel.as_deref() {
        if let Some(emote) = lookup.get(name)
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
        if let Some(ref folded) = folded_name
            && let Some(emote) = lookup.get(folded.as_str())
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
    }

    if let Some(user) = snapshot.user.as_deref() {
        if let Some(emote) = user.get(name)
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
        if let Some(ref folded) = folded_name
            && let Some(emote) = user.get(folded.as_str())
            && emote_visible(emote)
        {
            return Some(Arc::clone(emote));
        }
    }

    if let Some(emote) = snapshot.global.get(name)
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }
    if let Some(ref folded) = folded_name
        && let Some(emote) = snapshot.global.get(folded.as_str())
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// PERF: Get emote by name with a channel key. This compatibility wrapper
/// captures a snapshot for the one lookup; message tokenizers should pass one
/// snapshot to the `_from_snapshot` variants instead.
pub fn get_emote_fast_lowercased(channel_key_lowercase: &str, name: &str) -> Option<Arc<Emote>> {
    get_emote_fast_lowercased_from_snapshot(&snapshot_emote_lookups(channel_key_lowercase), name)
}

/// Get emote by exact case-sensitive name with pre-normalized channel key.
///
/// Unlike `get_emote_fast_lowercased`, this never falls back to case-folded
/// lookups. Chat tokenization should use this path to preserve emote
/// case-sensitivity semantics.
pub fn get_emote_fast_case_sensitive_from_snapshot(
    snapshot: &EmoteLookupSnapshot,
    name: &str,
) -> Option<Arc<Emote>> {
    let exact = |lookup: &EmoteNameMap| {
        lookup
            .get(name)
            .filter(|emote| emote.name.as_ref() == name && emote_visible(emote))
            .cloned()
    };
    if let Some(emote) = snapshot.personal.as_deref().and_then(exact) {
        return Some(emote);
    }
    if let Some(emote) = snapshot.channel.as_deref().and_then(exact) {
        return Some(emote);
    }
    if let Some(emote) = snapshot.user.as_deref().and_then(exact) {
        return Some(emote);
    }

    if let Some(emote) = snapshot.global.get(name)
        && emote.name.as_ref() == name
        && emote_visible(emote)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Exact case-sensitive compatibility wrapper for one-off consumers.
pub fn get_emote_fast_case_sensitive(
    channel_key_lowercase: &str,
    name: &str,
) -> Option<Arc<Emote>> {
    get_emote_fast_case_sensitive_from_snapshot(
        &snapshot_emote_lookups(channel_key_lowercase),
        name,
    )
}

/// Get a Twitch emote by exact Twitch emote ID using cache ID indices.
///
/// Lookup order mirrors name-based resolution precedence:
/// channel -> user emotes -> global.
pub fn get_twitch_emote_by_id_fast(
    channel_key_lowercase: &str,
    twitch_emote_id: &str,
) -> Option<Arc<Emote>> {
    if let Some(entry) = EMOTE_CACHE.channels.get(channel_key_lowercase)
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    let needs_normalize = channel_key_lowercase
        .bytes()
        .any(|b| b == b'#' || b.is_ascii_uppercase());
    if needs_normalize {
        let normalized = normalize_channel_key(channel_key_lowercase);
        if normalized != channel_key_lowercase
            && let Some(entry) = EMOTE_CACHE.channels.get(&normalized)
            && let Some(emote) = entry
                .emotes_by_id
                .get(twitch_emote_id, EmoteProvider::Twitch)
        {
            return Some(Arc::clone(emote));
        }
    }

    let user = EMOTE_CACHE.user_emotes.read_or_recover();
    if let Some(entry) = user.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }
    drop(user);

    let global = EMOTE_CACHE.global.read_or_recover();
    if let Some(entry) = global.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Get a Twitch emote by exact ID from any cached Helix-backed source (channel/user/global).
///
/// This is useful for tooltip enrichment when the render path only has a reduced emote instance
/// but we want to recover exact owner/type metadata from the same cache data the emote picker uses.
pub fn get_twitch_emote_by_id_any_cached(twitch_emote_id: &str) -> Option<Arc<Emote>> {
    let user = EMOTE_CACHE.user_emotes.read_or_recover();
    if let Some(entry) = user.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }
    drop(user);

    for entry in EMOTE_CACHE.channels.iter() {
        if let Some(emote) = entry
            .value()
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
        {
            return Some(Arc::clone(emote));
        }
    }

    let global = EMOTE_CACHE.global.read_or_recover();
    if let Some(entry) = global.as_ref()
        && let Some(emote) = entry
            .emotes_by_id
            .get(twitch_emote_id, EmoteProvider::Twitch)
    {
        return Some(Arc::clone(emote));
    }

    None
}

/// Get all emotes for a channel from dual-index (Arc references for O(1) cloning)
pub fn get_channel_emotes_fast(channel_login: &str) -> Vec<Arc<Emote>> {
    let cache_key = normalize_channel_key(channel_login);
    EMOTE_BY_CHANNEL
        .read_or_recover()
        .get(&cache_key)
        .map(|emotes| {
            emotes
                .iter()
                .filter(|emote| emote_visible(emote))
                .map(Arc::clone)
                .collect()
        })
        .unwrap_or_default()
}

#[inline]
fn publish_emote_metadata_change() {
    mark_emote_cache_dirty();
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::ProviderMetadata,
    );
    crate::managers::frame_rate_controller::record_activity();
}

/// Increment version for a specific channel's emotes.
/// Delegates to VersionRegistry for centralized version management.
fn increment_channel_version(channel_login: &str) {
    crate::state::VersionRegistry::bump_channel_emote_version(channel_login);
    increment_channel_catalog_version(channel_login);
}

fn increment_channel_catalog_version(channel_login: &str) {
    mark_emote_cache_dirty();
    let key = normalize_channel_key(channel_login);
    CHANNEL_EMOTE_CATALOG_VERSIONS
        .entry(key)
        .and_modify(|version| *version = version.wrapping_add(1))
        .or_insert(1);
    publish_emote_metadata_change();
}

/// Rebuild visibility-filtered catalogs and then reparse resident rows as one
/// latest-generation transaction. Normal provider publication never reparses
/// resident rows; this explicit user setting is the intentional exception.
pub fn schedule_provider_visibility_refresh() {
    let generation = begin_provider_visibility_generation();
    crate::backend::bridge::RUNTIME.spawn(async move {
        let _apply = PROVIDER_VISIBILITY_APPLY_LOCK.lock().await;
        let is_current = || provider_visibility_generation_is_current(generation);
        if !is_current() {
            return;
        }

        rebuild_global_emote_index().await;
        if !is_current() {
            return;
        }
        rebuild_user_emote_index().await;
        if !is_current() {
            return;
        }
        let mut channels = crate::backend::irc::active_emote_lookup_channels();
        channels.extend(crate::backend::irc::get_desired_channels());
        for channel in channels {
            rebuild_emote_indices(&channel).await;
            if !is_current() {
                return;
            }
        }

        if let Err(error) = tokio::task::spawn_blocking(move || {
            crate::backend::irc::retokenize_all_channels_if_current(|| {
                provider_visibility_generation_is_current(generation)
            });
        })
        .await
        {
            crate::log_emotes!(warn, "Provider visibility reparse task failed: {error}");
        }
    });
}

fn begin_provider_visibility_generation() -> u64 {
    PROVIDER_VISIBILITY_GENERATION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
}

fn provider_visibility_generation_is_current(generation: u64) -> bool {
    PROVIDER_VISIBILITY_GENERATION.load(Ordering::Acquire) == generation
}

/// Get the current cache version for a channel.
/// Delegates to VersionRegistry for centralized version management.
/// Note: Returns u32 for backward compatibility; internally uses u64.
pub fn get_emote_cache_version(channel_login: &str) -> u32 {
    let key = normalize_channel_key(channel_login);
    CHANNEL_EMOTE_CATALOG_VERSIONS
        .get(&key)
        .map(|version| *version)
        .unwrap_or(0)
}

/// Get the global emote cache version.
/// Delegates to VersionRegistry for centralized version management.
/// Note: Returns u32 for backward compatibility; internally uses u64.
pub fn get_global_emote_version() -> u32 {
    crate::state::VersionRegistry::global_emote_version() as u32
}

/// Install a tiny process-local emote set for the opt-in native completion
/// interaction harness. Synthetic fixture channels never persist this cache.
pub(crate) fn seed_completion_emotes_for_benchmark(channel_login: &str) -> bool {
    if !crate::benchmark_ui_fixture::is_fixture_channel(channel_login) {
        return false;
    }

    let cache_key = normalize_channel_key(channel_login);
    let channel: Arc<str> = Arc::from(channel_login);
    let mut emotes = HashMap::new();
    let mut emotes_by_id = EmoteIdentityMap::default();
    for (index, name) in crate::benchmark_ui_fixture::COMPLETION_EMOTES
        .iter()
        .enumerate()
    {
        let id: Arc<str> = Arc::from(format!("echo-fixture-completion-{index}"));
        let url: Arc<str> = Arc::from(format!(
            "https://static-cdn.jtvnw.net/emoticons/v2/{}/default/dark/2.0",
            25 + index
        ));
        EmoteCache::insert_emote_into_maps(
            &mut emotes,
            &mut emotes_by_id,
            Emote {
                id,
                name: Arc::from(*name),
                url: Arc::clone(&url),
                provider: EmoteProvider::Twitch,
                channel: Some(Arc::clone(&channel)),
                original_name: None,
                creator_name: None,
                twitch_owner_login: Some(Arc::clone(&channel)),
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: Some(Arc::clone(&url)),
                url_2x: Some(url),
                url_4x: None,
                zero_width: false,
                is_animated: false,
                width: Some(28),
                height: Some(28),
            },
        );
    }

    let lookup = Arc::new(emotes.clone());
    let publication = channel_emote_publication(&cache_key);
    let publication_epoch = publication.begin_rebuild();
    EMOTE_CACHE.channels.insert(
        cache_key.clone(),
        CacheEntry {
            emotes,
            emotes_by_id,
            fetched_at: Instant::now(),
        },
    );
    let _commit = publication.commit_lock.lock_or_recover();
    if publication.epoch.load(Ordering::Acquire) != publication_epoch {
        return false;
    }
    publication.lookup.store(lookup);
    increment_channel_version(channel_login);
    true
}

pub(crate) fn clear_completion_emotes_for_benchmark(channel_login: &str) -> bool {
    if !crate::benchmark_ui_fixture::is_enabled() {
        return false;
    }
    let cache_key = normalize_channel_key(channel_login);
    let publication = channel_emote_publication(&cache_key);
    publication.invalidate();
    let _commit = publication.commit_lock.lock_or_recover();
    let removed = EMOTE_CACHE.channels.remove(&cache_key).is_some();
    publication.lookup.store(Arc::new(HashMap::new()));
    if removed {
        increment_channel_version(channel_login);
    }
    removed
}

pub use echo_emote_core::EmoteCategory;

pub fn group_emotes_into_categories(
    emotes: Vec<Arc<Emote>>,
    channel_login: &str,
) -> Vec<EmoteCategory> {
    let started_at = Instant::now();
    let input_count = emotes.len();
    let categories = echo_emote_core::group_emotes_into_categories(emotes, channel_login);
    record_emote_stage_timed("emotes.categorize.group", started_at, input_count);
    categories
}

/// Get categorized emotes for a channel (combines cached channel + global emotes)
pub async fn get_categorized_emotes(channel_login: &str) -> Vec<EmoteCategory> {
    let total_start = Instant::now();
    let unique_fetch_start = Instant::now();
    let emotes = EMOTE_CACHE.get_all_emotes_unique(channel_login).await;
    record_emote_stage_timed(
        "emotes.categorize.fetch_unique",
        unique_fetch_start,
        emotes.len(),
    );
    crate::log_emotes!(
        info,
        "get_categorized_emotes for '{}': found {} total emotes",
        channel_login,
        emotes.len()
    );
    let group_start = Instant::now();
    let categories = group_emotes_into_categories(emotes, channel_login);
    record_emote_stage_timed("emotes.categorize.build", group_start, categories.len());
    crate::log_emotes!(
        info,
        "get_categorized_emotes for '{}': grouped into {} categories",
        channel_login,
        categories.len()
    );
    record_emote_stage_timed("emotes.categorize.total", total_start, categories.len());
    categories
}

/// Measure the actual production full and incremental provider-index paths.
#[cfg(feature = "provider-index-bench")]
pub fn benchmark_provider_index_refresh(iterations: usize) -> ProviderIndexBenchResult {
    fn fixture_emote(provider: EmoteProvider, index: usize, name: Arc<str>) -> Arc<Emote> {
        let id: Arc<str> = Arc::from(format!("{provider:?}-{index}"));
        Arc::new(Emote {
            url: Arc::from(format!("https://example.invalid/{id}")),
            id,
            name,
            provider,
            channel: None,
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: None,
            url_2x: None,
            url_4x: None,
            zero_width: false,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    fn median(mut values: Vec<u64>) -> u64 {
        values.sort_unstable();
        values[values.len() / 2]
    }

    let providers = [
        EmoteProvider::Twitch,
        EmoteProvider::FFZ,
        EmoteProvider::BTTV,
        EmoteProvider::SevenTV,
    ];
    let initial = providers.into_iter().flat_map(|provider| {
        (0..1025).map(move |index| {
            let name = if index == 12 {
                Arc::from("Collision")
            } else {
                Arc::from(format!("{provider:?}_{index}"))
            };
            fixture_emote(provider, index, name)
        })
    });
    let emotes_by_id = initial.collect::<EmoteIdentityMap>();
    let mut initial_entry = CacheEntry {
        emotes: HashMap::new(),
        emotes_by_id,
        fetched_at: Instant::now(),
    };
    EmoteCache::rebuild_name_map_from_identities(&mut initial_entry);
    let replacement = (0..1025)
        .filter(|index| *index != 40)
        .map(|index| {
            let name: Arc<str> = match index {
                12 => Arc::from("RenamedCollision"),
                41 => Arc::from("HUH"),
                _ => Arc::from(format!("SevenTV_{index}")),
            };
            fixture_emote(EmoteProvider::SevenTV, index, name)
        })
        .chain([fixture_emote(
            EmoteProvider::SevenTV,
            1025,
            Arc::from("huh"),
        )])
        .collect::<Vec<_>>();

    let iterations = iterations.max(1);
    let mut full_samples = Vec::with_capacity(iterations);
    let mut incremental_samples = Vec::with_capacity(iterations);
    let mut equivalent = true;
    for _ in 0..iterations {
        let mut full = initial_entry.clone();
        let full_started = Instant::now();
        let previous_provider = full
            .emotes_by_id
            .0
            .remove(&EmoteProvider::SevenTV)
            .unwrap_or_default();
        for emote in &replacement {
            let emote = find_emote_alias(&previous_provider, &emote.id, &emote.name)
                .filter(|previous| previous.as_ref() == emote.as_ref())
                .cloned()
                .unwrap_or_else(|| Arc::clone(emote));
            full.emotes_by_id.insert(emote);
        }
        EmoteCache::rebuild_name_map_from_identities(&mut full);
        full_samples.push(full_started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);

        let mut incremental = initial_entry.clone();
        let incremental_started = Instant::now();
        let incremental_diff = EmoteCache::replace_provider_rows_incrementally(
            &mut incremental,
            EmoteProvider::SevenTV,
            replacement.iter().cloned(),
        )
        .expect("benchmark fixture must remain inside the incremental threshold");
        incremental_samples.push(
            incremental_started
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );

        equivalent &= incremental.emotes == full.emotes
            && incremental_diff.provider == EmoteProvider::SevenTV;
    }

    ProviderIndexBenchResult {
        entries: initial_entry.emotes_by_id.len(),
        iterations,
        equivalent,
        full_median_ns: median(full_samples),
        incremental_median_ns: median(incremental_samples),
    }
}

#[cfg(test)]
mod provider_index_tests {
    use super::*;
    use core::prelude::v1::test;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ChannelTestCleanup(String);

    impl Drop for ChannelTestCleanup {
        fn drop(&mut self) {
            crate::backend::irc::CHAT_MESSAGES.remove(&self.0);
            EMOTE_CACHE.channels.remove(&self.0);
            CHANNEL_EMOTE_CATALOG_VERSIONS.remove(&self.0);
            remove_channel_emotes(&self.0);
        }
    }

    fn emote(
        provider: EmoteProvider,
        id: impl Into<Arc<str>>,
        name: impl Into<Arc<str>>,
    ) -> Arc<Emote> {
        let id = id.into();
        Arc::new(Emote {
            url: Arc::from(format!("https://example.invalid/{id}")),
            id,
            name: name.into(),
            provider,
            channel: None,
            original_name: None,
            creator_name: None,
            twitch_owner_login: None,
            twitch_owner_id: None,
            twitch_emote_type: None,
            url_1x: None,
            url_2x: None,
            url_4x: None,
            zero_width: false,
            is_animated: false,
            width: Some(28),
            height: Some(28),
        })
    }

    fn entry(emotes: impl IntoIterator<Item = Arc<Emote>>) -> CacheEntry {
        let emotes_by_id = emotes.into_iter().collect::<EmoteIdentityMap>();
        let mut entry = CacheEntry {
            emotes: HashMap::new(),
            emotes_by_id,
            fetched_at: Instant::now(),
        };
        EmoteCache::rebuild_name_map_from_identities(&mut entry);
        entry
    }

    #[test]
    fn seventv_aliases_of_one_emote_id_all_resolve() {
        let lookup_up = emote(EmoteProvider::SevenTV, "01KGH222", "lookUp");
        let test2 = emote(EmoteProvider::SevenTV, "01KGH222", "test2");
        let mut entry = entry([Arc::clone(&lookup_up), Arc::clone(&test2)]);
        assert_eq!(entry.emotes_by_id.len(), 2);
        assert!(entry.emotes.contains_key("lookUp"));
        assert!(entry.emotes.contains_key("test2"));

        let removed = EmoteCache::remove_seventv_emote(
            &mut entry,
            &SevenTvEmoteIdentity {
                id: Some("01KGH222".to_string()),
                name: Some("test2".to_string()),
            },
            true,
        )
        .expect("the named alias is removed");
        assert_eq!(removed.name.as_ref(), "test2");
        EmoteCache::rebuild_name_map_from_identities(&mut entry);
        assert!(entry.emotes.contains_key("lookUp"));
        assert!(!entry.emotes.contains_key("test2"));
    }

    #[test]
    fn seventv_add_of_a_case_variant_keeps_the_other_emote() {
        let lower = emote(EmoteProvider::SevenTV, "id-a", "huh");
        let mut entry = entry([Arc::clone(&lower)]);
        let upper = emote(EmoteProvider::SevenTV, "id-b", "HUH");
        let outcome = EmoteCache::apply_seventv_patch_to_entry(
            &mut entry,
            SevenTvEmoteSetPatch {
                added: vec![upper.as_ref().clone()],
                ..SevenTvEmoteSetPatch::default()
            },
            &mut Vec::new(),
        );
        assert_eq!(outcome.changed, 1);
        assert_eq!(entry.emotes_by_id.len(), 2);
        assert_eq!(entry.emotes.get("huh").map(|e| e.id.as_ref()), Some("id-a"));
        assert_eq!(entry.emotes.get("HUH").map(|e| e.id.as_ref()), Some("id-b"));
    }

    #[test]
    fn removing_one_case_alias_keeps_the_folded_key_for_its_sibling() {
        let upper = emote(EmoteProvider::SevenTV, "id-a", "HUH");
        let mixed = emote(EmoteProvider::SevenTV, "id-a", "Huh");
        let mut entry = entry([Arc::clone(&upper), Arc::clone(&mixed)]);
        let holder = entry.emotes.get("huh").map(|e| Arc::clone(&e.name));
        let removed_name = holder.expect("folded key is present");
        EmoteCache::remove_seventv_emote(
            &mut entry,
            &SevenTvEmoteIdentity {
                id: Some("id-a".to_string()),
                name: Some(removed_name.to_string()),
            },
            true,
        )
        .expect("alias removed");
        let heir = entry
            .emotes
            .get("huh")
            .expect("sibling keeps the folded key");
        assert_ne!(heir.name, removed_name);
    }

    fn lookup_signature(map: &EmoteNameMap) -> HashMap<String, (EmoteProvider, String)> {
        map.iter()
            .map(|(name, emote)| (name.to_string(), (emote.provider, emote.id.to_string())))
            .collect()
    }

    #[test]
    fn provider_timeout_cooldown_applies_across_channels_and_expires() {
        let first_channel = format!("provider-timeout-first-{}", std::process::id());
        let second_channel = format!("provider-timeout-second-{}", std::process::id());
        let now = Instant::now();
        record_seventv_sync_timeout(first_channel.clone(), now);

        assert!(seventv_sync_cooldown_remaining(&second_channel, now).is_some());
        assert!(
            seventv_sync_cooldown_remaining(
                &second_channel,
                now + Duration::from_secs(SEVENTV_CHANNEL_SYNC_GLOBAL_COOLDOWN_SECS + 1),
            )
            .is_none()
        );

        SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&first_channel);
        *SEVENTV_CHANNEL_SYNC_TIMEOUT_GLOBAL_LAST_AT.lock_or_recover() = None;
    }

    #[test]
    fn provider_sync_probe_is_claimed_once_per_process() {
        SEVENTV_CHANNEL_SYNC_PROBE_CLAIMED.store(false, Ordering::Release);
        assert!(claim_seventv_sync_probe());
        assert!(!claim_seventv_sync_probe());
        SEVENTV_CHANNEL_SYNC_PROBE_CLAIMED.store(false, Ordering::Release);
    }

    #[test]
    fn provider_visibility_generation_rejects_stale_transactions() {
        let first = begin_provider_visibility_generation();
        assert!(provider_visibility_generation_is_current(first));
        let second = begin_provider_visibility_generation();
        assert!(!provider_visibility_generation_is_current(first));
        assert!(provider_visibility_generation_is_current(second));
    }

    #[test]
    fn full_rebuild_preserves_provider_and_exact_name_winners() {
        let mut entry = entry([
            emote(EmoteProvider::SevenTV, "7-upper", "HUH"),
            emote(EmoteProvider::BTTV, "b-lower", "huh"),
            emote(EmoteProvider::FFZ, "f-collision", "SameName"),
            emote(EmoteProvider::SevenTV, "7-collision", "SameName"),
        ]);

        EmoteCache::rebuild_name_map_from_identities(&mut entry);

        assert_eq!(entry.emotes["huh"].id.as_ref(), "b-lower");
        assert_eq!(entry.emotes["HUH"].id.as_ref(), "7-upper");
        assert_eq!(entry.emotes["SameName"].id.as_ref(), "7-collision");
        assert_eq!(entry.emotes["samename"].id.as_ref(), "7-collision");
    }

    #[tokio::test]
    async fn live_catalog_add_and_removal_affect_only_later_ingestion() {
        let channel = format!(
            "session-emote-semantics-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos()
        );
        let _cleanup = ChannelTestCleanup(channel.clone());

        EMOTE_CACHE
            .channels
            .insert(channel.clone(), entry(std::iter::empty::<Arc<Emote>>()));
        activate_channel_emotes(&channel);
        assert_eq!(publish_authoritative_channel_snapshot(&channel), 0);

        let resident_slots = crate::backend::slots::parse_to_slots(
            "LOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        assert!(matches!(
            resident_slots.as_slice(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "LOL"
        ));
        let mut buffer = echo_core::message_buffer::MessageBuffer::with_default_capacity();
        buffer.push(crate::backend::models::ChatMessage {
            id: Arc::from("resident-before-add"),
            channel: Arc::from(channel.as_str()),
            content: Arc::from("LOL"),
            structure_hash: crate::backend::slots::compute_structure_hash(&resident_slots),
            slots: resident_slots.into(),
            ..Default::default()
        });
        crate::backend::irc::CHAT_MESSAGES.insert(channel.clone(), buffer);
        let layout_version_before = crate::state::VersionRegistry::channel_emote_version(&channel);
        let catalog_version_before = get_emote_cache_version(&channel);

        apply_7tv_channel_emote_patch(
            &channel,
            SevenTvEmoteSetPatch {
                added: vec![
                    emote(EmoteProvider::SevenTV, "lol-live", "LOL")
                        .as_ref()
                        .clone(),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("live add should update the channel catalog");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            crate::state::VersionRegistry::channel_emote_version(&channel),
            layout_version_before,
            "live catalog mutation must not stale resident message layouts"
        );
        assert!(get_emote_cache_version(&channel) > catalog_version_before);

        let resident_after_add = crate::backend::irc::CHAT_MESSAGES
            .get(&channel)
            .and_then(|messages| messages.get(0).cloned())
            .expect("resident row should remain buffered");
        assert!(matches!(
            resident_after_add.slots.as_ref(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "LOL"
        ));

        let later_slots = crate::backend::slots::parse_to_slots(
            "LOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        let later_emote = match later_slots.as_slice() {
            [crate::backend::slots::MessageSlot::Emote(emote)] => Arc::clone(emote),
            slots => panic!("later ingestion should resolve LOL as an emote, got {slots:?}"),
        };
        crate::backend::irc::CHAT_MESSAGES
            .get_mut(&channel)
            .expect("channel buffer should remain installed")
            .push(crate::backend::models::ChatMessage {
                id: Arc::from("resident-after-add"),
                channel: Arc::from(channel.as_str()),
                content: Arc::from("LOL"),
                structure_hash: crate::backend::slots::compute_structure_hash(&later_slots),
                slots: later_slots.into(),
                ..Default::default()
            });

        apply_7tv_channel_emote_patch(
            &channel,
            SevenTvEmoteSetPatch {
                removed: vec![SevenTvEmoteIdentity {
                    id: Some("lol-live".to_string()),
                    name: Some("LOL".to_string()),
                }],
                ..Default::default()
            },
        )
        .await
        .expect("live removal should update the channel catalog");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let messages = crate::backend::irc::CHAT_MESSAGES
            .get(&channel)
            .expect("session rows should remain buffered");
        assert!(matches!(
            messages.get(0).expect("pre-add row").slots.as_ref(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "LOL"
        ));
        assert!(matches!(
            messages.get(1).expect("post-add row").slots.as_ref(),
            [crate::backend::slots::MessageSlot::Emote(emote)]
                if Arc::ptr_eq(emote, &later_emote)
                    && emote.provider == EmoteProvider::SevenTV
                    && emote.id.as_ref() == "lol-live"
        ));
        drop(messages);

        let after_removal = crate::backend::slots::parse_to_slots(
            "LOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        assert!(matches!(
            after_removal.as_slice(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "LOL"
        ));
    }

    #[tokio::test]
    async fn exact_set_reconcile_and_clear_preserve_resident_bindings() {
        let channel = format!(
            "session-exact-set-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should follow the Unix epoch")
                .as_nanos()
        );
        let _cleanup = ChannelTestCleanup(channel.clone());
        EMOTE_CACHE
            .channels
            .insert(channel.clone(), entry(std::iter::empty::<Arc<Emote>>()));
        activate_channel_emotes(&channel);
        publish_authoritative_channel_snapshot(&channel);

        let before = crate::backend::slots::parse_to_slots(
            "SETLOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        apply_7tv_channel_emote_rows(
            &channel,
            vec![emote(EmoteProvider::SevenTV, "set-lol", "SETLOL")],
            "set-session",
        )
        .await;
        assert!(matches!(
            before.as_slice(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "SETLOL"
        ));

        let during = crate::backend::slots::parse_to_slots(
            "SETLOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        let retained = match during.as_slice() {
            [crate::backend::slots::MessageSlot::Emote(emote)] => Arc::clone(emote),
            slots => panic!("exact set should affect later ingestion, got {slots:?}"),
        };

        assert_eq!(
            clear_7tv_channel_emote_rows_if_set(&channel, "set-session").await,
            Some(1)
        );
        assert_eq!(retained.id.as_ref(), "set-lol");
        assert!(matches!(
            during.as_slice(),
            [crate::backend::slots::MessageSlot::Emote(emote)]
                if Arc::ptr_eq(emote, &retained)
        ));
        let after = crate::backend::slots::parse_to_slots(
            "SETLOL",
            &channel,
            EmoteSender::NONE,
            &rustc_hash::FxHashMap::default(),
        );
        assert!(matches!(
            after.as_slice(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "SETLOL"
        ));
    }

    #[test]
    fn small_provider_diff_matches_full_rebuild_lookup() {
        let providers = [
            EmoteProvider::Twitch,
            EmoteProvider::FFZ,
            EmoteProvider::BTTV,
            EmoteProvider::SevenTV,
        ];
        let initial = providers.into_iter().flat_map(|provider| {
            (0..1025).map(move |index| {
                let name = if index == 12 {
                    Arc::from("Collision")
                } else {
                    Arc::from(format!("{:?}_{index}", provider))
                };
                emote(provider, format!("{:?}-{index}", provider), name)
            })
        });
        let mut incremental = entry(initial.clone());
        let mut full = entry(initial);
        let replacement = (0..1025)
            .filter(|index| *index != 40)
            .map(|index| {
                let name: Arc<str> = match index {
                    12 => Arc::from("RenamedCollision"),
                    41 => Arc::from("HUH"),
                    _ => Arc::from(format!("SevenTV_{index}")),
                };
                emote(EmoteProvider::SevenTV, format!("SevenTV-{index}"), name)
            })
            .chain([emote(EmoteProvider::SevenTV, "SevenTV-new", "huh")])
            .collect::<Vec<_>>();

        let diff = EmoteCache::replace_provider_rows_incrementally(
            &mut incremental,
            EmoteProvider::SevenTV,
            replacement.clone(),
        )
        .expect("three changed identities use the incremental path");

        let previous_provider = full
            .emotes_by_id
            .0
            .remove(&EmoteProvider::SevenTV)
            .unwrap_or_default();
        for emote in replacement {
            let emote = find_emote_alias(&previous_provider, &emote.id, &emote.name)
                .filter(|previous| previous.as_ref() == emote.as_ref())
                .cloned()
                .unwrap_or(emote);
            full.emotes_by_id.insert(emote);
        }
        EmoteCache::rebuild_name_map_from_identities(&mut full);

        assert_eq!(
            lookup_signature(&incremental.emotes),
            lookup_signature(&full.emotes)
        );
        assert_eq!(diff.provider, EmoteProvider::SevenTV);
        assert_eq!(
            incremental.emotes["Collision"].provider,
            EmoteProvider::BTTV
        );
        assert_eq!(incremental.emotes["huh"].id.as_ref(), "SevenTV-new");
    }

    #[test]
    fn stale_publication_fallback_rebuilds_all_authoritative_providers() {
        let channel = format!("provider-race-{}", std::process::id());
        EMOTE_CACHE.channels.insert(
            channel.clone(),
            entry([
                emote(EmoteProvider::BTTV, "bttv-one", "SharedName"),
                emote(EmoteProvider::SevenTV, "7tv-one", "SevenOnly"),
            ]),
        );
        let publication = channel_emote_publication(&channel);
        publication.lookup.store(Arc::new(HashMap::new()));
        publication.begin_rebuild();
        publication.begin_rebuild();

        let count = publish_authoritative_channel_snapshot(&channel);
        let lookup = publication.lookup_snapshot();
        assert_eq!(count, lookup.len());
        assert_eq!(lookup["SharedName"].id.as_ref(), "bttv-one");
        assert_eq!(lookup["SevenOnly"].id.as_ref(), "7tv-one");

        EMOTE_CACHE.channels.remove(&channel);
        remove_channel_emotes(&channel);
    }

    #[test]
    fn fallback_invalidates_builder_started_inside_writer_window() {
        let channel = format!("provider-barrier-race-{}", std::process::id());
        EMOTE_CACHE.channels.insert(
            channel.clone(),
            entry([
                emote(EmoteProvider::BTTV, "bttv-barrier", "BarrierBttv"),
                emote(EmoteProvider::SevenTV, "7tv-barrier", "BarrierSeven"),
            ]),
        );
        let publication = channel_emote_publication(&channel);
        publication.lookup.store(Arc::new(HashMap::new()));
        let concurrent_epoch = AtomicU64::new(0);

        publish_authoritative_channel_snapshot_with_hook(&channel, |publication| {
            assert!(publication.lookup_snapshot().is_empty());
            concurrent_epoch.store(publication.begin_rebuild(), Ordering::Release);
        });

        assert_ne!(
            publication.epoch.load(Ordering::Acquire),
            concurrent_epoch.load(Ordering::Acquire),
            "the waiting builder must be stale after authoritative publication"
        );
        let lookup = publication.lookup_snapshot();
        assert_eq!(lookup["BarrierBttv"].id.as_ref(), "bttv-barrier");
        assert_eq!(lookup["BarrierSeven"].id.as_ref(), "7tv-barrier");

        EMOTE_CACHE.channels.remove(&channel);
        remove_channel_emotes(&channel);
    }

    #[test]
    fn stale_fallback_cannot_resurrect_closed_channel_tombstone() {
        let channel = format!("provider-close-race-{}", std::process::id());
        EMOTE_CACHE.channels.insert(
            channel.clone(),
            entry([emote(
                EmoteProvider::SevenTV,
                "closed-resident",
                "ClosedResident",
            )]),
        );
        activate_channel_emotes(&channel);
        let initial_count = publish_authoritative_channel_snapshot(&channel);
        let publication = channel_emote_publication(&channel);
        assert_eq!(initial_count, publication.lookup_snapshot().len());
        publication.begin_rebuild();

        remove_channel_emotes(&channel);
        assert!(publication.lookup_snapshot().is_empty());
        assert!(!EMOTE_BY_CHANNEL.read_or_recover().contains_key(&channel));

        assert_eq!(publish_authoritative_channel_snapshot(&channel), 0);
        assert!(publication.lookup_snapshot().is_empty());
        assert!(!EMOTE_BY_CHANNEL.read_or_recover().contains_key(&channel));

        activate_channel_emotes(&channel);
        let reopened_count = publish_authoritative_channel_snapshot(&channel);
        assert_eq!(reopened_count, publication.lookup_snapshot().len());
        assert_eq!(
            publication.lookup_snapshot()["ClosedResident"].id.as_ref(),
            "closed-resident"
        );

        EMOTE_CACHE.channels.remove(&channel);
        remove_channel_emotes(&channel);
    }

    #[test]
    fn manual_reload_bypasses_the_eventapi_window_fetch_cache() {
        let channel = "manual-reload-cache-bypass";
        let cache_key = format!("123:{channel}");
        SEVENTV_CHANNEL_FETCH_CACHE.insert(
            cache_key.clone(),
            SevenTVChannelFetchCacheEntry {
                fetched_at: Instant::now(),
                result: Arc::new((
                    SevenTvExactSetFetchOutcome::Present {
                        emotes: vec![emote(EmoteProvider::SevenTV, "stale", "Stale")],
                        emote_set_id: Some("set-id".to_string()),
                    },
                    None,
                )),
            },
        );
        let live_window = Duration::from_secs(SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS);
        assert!(clone_cached_7tv_channel(&cache_key, live_window).is_some());

        invalidate_7tv_channel_fetch_cache("#Manual-Reload-Cache-Bypass");

        assert!(clone_cached_7tv_channel(&cache_key, live_window).is_none());
    }

    #[test]
    fn manual_reload_status_reports_each_provider_and_7tv_failures() {
        assert_eq!(
            format_manual_reload_status(Ok(Some(12)), 3, 1),
            "Channel emotes reloaded (7TV 12, BetterTTV 3, FrankerFaceZ 1)."
        );
        assert_eq!(
            format_manual_reload_status(Ok(None), 0, 0),
            "Channel emotes reloaded (7TV none, BetterTTV 0, FrankerFaceZ 0)."
        );
        assert_eq!(
            format_manual_reload_status(Err("HTTP 503"), 2, 0),
            "Channel emotes reloaded (BetterTTV 2, FrankerFaceZ 0). Failed to fetch 7TV channel emotes: HTTP 503"
        );
    }
}
