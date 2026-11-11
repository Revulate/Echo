//! Emote provider integrations (7TV, BTTV, FFZ, Twitch)

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
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};
use std::num::NonZeroU32;

use echo_core::channel_identity::{channel_key_cow, normalize_channel_key};
pub(crate) use echo_emote_core::PERSONAL_SEVENTV_CHANNEL;
pub use echo_emote_core::{Emote, EmoteCategory, EmoteProvider};

pub mod benchmark_hooks;
pub mod personal;
pub mod refresh;
pub mod seventv_eventapi;

mod bttv;
mod ffz;
mod lookup;
mod publication;
mod seventv;
mod twitch;
use bttv::*;
use ffz::*;
pub use lookup::*;
pub use publication::*;
use seventv::*;
use twitch::*;

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

/// What [`EmoteCache::load_from_disk_scoped`] hydrated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DiskCacheLoad {
    /// The global sets were loaded.
    pub(crate) global: bool,
    /// The signed-in account's user emotes were loaded.
    pub(crate) user: bool,
    /// Number of channel sets loaded.
    pub(crate) channels: usize,
}

/// Run `task` on a named OS thread with its own current-thread runtime.
///
/// `running` is the caller's already-claimed "worker is up" flag; it is
/// cleared when the thread exits for any reason, or right away when the thread
/// or its runtime cannot be created.
fn spawn_runtime_thread<F, Fut>(name: &'static str, running: &'static AtomicBool, task: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    struct RunningGuard(&'static AtomicBool);
    impl Drop for RunningGuard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _guard = RunningGuard(running);
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(task()),
                Err(error) => {
                    crate::log_emotes!(error, "Failed to create the {} runtime: {}", name, error);
                }
            }
        });
    if let Err(error) = spawned {
        running.store(false, Ordering::SeqCst);
        crate::log_emotes!(error, "Failed to spawn the {} thread: {}", name, error);
    }
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
const CHANNEL_MISS_REFRESH_COOLDOWN_SECS: u64 = 300;
/// Refresh delay after a provider failed with no cached emotes to carry over.
const CHANNEL_PROVIDER_FAILURE_RETRY_SECS: u64 = 60;
/// Default-on bounded self-heal: when a chat token looked like an emote but
/// missed the lookup map, schedule a refresh so a stale/pruned set recovers
/// instead of rendering plain text forever. The 5-minute per-channel cooldown,
/// the mixed-case candidate heuristic (`unresolved_emote_refresh_candidate`),
/// and the once-per-hour per-token gate keep this from stampeding providers
/// on ordinary chat words — 7TV EventAPI remains the primary update path.
/// On unless `ECHO_EMOTE_MISS_REFRESH` is set to an off value (for example `0`).
static CHANNEL_MISS_REFRESH_ENABLED: LazyLock<bool> =
    LazyLock::new(|| echo_core::env::flag_or("ECHO_EMOTE_MISS_REFRESH", true));

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

/// Which 7TV fetch a deferred-refresh watchdog event belongs to.
#[derive(Clone, Copy)]
enum DeferredScope {
    Global,
    Channel,
}

/// Lifecycle of a deferred 7TV fetch: scheduled after the synchronous fetch
/// timed out, started, then completed, failed, timed out again, or lost
/// to a task join failure.
#[derive(Clone, Copy)]
enum DeferredEvent {
    Scheduled,
    Started,
    Completed,
    Failed,
    Timeout,
    JoinFailed,
}

impl DeferredScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Channel => "channel",
        }
    }

    fn counters(self) -> &'static [AtomicU64; DeferredEvent::COUNT] {
        static GLOBAL: [AtomicU64; DeferredEvent::COUNT] = [const { AtomicU64::new(0) }; 6];
        static CHANNEL: [AtomicU64; DeferredEvent::COUNT] = [const { AtomicU64::new(0) }; 6];
        match self {
            Self::Global => &GLOBAL,
            Self::Channel => &CHANNEL,
        }
    }
}

impl DeferredEvent {
    const COUNT: usize = 6;

    fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::JoinFailed => "join_failed",
        }
    }

    fn stage(self, scope: DeferredScope) -> &'static str {
        match (scope, self) {
            (DeferredScope::Global, Self::Scheduled) => "emotes.7tv.deferred.global.scheduled",
            (DeferredScope::Global, Self::Started) => "emotes.7tv.deferred.global.started",
            (DeferredScope::Global, Self::Completed) => "emotes.7tv.deferred.global.completed",
            (DeferredScope::Global, Self::Failed) => "emotes.7tv.deferred.global.failed",
            (DeferredScope::Global, Self::Timeout) => "emotes.7tv.deferred.global.timeout",
            (DeferredScope::Global, Self::JoinFailed) => "emotes.7tv.deferred.global.join_failed",
            (DeferredScope::Channel, Self::Scheduled) => "emotes.7tv.deferred.channel.scheduled",
            (DeferredScope::Channel, Self::Started) => "emotes.7tv.deferred.channel.started",
            (DeferredScope::Channel, Self::Completed) => "emotes.7tv.deferred.channel.completed",
            (DeferredScope::Channel, Self::Failed) => "emotes.7tv.deferred.channel.failed",
            (DeferredScope::Channel, Self::Timeout) => "emotes.7tv.deferred.channel.timeout",
            (DeferredScope::Channel, Self::JoinFailed) => "emotes.7tv.deferred.channel.join_failed",
        }
    }
}

fn record_seventv_deferred_watchdog(scope: DeferredScope, event: DeferredEvent) {
    let counters = scope.counters();
    let stage = event.stage(scope);
    let count = counters[event as usize].fetch_add(1, Ordering::Relaxed) + 1;
    crate::backend::metrics::record_emote_event(stage);
    record_emote_stage_duration(stage, Duration::ZERO, 1);

    let load = |event: DeferredEvent| counters[event as usize].load(Ordering::Relaxed);
    crate::log_perf!(
        debug,
        scope = scope.as_str(),
        event = event.as_str(),
        count,
        scheduled = load(DeferredEvent::Scheduled),
        started = load(DeferredEvent::Started),
        completed = load(DeferredEvent::Completed),
        failed = load(DeferredEvent::Failed),
        timed_out = load(DeferredEvent::Timeout),
        join_failed = load(DeferredEvent::JoinFailed),
        "7TV deferred watchdog event"
    );
}

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
    let key = normalize_channel_key(channel_login);
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
    let key = normalize_channel_key(channel_login);
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

    let grace_secs = echo_core::env::parsed("ECHO_CHANNEL_STARTUP_REFRESH_GRACE_SECS")
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

    Some(
        crate::cache::disk_layer::MediaSourceId::with_interned_provider(
            provider,
            Arc::clone(&emote.id),
            variant,
        ),
    )
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
    /// Per-channel emotes, keyed by normalized channel login.
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

impl Default for EmoteCache {
    fn default() -> Self {
        Self::new()
    }
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
            invalidate_seventv_channel_fetch_cache(&cache_key);
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
            invalidate_seventv_global_fetch_cache();
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
    pub fn set_token(&self, token: String) {
        *self.token.write_or_recover() = Some(token);
    }

    /// Clear the OAuth token (used when entering anonymous mode)
    pub fn clear_token(&self) {
        *self.token.write_or_recover() = None;
        crate::log_emotes!(info, "Token cleared");
    }

    /// Set the cache file path (call this when app starts with app_data_dir)
    pub fn set_cache_path(&self, path: PathBuf) {
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
    ) -> DiskCacheLoad {
        let disk_user_request_generation =
            self.user_emote_request_generation.load(Ordering::SeqCst);
        let disk_auth_fence = crate::backend::auth::capture_authenticated_resource_fence();
        let allowed_channels = allowed_channels.map(|channels| {
            channels
                .into_iter()
                .map(|channel| normalize_channel_key(channel.trim()))
                .filter(|channel| !channel.is_empty())
                .collect::<HashSet<_>>()
        });

        let Some(path) = self.cache_path.read_or_recover().as_ref().cloned() else {
            crate::log_emotes!(info, "No cache path set, skipping disk load");
            return DiskCacheLoad::default();
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
            _ => return DiskCacheLoad::default(),
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
            let cache_key = normalize_channel_key(&channel);
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
                crate::cache::loader::trigger_preload(
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

        DiskCacheLoad {
            global: global_loaded,
            user: user_loaded,
            channels: channels_loaded,
        }
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
                            && matches!(e.scope(), echo_emote_core::EmoteScope::Subscription(_))
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
                        fetch_seventv_personal_emotes(user_id),
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
                    let result = fetch_seventv_personal_emotes(user_id).await;
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
                let mut fetch_task = crate::backend::bridge::RUNTIME.spawn(fetch_seventv_global());
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
            record_seventv_deferred_watchdog(DeferredScope::Global, DeferredEvent::Scheduled);
            crate::log_emotes!(
                debug,
                "Scheduling deferred apply from timed-out in-flight 7TV global task"
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                record_seventv_deferred_watchdog(DeferredScope::Global, DeferredEvent::Started);
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
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Completed,
                        );
                    }
                    Ok(Ok(Err(err))) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Failed,
                        );
                        crate::log_emotes!(debug, "Deferred global 7TV fetch failed: {}", err);
                    }
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::JoinFailed,
                        );
                        crate::log_emotes!(debug, "Deferred global 7TV join failed: {}", err);
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        timed_out_fetch.abort();
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Timeout,
                        );
                        crate::log_emotes!(
                            debug,
                            "Deferred global 7TV fetch timed out after {}s",
                            SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS
                        );
                    }
                }
            });
        } else if defer_global_7tv_refresh {
            record_seventv_deferred_watchdog(DeferredScope::Global, DeferredEvent::Scheduled);
            crate::log_emotes!(
                debug,
                "Scheduling deferred retry with new 7TV global fetch task"
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                record_seventv_deferred_watchdog(DeferredScope::Global, DeferredEvent::Started);
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    fetch_seventv_global(),
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
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Completed,
                        );
                    }
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Failed,
                        );
                        crate::log_emotes!(debug, "Deferred global 7TV fetch failed: {}", err);
                    }
                    Err(_) => {
                        record_emote_stage_timed(
                            "emotes.global.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(
                            DeferredScope::Global,
                            DeferredEvent::Timeout,
                        );
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
        let cache_key = normalize_channel_key(channel_login);
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
                let timeout_key = normalize_channel_key(channel_login);
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
                        fetch_seventv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
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
                        fetch_seventv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
                    });
                    return SevenTvChannelFetchState::TimedOut(fetch_task);
                }

                let seven_tv_channel_id = actual_id.clone();
                let seven_tv_channel_login = channel_login.to_string();
                let mut fetch_task = crate::backend::bridge::RUNTIME.spawn(async move {
                    fetch_seventv_channel(&seven_tv_channel_id, &seven_tv_channel_login).await
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
                            .map(|result| result.outcome.emote_count())
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
                    SevenTvChannelFetch {
                        outcome:
                            SevenTvExactSetFetchOutcome::Present {
                                emotes,
                                emote_set_id,
                            },
                        subscription,
                    } => {
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
                    SevenTvChannelFetch {
                        outcome: SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    } => {
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
            let deferred_timeout_key = normalize_channel_key(channel_login);
            let deferred_catalog_pending = catalog_pending.take();
            record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Scheduled);
            crate::log_emotes!(
                debug,
                "Scheduling deferred apply from timed-out in-flight 7TV task for {}",
                deferred_channel_login
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                let _catalog_pending = deferred_catalog_pending;
                record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Started);
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
                    SevenTvChannelFetch {
                        outcome: SevenTvExactSetFetchOutcome::Present {
                            emotes,
                            emote_set_id,
                        },
                        subscription,
                    } => {
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Completed);
                    }
                    SevenTvChannelFetch {
                        outcome: SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    } => {
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Completed);
                    }
                    },
                    Ok(Ok(Err(err))) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Failed);
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::JoinFailed);
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Timeout);
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
            let deferred_timeout_key = normalize_channel_key(channel_login);
            let deferred_actual_id = actual_id.clone();
            let deferred_catalog_pending = catalog_pending.take();
            record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Scheduled);
            crate::log_emotes!(
                debug,
                "Scheduling deferred retry with new 7TV fetch task for {}",
                deferred_channel_login
            );
            crate::backend::bridge::RUNTIME.spawn(async move {
                let _catalog_pending = deferred_catalog_pending;
                record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Started);
                let deferred_fetch_start = Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
                    fetch_seventv_channel(&deferred_actual_id, &deferred_channel_login),
                )
                .await
                {
                    Ok(Ok(shared)) => match shared.as_ref() {
                    SevenTvChannelFetch {
                        outcome: SevenTvExactSetFetchOutcome::Present {
                            emotes,
                            emote_set_id,
                        },
                        subscription,
                    } => {
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Completed);
                    }
                    SevenTvChannelFetch {
                        outcome: SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id },
                        subscription,
                    } => {
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Completed);
                    }
                    },
                    Ok(Err(err)) => {
                        record_emote_stage_timed(
                            "emotes.channel.fetch.7tv.deferred",
                            deferred_fetch_start,
                            0,
                        );
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Failed);
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
                        record_seventv_deferred_watchdog(DeferredScope::Channel, DeferredEvent::Timeout);
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
    pub fn global_needs_refresh(&self) -> bool {
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
    pub fn get_all_emotes(&self, channel_login: &str) -> Vec<Arc<Emote>> {
        let cache_key = channel_key_cow(channel_login);

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
    pub fn get_all_emotes_unique(&self, channel_login: &str) -> Vec<Arc<Emote>> {
        let started_at = Instant::now();
        let cache_key = channel_key_cow(channel_login);
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
    pub fn get_global_emotes(&self) -> Vec<Arc<Emote>> {
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
        let cache_key = normalize_channel_key(channel_login);
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
        let cache_key = channel_key_cow(channel_login);
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
    pub fn get_user_emotes(&self) -> Vec<Arc<Emote>> {
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
    pub fn clear_user_emotes(&self) {
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
    invalidate_seventv_channel_fetch_cache(&cache_key);

    let seventv = match tokio::time::timeout(
        Duration::from_secs(SEVENTV_DEFERRED_FETCH_TIMEOUT_SECS),
        fetch_seventv_channel(channel_id, &cache_key),
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
        && let SevenTvChannelFetch {
            outcome:
                SevenTvExactSetFetchOutcome::Present {
                    emotes,
                    emote_set_id,
                },
            subscription,
        } = shared.as_ref()
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
        Ok(shared) => match &shared.outcome {
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

pub(crate) fn seventv_dispatch_emote(
    value: &serde_json::Value,
    channel: Option<&str>,
) -> Option<Emote> {
    serde_json::from_value::<SevenTvEmote>(value.clone())
        .ok()
        .and_then(|emote| convert_seventv_emote(emote, channel))
}

pub(crate) async fn apply_seventv_channel_emote_patch(
    channel_login: &str,
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE
        .apply_channel_seventv_patch(channel_login, patch)
        .await
}

pub(crate) async fn apply_seventv_global_emote_patch(
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE.apply_global_seventv_patch(patch).await
}

pub(crate) async fn apply_seventv_user_emote_patch(
    patch: SevenTvEmoteSetPatch,
) -> Result<SevenTvPatchApplyOutcome, String> {
    EMOTE_CACHE.apply_user_seventv_patch(patch).await
}

pub(crate) async fn clear_seventv_channel_emote_rows_if_set(
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

pub(crate) async fn apply_seventv_channel_emote_rows(
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

pub(crate) async fn apply_seventv_global_emote_set(emotes: Vec<Arc<Emote>>) -> usize {
    EMOTE_CACHE
        .apply_global_provider_emotes(EmoteProvider::SevenTV, emotes)
        .await
}

pub(crate) async fn clear_seventv_global_emote_set_if_matches(
    expected_set_id: &str,
) -> Option<usize> {
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
    Some(apply_seventv_global_emote_set(Vec::new()).await)
}

pub(crate) async fn apply_seventv_user_emote_set(emotes: Vec<Arc<Emote>>) -> usize {
    EMOTE_CACHE
        .apply_user_provider_emotes(EmoteProvider::SevenTV, emotes)
        .await
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

#[inline]
pub fn emote_provider_enabled(provider: EmoteProvider) -> bool {
    use crate::backend::emote_provider_visibility::{
        EmoteProviderVisibility, emote_provider_visible,
    };

    if matches!(provider, EmoteProvider::Twitch | EmoteProvider::Emoji) {
        return true;
    }
    emote_provider_visible(
        provider,
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
        || crate::bench::ui_fixture::is_fixture_channel(&channel_key)
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
    let emotes = EMOTE_CACHE.get_all_emotes_unique(channel_login);
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

        apply_seventv_channel_emote_patch(
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

        apply_seventv_channel_emote_patch(
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
        apply_seventv_channel_emote_rows(
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
            clear_seventv_channel_emote_rows_if_set(&channel, "set-session").await,
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
            SevenTvChannelFetchCacheEntry {
                fetched_at: Instant::now(),
                result: Arc::new(SevenTvChannelFetch {
                    outcome: SevenTvExactSetFetchOutcome::Present {
                        emotes: vec![emote(EmoteProvider::SevenTV, "stale", "Stale")],
                        emote_set_id: Some("set-id".to_string()),
                    },
                    subscription: None,
                }),
            },
        );
        let live_window = Duration::from_secs(SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS);
        assert!(clone_cached_seventv_channel(&cache_key, live_window).is_some());

        invalidate_seventv_channel_fetch_cache("#Manual-Reload-Cache-Bypass");

        assert!(clone_cached_seventv_channel(&cache_key, live_window).is_none());
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
