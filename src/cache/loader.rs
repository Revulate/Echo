//! Image loading and download orchestration.
//!
//! - Connection pooling with configurable concurrency
//! - Global URL deduplication
//! - Lazy vs eager hydration modes
//! - Disk + memory cache integration

use crate::utils::env_usize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::mapref::entry::Entry as DashEntry;
use dashmap::{DashMap, DashSet};
use echo_cache_policy::loaded_urls::{forget_url, mark_url_loaded, url_is_loaded, url_key};
use gpui::RenderImage;
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::sync::LazyLock;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::budget::BudgetManager;
use super::disk_layer::{DiskLayer, MediaSourceId};
use super::frame_layer::FrameLayer;
use super::memory_layer::MemoryLayer;
use super::preload_plan::{
    EAGER_CHANNEL_PRELOAD_LIMIT, MAX_CHANNEL_PRELOAD_URLS, plan_channel_preload,
};
use super::stats::CacheStats;

pub use super::PickerViewport;

/// Image loading state
#[derive(Clone, Debug, PartialEq)]
pub enum ImageLoadState {
    /// Image is being loaded
    Loading,
    /// Image loaded successfully (static)
    Loaded,
    /// Image failed to load
    Failed,
}

/// Delay before the first retry of a failed image (seconds); later retries
/// double it up to [`MAX_RETRY_BACKOFF_SECS`].
const RETRY_COOLDOWN_SECS: u64 = 30;

/// Longest wait between retries of an image that keeps failing (seconds).
const MAX_RETRY_BACKOFF_SECS: u64 = 300;

/// Retry state for a URL
struct RetryState {
    count: AtomicU8,
    last_retry: Instant,
}

/// Retry tracking for failed URLs
/// Memory optimization: Limited to MAX_RETRY_ENTRIES with TTL-based cleanup
static RETRY_STATES: LazyLock<DashMap<u64, RetryState>> =
    LazyLock::new(|| DashMap::with_capacity(500));

/// Maximum number of retry states to track (prevents unbounded growth)
const MAX_RETRY_ENTRIES: usize = 1000;

/// TTL for retry states (seconds) - entries older than this can be evicted.
/// Longer than the backoff ceiling so a failing image keeps its backoff.
const RETRY_STATE_TTL_SECS: u64 = 900;

/// Wait after `count` retries: 30 s, doubling to a 5 min ceiling. There is no
/// retry limit, so an image that is still shown keeps getting retried.
fn retry_backoff(count: u8) -> Duration {
    let doublings = u32::from(count.saturating_sub(1).min(4));
    Duration::from_secs((RETRY_COOLDOWN_SECS << doublings).min(MAX_RETRY_BACKOFF_SECS))
}

/// Animated image cache state
#[derive(Clone, Debug, PartialEq)]
pub enum AnimatedCacheState {
    /// Animation not loaded
    NotLoaded,
    /// Animation is being decoded
    Loading,
    /// Animation is ready
    Ready,
    /// Animation failed to load
    Failed,
}

/// Global budget manager
static BUDGET: LazyLock<Arc<BudgetManager>> = LazyLock::new(|| Arc::new(BudgetManager::new()));

/// Shared decoded-media budget for app-owned caches that create GPUI
/// `RenderImage`s outside the normal loader layers. Keeping these owners on the
/// same Static allocation counter prevents a second, independent media ceiling.
pub(crate) fn shared_decoded_media_budget() -> Arc<BudgetManager> {
    Arc::clone(&BUDGET)
}

/// Global statistics
static STATS: LazyLock<Arc<CacheStats>> = LazyLock::new(|| Arc::new(CacheStats::new()));

/// Global memory layer
static MEMORY_LAYER: LazyLock<Arc<MemoryLayer>> =
    LazyLock::new(|| Arc::new(MemoryLayer::new(Arc::clone(&BUDGET), Arc::clone(&STATS))));

/// Global disk layer
static DISK_LAYER: LazyLock<Arc<DiskLayer>> = LazyLock::new(|| {
    echo_media_cache::install_cache_log_enabled(|| crate::log::enabled("cache"));
    Arc::new(DiskLayer::new(
        crate::benchmark_profile::config_root_or_current_dir().join("cache"),
        crate::utils::replace_file,
        Arc::clone(&BUDGET),
        Arc::clone(&STATS),
    ))
});

/// Global frame layer for animations
static FRAME_LAYER: LazyLock<Arc<FrameLayer>> = LazyLock::new(|| {
    echo_media_cache::install_cache_log_enabled(|| crate::log::enabled("cache"));
    echo_media_cache::frames::install_retained_decoded_url_predicate(is_retained_decoded_url_key);
    echo_media_cache::frames::install_animated_frame_prefetch(prefetch_animated_frames);
    echo_media_cache::frames::install_visible_animation_count(
        crate::emote_renderer::visible_metadata_count,
    );
    Arc::new(FrameLayer::new(Arc::clone(&BUDGET), Arc::clone(&STATS)))
});

/// Animated decode ownership: URL and provider identity keys map to one task token.
static DECODING_ANIMATED: LazyLock<DashMap<u64, u64>> =
    LazyLock::new(|| DashMap::with_capacity(200));
static DECODING_ANIMATED_STARTED_AT: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(100));
static DECODING_ANIMATED_CLAIM_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static NEXT_ANIMATED_DECODE_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Get global stats
pub fn stats() -> &'static Arc<CacheStats> {
    &STATS
}

/// Decoded-media residency for the memory breakdown diagnostic, in bytes:
/// `(static, frames, metadata, animation_sources, total_budget)`.
///
/// The first three are what the budget manager believes each layer holds; the
/// fourth is the zstd-compressed animation source cache, which is accounted
/// separately because it is not charged to a budget layer.
pub fn decoded_media_residency() -> (usize, usize, usize, usize, usize) {
    use super::budget::CacheLayer;
    (
        BUDGET.used_bytes(CacheLayer::Static),
        BUDGET.used_bytes(CacheLayer::Frames),
        BUDGET.used_bytes(CacheLayer::Metadata),
        DECODED_ANIMATION_SOURCE_BYTES.load(Ordering::Relaxed),
        super::budget::default_total_budget_bytes_public(),
    )
}

/// Bytes of animation refill sources held in sidecar files on disk, including
/// sources the cache evicted that hot frames still hold.
pub fn decoded_animation_source_disk_bytes() -> u64 {
    super::animation_source::sidecar_disk_bytes()
}

/// CPU pixel bytes hot animation frames still hold.
pub fn hot_frame_resident_pixel_bytes() -> usize {
    FRAME_LAYER.resident_pixel_bytes()
}

/// Released hot frames rebuilt from their animation source since start, for a
/// window or device whose atlas lacked them.
pub fn animation_frame_rebuilds() -> u64 {
    ANIMATION_FRAME_REBUILDS.load(Ordering::Relaxed)
}

/// Root of the on-disk media cache.
pub(super) fn media_cache_base_path() -> std::path::PathBuf {
    DISK_LAYER.base_path.clone()
}

/// Picker ownership and hot-frame counts for the opt-in memory diagnostic.
///
/// The fields are observational only; no cache behavior depends on them.
/// Keeping the collection here avoids exposing loader internals to the backend
/// diagnostics module.
pub(crate) struct PickerMediaDiagnostics {
    pub(crate) picker_scopes: usize,
    pub(crate) picker_touched_urls: usize,
    pub(crate) picker_touched_keys: usize,
    pub(crate) picker_visible: super::PickerVisibleMediaStats,
    pub(crate) hot_frames: usize,
    pub(crate) hot_frame_urls: usize,
    pub(crate) picker_protected_frames: usize,
    pub(crate) picker_protected_bytes: usize,
    pub(crate) hot_frames_max_per_url: usize,
    pub(crate) hot_frame_urls_over_limit: usize,
    pub(crate) hot_render_images: usize,
    pub(crate) hot_frame_entry_limit: usize,
    pub(crate) hot_frame_entries_over_limit: usize,
}

pub(crate) fn picker_media_diagnostics() -> PickerMediaDiagnostics {
    let visible = super::picker_visible_media_stats();
    let (
        frames,
        frame_urls,
        protected_frames,
        protected_bytes,
        maximum_frames_per_url,
        urls_over_limit,
        render_images,
        global_entry_limit,
        entries_over_global_limit,
    ) = FRAME_LAYER.diagnostic_residency_counts();
    PickerMediaDiagnostics {
        picker_scopes: BUDGET.picker_scope_count(),
        picker_touched_urls: PICKER_TOUCHED_URLS.len(),
        picker_touched_keys: PICKER_TOUCHED_MEDIA_KEYS.len(),
        picker_visible: visible,
        hot_frames: frames,
        hot_frame_urls: frame_urls,
        picker_protected_frames: protected_frames,
        picker_protected_bytes: protected_bytes,
        hot_frames_max_per_url: maximum_frames_per_url,
        hot_frame_urls_over_limit: urls_over_limit,
        hot_render_images: render_images,
        hot_frame_entry_limit: global_entry_limit,
        hot_frame_entries_over_limit: entries_over_global_limit,
    }
}

/// Maximum concurrent HTTP connections.
///
/// Keep this below the point where startup channel hydration and visible
/// demand compete hard enough to inflate process memory with decoded images,
/// response bodies, and queued GPU work. Must be at least the sum of the two
/// busiest per-host caps (Twitch 12 + 7TV 10) or the per-host tuning in
/// `host_download_limit` can never be reached.
const MAX_CONCURRENT_DOWNLOADS: usize = 24;
/// Default per-host download concurrency (host-specific overrides applied below).
const MAX_CONCURRENT_DOWNLOADS_PER_HOST_DEFAULT: usize = 4;
/// Per-batch concurrent download cap for eager/UI-critical preloads.
///
/// This is the clamp ceiling for `eager_batch_inflight_limit`, whose adaptive
/// targets range 18-32; it must sit at the top of that range or the tuner is
/// inert and every eager batch is pinned to the ceiling instead.
const MAX_EAGER_BATCH_INFLIGHT: usize = 32;
/// Lower bound for eager/UI preload inflight to avoid starvation when constrained.
const MIN_EAGER_BATCH_INFLIGHT: usize = 2;
/// Per-batch concurrent download cap for lazy/background preloads.
///
/// Clamp ceiling for `lazy_batch_inflight_limit`, whose load tiers target
/// 1/5/6/ceiling — a ceiling of 1 would pin every lazy batch to serial
/// downloads and turn the tiering into dead code.
const MAX_LAZY_BATCH_INFLIGHT: usize = 8;
/// Lower bound for lazy/background inflight under high pressure.
const MIN_LAZY_BATCH_INFLIGHT: usize = 1;
/// Startup burst window where eager preload gets a slightly higher target.
const PRELOAD_STARTUP_BURST_WINDOW_SECS: u64 = 20;

/// Longest wait for the next bytes of a media response, in seconds.
const REQUEST_TIMEOUT_SECS: u64 = 4;
/// Backstop for one whole media request, so large files on slow links can finish.
const REQUEST_TOTAL_TIMEOUT_SECS: u64 = 30;
/// Redirect hops a media request may follow (https targets only).
const MAX_MEDIA_REDIRECTS: usize = 3;

/// Maximum batch timeout
const BATCH_TIMEOUT_SECS: u64 = 10;
/// Adaptive upper bound for eager batch timeout (UI-critical but can still be large).
const EAGER_BATCH_TIMEOUT_MAX_SECS: u64 = 25;
/// Adaptive upper bound for lazy/background batch timeout (large warm-start tails).
const LAZY_BATCH_TIMEOUT_MAX_SECS: u64 = 90;

/// Failed URL cooldown in seconds
const FAILED_URL_COOLDOWN_SECS: u64 = 300;
/// Stale in-flight download timeout (seconds).
/// If exceeded, the URL is removed from in-flight tracking and retried.
/// Keep this above the largest owner batch timeout so lazy queued URLs are
/// cleared by their owning preload batch before unrelated batches retry them.
const DOWNLOADING_STALE_TIMEOUT_SECS: u64 = LAZY_BATCH_TIMEOUT_MAX_SECS + 15;
/// Maximum network retry attempts per URL within a single preload operation.
const DOWNLOAD_MAX_ATTEMPTS: u8 = 3;
/// Base retry delay for transient failures.
const DOWNLOAD_RETRY_BASE_MS: u64 = 120;
/// Upper bound for adaptive retry backoff.
const DOWNLOAD_RETRY_MAX_MS: u64 = 1600;
/// Decay interval for host pressure scoring (seconds).
const HOST_PRESSURE_DECAY_STEP_SECS: u64 = 3;
/// Maximum host pressure score retained.
const HOST_PRESSURE_MAX_SCORE: u8 = 24;
/// Maximum soft backpressure delay injected before a request.
const HOST_BACKPRESSURE_MAX_DELAY_MS: u64 = 420;

/// One request may run to the whole-request backstop; earlier failed attempts
/// are budgeted at the stall timeout.
#[inline]
fn minimum_download_batch_timeout_secs() -> u64 {
    let attempts = u64::from(DOWNLOAD_MAX_ATTEMPTS);
    let retry_slots = attempts.saturating_sub(1);
    let request_budget_secs =
        REQUEST_TOTAL_TIMEOUT_SECS.saturating_add(REQUEST_TIMEOUT_SECS.saturating_mul(retry_slots));
    let retry_budget_ms = DOWNLOAD_RETRY_MAX_MS.saturating_mul(retry_slots);
    let backpressure_budget_ms = HOST_BACKPRESSURE_MAX_DELAY_MS.saturating_mul(attempts);
    let delay_budget_secs = retry_budget_ms
        .saturating_add(backpressure_budget_ms)
        .saturating_add(999)
        / 1000;
    request_budget_secs.saturating_add(delay_budget_secs)
}

static DECODED_ANIMATION_SOURCE_CACHE_MB: LazyLock<usize> = LazyLock::new(|| {
    env_usize(
        "ECHO_DECODED_ANIMATION_SOURCE_CACHE_MB",
        decoded_animation_source_cache_mb_default(),
    )
    .min(512)
});

static PICKER_DECODED_ANIMATION_SOURCE_CACHE_MB: LazyLock<usize> = LazyLock::new(|| {
    env_usize(
        "ECHO_PICKER_DECODED_ANIMATION_SOURCE_CACHE_MB",
        decoded_animation_source_cache_mb_picker_default(),
    )
});

/// Cap on animation refill sources held in sidecar files.
static DECODED_ANIMATION_SOURCE_DISK_CACHE_MB: LazyLock<u64> =
    LazyLock::new(|| env_usize("ECHO_DECODED_ANIMATION_DISK_CACHE_MB", 512).min(4096) as u64);

fn decoded_animation_source_disk_budget_bytes() -> u64 {
    *DECODED_ANIMATION_SOURCE_DISK_CACHE_MB * 1024 * 1024
}

fn decoded_animation_source_cache_budget_bytes() -> usize {
    let base_mb = *DECODED_ANIMATION_SOURCE_CACHE_MB;
    let mb = if BUDGET.picker_scope_active() {
        (*PICKER_DECODED_ANIMATION_SOURCE_CACHE_MB).clamp(base_mb, 512)
    } else {
        base_mb
    };
    mb * 1024 * 1024
}

/// RAM-tiered default (MB) for the CPU-decoded animation source cache.
///
/// Mirrors the media-budget tiers in `budget.rs`: same prewarmed system-memory
/// probe and the same 8.5 / 16.5 GB thresholds. The explicit
/// `ECHO_DECODED_ANIMATION_SOURCE_CACHE_MB` override in
/// `decoded_animation_source_cache_budget_bytes` still takes precedence.
///   - ≤ 8 GB:  64 MB
///   - ≤16 GB:  96 MB
///   - >16 GB: 128 MB (unchanged from the prior flat default)
fn decoded_animation_source_cache_mb_default() -> usize {
    let total_gb =
        (super::budget::prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
    if total_gb <= 8.5 {
        64
    } else if total_gb <= 16.5 {
        96
    } else {
        128
    }
}

/// Expanded compressed-source residency while a picker is open. The tiers
/// mirror the picker media budget so an unusually dense exact viewport can
/// keep refill data for continuously playing animations without evicting and
/// re-decoding it on every frame transition.
fn decoded_animation_source_cache_mb_picker_default() -> usize {
    let total_gb =
        (super::budget::prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
    if total_gb <= 8.5 {
        128
    } else if total_gb <= 16.5 {
        256
    } else {
        512
    }
}

fn warm_start_eager_url_limit() -> usize {
    static LIMIT: LazyLock<usize> = LazyLock::new(|| {
        env_usize(
            "ECHO_WARM_START_EAGER_URL_LIMIT",
            WARM_START_EAGER_URL_LIMIT_DEFAULT,
        )
        .min(512)
    });
    *LIMIT
}

/// RAM-tiered default for concurrent animated decodes.
///
/// Each in-flight streaming decode still owns a decoder canvas, its current raw
/// frame, and the growing compressed refill source. Scale the permit count with
/// system memory like the other caches so animated-heavy channel joins do not
/// multiply that bounded transient work without limit. The
/// `ECHO_MAX_CONCURRENT_ANIMATED_DECODES` override for the reserved non-picker
/// lane still takes precedence.
///   - <= 8 GB: 2
///   - <=16 GB: 3
///   - >16 GB:  4 (unchanged from the prior flat default)
fn max_concurrent_animated_decodes_default() -> usize {
    let total_gb =
        (super::budget::prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
    if total_gb <= 8.5 {
        2
    } else if total_gb <= 16.5 {
        3
    } else {
        4
    }
}

/// Picker-only full-decode concurrency.
///
/// Dense picker views use compact provider variants and can expose hundreds of
/// independent animations. Scale their headroom with both RAM and logical CPU
/// count so a maximized catalog converges without changing the independent
/// normal-chat cap.
fn max_concurrent_picker_animated_decodes_default() -> usize {
    let normal = max_concurrent_animated_decodes_default();
    let total_gb =
        (super::budget::prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
    let ram_limit = if total_gb <= 8.5 {
        4
    } else if total_gb <= 16.5 {
        6
    } else {
        12
    };
    let cpu_limit = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(normal);
    ram_limit.min(cpu_limit).max(normal)
}

/// URLs currently being downloaded
static DOWNLOADING: LazyLock<DashSet<u64>> = LazyLock::new(|| DashSet::with_capacity(1000));
/// Download start timestamps for in-flight URLs (for stale detection).
static DOWNLOADING_SINCE: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(1000));

/// Stable media source IDs that have been successfully resolved to disk or memory.
///
/// URL state is still tracked separately for legacy/non-emote callers. This set
/// lets emote-aware preload paths coalesce aliases for the same provider/id/
/// variant across splits and channels.
static LOADED_MEDIA_KEYS: LazyLock<DashSet<u64>> = LazyLock::new(|| DashSet::with_capacity(5000));
/// Hard cap on LOADED_MEDIA_KEYS. When exceeded, ~10% of entries are pruned
/// arbitrarily. Re-population is cheap (cache layers will re-mark on next hit).
const LOADED_MEDIA_KEYS_MAX: usize = 16_384;
/// URLs that failed to load (with timestamp)
static FAILED_URLS: LazyLock<DashMap<u64, Instant>> = LazyLock::new(|| DashMap::with_capacity(500));
/// Static-fallback requeues that never produced a resident frame, keyed by URL
/// hash: (requeues in the current window, window start).
static STATIC_FALLBACK_REQUEUES: LazyLock<DashMap<u64, (u8, Instant)>> =
    LazyLock::new(DashMap::new);
/// Requeues within one window after which a static fallback that the memory
/// layer keeps refusing is treated as failed until the failure cooldown ends.
const STATIC_FALLBACK_MAX_REQUEUES: u8 = 5;
const STATIC_FALLBACK_REQUEUE_WINDOW: Duration = Duration::from_secs(60);
const STATIC_FALLBACK_REQUEUE_TRACKED_MAX: usize = 1024;
/// Hard cap on FAILED_URLS. Prevents unbounded growth if a large number of
/// distinct URLs fail (e.g. CDN outage). Oldest-ish entries are pruned when hit.
const FAILED_URLS_MAX: usize = 2_000;

/// Prune ~10% of LOADED_MEDIA_KEYS entries when over cap.
/// Called just before inserting to keep size bounded.
#[inline]
fn maybe_prune_loaded_media_keys() {
    if LOADED_MEDIA_KEYS.len() >= LOADED_MEDIA_KEYS_MAX {
        let prune_count = LOADED_MEDIA_KEYS_MAX / 10;
        let to_remove: Vec<u64> = LOADED_MEDIA_KEYS
            .iter()
            .take(prune_count)
            .map(|r| *r)
            .collect();
        for key in to_remove {
            LOADED_MEDIA_KEYS.remove(&key);
        }
    }
}

/// Prune ~10% of FAILED_URLS entries when over cap (backstop; TTL cleanup is primary).
#[inline]
fn maybe_prune_failed_urls() {
    if FAILED_URLS.len() >= FAILED_URLS_MAX {
        let prune_count = FAILED_URLS_MAX / 10;
        let to_remove: Vec<u64> = FAILED_URLS
            .iter()
            .take(prune_count)
            .map(|r| *r.key())
            .collect();
        for key in to_remove {
            FAILED_URLS.remove(&key);
        }
    }
}

/// Animated URLs confirmed to serve static bytes (common Twitch CDN quirk).
/// Memoizing this prevents repeated animated decode attempts and GPUI animated fallback churn.
static ANIMATED_STATIC_FALLBACK_URLS: LazyLock<DashSet<u64>> =
    LazyLock::new(|| DashSet::with_capacity(512));
/// Monotonic invalidation key for picker animation-variant eligibility.
/// A compact source becoming a terminal static fallback makes its primary
/// variant the active decode URL and must invalidate a same-range publication.
static ANIMATED_STATE_REVISION: AtomicU64 = AtomicU64::new(1);

/// Animated URLs intentionally decoded into the static memory layer as picker
/// posters. A poster must not make the full-animation decoder conclude that the
/// animation is already satisfied by a static fallback.
static ANIMATED_POSTER_URLS: LazyLock<DashSet<u64>> =
    LazyLock::new(|| DashSet::with_capacity(2048));

/// Channels currently being preloaded
static PRELOADING_CHANNELS: LazyLock<DashSet<String>> =
    LazyLock::new(|| DashSet::with_capacity(20));

/// Shared HTTP client
static HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_media_http_client));

/// URLs that came from link-preview metadata. Any page can name them, so they
/// are fetched with [`EMBED_THUMBNAIL_HTTP_CLIENT`] whichever surface asks.
static EMBED_THUMBNAIL_URLS: LazyLock<DashSet<u64>> = LazyLock::new(DashSet::new);

/// Media client with the embed fetcher's public-only redirect and DNS policy.
static EMBED_THUMBNAIL_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_embed_thumbnail_http_client));

fn media_client_for(url: &str) -> &'static crate::backend::http::ClientCache {
    if EMBED_THUMBNAIL_URLS.contains(&url_key(url)) {
        &EMBED_THUMBNAIL_HTTP_CLIENT
    } else {
        &HTTP_CLIENT
    }
}

fn build_embed_thumbnail_http_client() -> reqwest::Client {
    media_http_client_builder()
        .redirect(crate::backend::embeds::fetchers::embed_redirect_policy())
        .dns_resolver(crate::backend::embeds::fetchers::PublicOnlyResolver)
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("Failed to create embed thumbnail HTTP client: {error}");
            crate::backend::embeds::fetchers::blocked_embed_client()
        })
}

fn media_http_client_builder() -> reqwest::ClientBuilder {
    crate::backend::http::client_builder()
        .read_timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .timeout(std::time::Duration::from_secs(REQUEST_TOTAL_TIMEOUT_SECS))
        .connect_timeout(std::time::Duration::from_secs(3))
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .tcp_nodelay(true)
}

fn build_media_http_client() -> reqwest::Client {
    media_http_client_builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > MAX_MEDIA_REDIRECTS {
                attempt.error("too many media redirects")
            } else if attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("Failed to create media HTTP client: {error}");
            crate::backend::http::timed_client(std::time::Duration::from_secs(
                REQUEST_TOTAL_TIMEOUT_SECS,
            ))
        })
}

/// Why a media fetch produced no bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaFetchFailure {
    /// The server answered for good (4xx other than 429, or an oversized
    /// body), so a smaller copy of the same emote may still exist.
    Refused,
    /// Transport errors, 429 or 5xx: a later retry may work, and smaller
    /// copies from the same host would fail the same way.
    Transient,
}

/// Fetch one media URL with the loader's transient-failure retry policy.
async fn fetch_media_bytes(url: &str) -> Result<Arc<[u8]>, MediaFetchFailure> {
    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        let transfer = acquire_network_transfer(url, None, Arc::clone(&STATS))
            .await
            .ok_or(MediaFetchFailure::Transient)?;
        let retry_status = match media_client_for(url).get(url).send().await {
            Ok(response) if response.status().is_success() => {
                match read_media_body(response).await {
                    Ok(bytes) => return Ok(bytes),
                    Err(MediaBodyError::TooLarge) => return Err(MediaFetchFailure::Refused),
                    Err(MediaBodyError::Read) => None,
                }
            }
            Ok(response) => {
                let status = response.status();
                if !is_retryable_status(status) {
                    return Err(MediaFetchFailure::Refused);
                }
                Some(status)
            }
            Err(error) => {
                if !is_retryable_error(&error) {
                    return Err(MediaFetchFailure::Transient);
                }
                None
            }
        };
        drop(transfer);
        if attempt < DOWNLOAD_MAX_ATTEMPTS {
            tokio::time::sleep(adaptive_retry_delay(url, attempt, retry_status)).await;
        }
    }
    Err(MediaFetchFailure::Transient)
}

/// Why a successful media response produced no body. Only a read failure is
/// worth retrying; an oversized body would be downloaded again for nothing.
enum MediaBodyError {
    TooLarge,
    Read,
}

/// Read a media response body, refusing anything larger than a disk cache entry.
async fn read_media_body(mut response: reqwest::Response) -> Result<Arc<[u8]>, MediaBodyError> {
    let max_bytes = super::disk_layer::MAX_DISK_CACHE_ENTRY_BYTES;
    let expected = response.content_length().unwrap_or_default();
    if expected > max_bytes as u64 {
        return Err(MediaBodyError::TooLarge);
    }
    let mut body = Vec::with_capacity(expected as usize);
    while let Some(chunk) = response.chunk().await.map_err(|_| MediaBodyError::Read)? {
        if chunk.len() > max_bytes - body.len() {
            return Err(MediaBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.into())
}

/// Download concurrency semaphore
static DOWNLOAD_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)));

// Reserve visible decode admission independently of bulk hydration.
static VISIBLE_STATIC_DECODE_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));
static BACKGROUND_STATIC_DECODE_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(1)));
// Bound encoded bodies before taking network capacity, with room to overlap I/O and decode.
static VISIBLE_STATIC_BODY_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(8)));
static BACKGROUND_STATIC_BODY_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(4)));

struct AdmittedStaticBytes<B> {
    bytes: B,
    _admission: Arc<OwnedSemaphorePermit>,
}

impl<B: AsRef<[u8]>> AsRef<[u8]> for AdmittedStaticBytes<B> {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

async fn acquire_static_body_admission(source: PreloadSource) -> Option<Arc<OwnedSemaphorePermit>> {
    let lane = if source.priority(false) == 0 {
        &VISIBLE_STATIC_BODY_SEMAPHORE
    } else {
        &BACKGROUND_STATIC_BODY_SEMAPHORE
    };
    let start = Instant::now();
    let permit = Arc::clone(lane).acquire_owned().await.ok()?;
    record_cache_emote_stage_with_source(
        "cache.preload.wait_static_body_permit",
        source,
        start.elapsed(),
        1,
    );
    Some(Arc::new(permit))
}

fn picker_static_request_is_stale(
    url: &str,
    source_key: Option<u64>,
    source: PreloadSource,
) -> bool {
    let url_hash = url_key(url);
    let request_key = source_key.unwrap_or(url_hash);
    source.is_picker()
        && !picker_scope_owns_media(url_hash, request_key)
        && !is_retained_decoded_media(url_hash, request_key)
}

/// Picker-only CPU/memory concurrency ceiling for animated decode.
///
/// Keeping this queue independent prevents a maximized picker from placing
/// hundreds of FIFO waiters ahead of normal chat animation work.
static PICKER_ANIMATION_DECODE_LANE_LIMITS: LazyLock<(usize, usize)> = LazyLock::new(|| {
    let total = env_usize(
        "ECHO_MAX_CONCURRENT_PICKER_ANIMATED_DECODES",
        max_concurrent_picker_animated_decodes_default(),
    )
    .clamp(1, 16);
    if total == 1 {
        return (1, 0);
    }
    let completion = (total / 6).max(1);
    (total.saturating_sub(completion).max(1), completion)
});

static ANIMATED_PICKER_DECODE_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(PICKER_ANIMATION_DECODE_LANE_LIMITS.0)));

/// Reserved non-picker decode lane. Picker work never acquires this semaphore,
/// so chat animation admission cannot be starved by the picker queue.
static ANIMATED_NON_PICKER_DECODE_SEMAPHORE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let permits = env_usize(
        "ECHO_MAX_CONCURRENT_ANIMATED_DECODES",
        max_concurrent_animated_decodes_default(),
    )
    .clamp(1, 16);
    Arc::new(Semaphore::new(permits))
});

/// A maximized picker can expose hundreds of independent animations at once.
/// Dense views can expose hundreds of independently managed textures. Keep the
/// picker window aligned with its two-frame per-animation residency cap: the
/// current frame plus one-frame runway is enough for progressive refill without
/// rebuilding the large texture population that the cap exists to prevent.
const PICKER_ANIMATION_INITIAL_DECODE_WINDOW_FRAMES: u16 = 2;
/// Materialize only the requested next frame after initial picker admission.
///
/// Picker animations retain the current frame plus one-frame runway. Expanding
/// one missing lookahead into another two-frame window rotates both resident
/// textures out before the source timeline reaches them, so playback can skip
/// ahead at refill speed. Initial admission remains two frames; subsequent
/// refills replace only the obsolete frame.
const PICKER_ANIMATION_REFILL_DECODE_WINDOW_FRAMES: u16 = 1;
/// Name-paint animations are uncommon, compact chat decorations rather than a
/// dense picker workload. This is the sliding-window fallback for a paint whose
/// complete decoded cycle is too large to keep hot.
const NAME_PAINT_ANIMATION_DECODE_WINDOW_FRAMES: u16 = 32;
/// Keep a complete name-paint cycle hot when its raw frame payload is compact.
///
/// A frame-count-only window covered Staff Shine (24 frames) but not long,
/// lightweight overlay effects such as k4Qu's 150-frame `elis` paint. Those
/// paints otherwise cross a refill boundary roughly once per second and can
/// visibly hold or jump between resident windows. Eight MiB admits both known
/// reference paints while leaving the global frame budget authoritative for
/// multiple distinct animations and keeping larger sources on the bounded
/// sliding-window path.
const NAME_PAINT_ANIMATION_FULL_CYCLE_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Host-scoped semaphores to smooth burst pressure against individual CDNs.
static HOST_DOWNLOAD_SEMAPHORES: LazyLock<DashMap<String, Arc<Semaphore>>> =
    LazyLock::new(|| DashMap::with_capacity(16));

#[inline]
fn is_7tv_name_paint_url(url: &str) -> bool {
    url.contains("//cdn.7tv.app/paint/")
}

/// Resolve the initial hot-frame window for one decoded 7TV name paint.
///
/// Exposed for the non-test animation decoder diagnostic so live reference
/// assets can verify the same admission decision used by the runtime.
#[doc(hidden)]
pub fn name_paint_animation_decode_window(frame_count: usize, raw_frame_bytes: usize) -> u16 {
    let frame_count = frame_count.min(u16::MAX as usize) as u16;
    if raw_frame_bytes <= NAME_PAINT_ANIMATION_FULL_CYCLE_MAX_BYTES {
        frame_count
    } else {
        NAME_PAINT_ANIMATION_DECODE_WINDOW_FRAMES.min(frame_count)
    }
}

/// Recent per-host pressure signals (timeouts/429s/retryable errors) used to apply
/// soft backpressure without permanently lowering concurrency.
static HOST_DOWNLOAD_PRESSURE: LazyLock<DashMap<String, HostPressureState>> =
    LazyLock::new(|| DashMap::with_capacity(16));

/// Shared runtime for background preloading
static PRELOAD_RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
    let default_blocking_threads = max_concurrent_animated_decodes_default()
        .saturating_add(max_concurrent_picker_animated_decodes_default())
        .clamp(8, 16);
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(env_usize("ECHO_CACHE_LOADER_THREADS", 1).clamp(1, 8))
        .max_blocking_threads(
            env_usize("ECHO_CACHE_BLOCKING_THREADS", default_blocking_threads).clamp(2, 16),
        )
        .thread_name("cache-loader")
        .enable_all()
        .build()
    {
        Ok(runtime) => Box::leak(Box::new(runtime)).handle().clone(),
        Err(error) => {
            tracing::error!("Failed to create preload runtime, using the backend runtime: {error}");
            crate::backend::bridge::get_runtime().handle().clone()
        }
    }
});

/// Active download counter (for stats)
static ACTIVE_DOWNLOADS: AtomicU32 = AtomicU32::new(0);
static PRELOAD_TUNER_STARTED_AT: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Maximum concurrent preload threads (bounds unbounded thread spawning)
const MAX_CONCURRENT_PRELOADS: u32 = 4;

/// Currently active preload thread count
static ACTIVE_PRELOAD_THREADS: AtomicU32 = AtomicU32::new(0);

/// Media whose decoded representation was requested by the emote picker.
///
/// Disk cache entries are not removed when this scope is purged. The picker can
/// reopen from disk without keeping all decoded textures and frames resident.
#[derive(Clone)]
struct PickerTouchedMedia {
    url: Arc<str>,
    identity: Option<Arc<MediaSourceId>>,
}

impl PickerTouchedMedia {
    fn request_key(&self) -> u64 {
        media_request_key(&self.url, self.identity.as_deref())
    }
}

static PICKER_TOUCHED_URLS: LazyLock<DashMap<u64, PickerTouchedMedia>> =
    LazyLock::new(|| DashMap::with_capacity(2048));
static PICKER_TOUCHED_MEDIA_KEYS: LazyLock<DashSet<u64>> =
    LazyLock::new(|| DashSet::with_capacity(2048));

/// URL and provider-identity keys owned by chat, channel, split, or startup paths.
///
/// Picker cleanup must not evict these. Lazy disk-only hints intentionally do
/// not mark retention; the URL is retained only when a non-picker path requested
/// decoded media.
static RETAINED_DECODED_URLS: LazyLock<DashSet<u64>> =
    LazyLock::new(|| DashSet::with_capacity(8192));
/// Hard cap on RETAINED_DECODED_URLS. This set is insert-only (no removal path),
/// so it would otherwise grow without bound as distinct emotes are displayed.
/// When exceeded, ~10% of entries are pruned arbitrarily. Pruning an entry only
/// makes that URL's decoded frames evictable again by the idle sweep — worst
/// case a re-decode on next display — never incorrect rendering.
const RETAINED_DECODED_URLS_MAX: usize = 16_384;

struct CachedDecodedAnimation {
    source: Arc<super::animation_source::AnimationSource>,
    /// RAM and sidecar bytes added to the cache totals while this entry is in
    /// `DECODED_ANIMATION_SOURCES`. Changed only under that map's entry lock,
    /// so a removal always subtracts what was added.
    charged_ram: AtomicUsize,
    charged_disk: AtomicU64,
    last_accessed: Mutex<Instant>,
}

impl CachedDecodedAnimation {
    /// Add this entry's current RAM and disk bytes to the cache totals.
    fn charge(&self) {
        let (ram, disk) = self.source.byte_usage();
        let old_ram = self.charged_ram.swap(ram, Ordering::Relaxed);
        let old_disk = self.charged_disk.swap(disk, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_BYTES.fetch_sub(old_ram, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_BYTES.fetch_add(ram, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_DISK_BYTES.fetch_sub(old_disk, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_DISK_BYTES.fetch_add(disk, Ordering::Relaxed);
    }

    /// Remove this entry's bytes from the cache totals, returning the RAM freed.
    fn uncharge(&self) -> usize {
        let ram = self.charged_ram.swap(0, Ordering::Relaxed);
        let disk = self.charged_disk.swap(0, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_BYTES.fetch_sub(ram, Ordering::Relaxed);
        DECODED_ANIMATION_SOURCE_DISK_BYTES.fetch_sub(disk, Ordering::Relaxed);
        ram
    }
}

/// CPU-decoded animation frames used as the refill source for hot GPU frames.
///
/// The hot `FrameLayer` stays small and GPU-facing. This cache avoids decoding
/// the same GIF/WebP source again every time playback advances past that hot
/// window.
static DECODED_ANIMATION_SOURCES: LazyLock<DashMap<u64, Arc<CachedDecodedAnimation>>> =
    LazyLock::new(|| DashMap::with_capacity(128));
static DECODED_ANIMATION_SOURCE_ALIASES: LazyLock<DashMap<u64, u64>> =
    LazyLock::new(|| DashMap::with_capacity(128));
static DECODED_ANIMATION_SOURCE_BYTES: AtomicUsize = AtomicUsize::new(0);
static DECODED_ANIMATION_SOURCE_DISK_BYTES: AtomicU64 = AtomicU64::new(0);
static ANIMATION_FRAME_DECOMPRESSIONS: AtomicU64 = AtomicU64::new(0);
static ANIMATION_FRAME_REBUILDS: AtomicU64 = AtomicU64::new(0);
static ANIMATION_REFILL_WINDOWS: AtomicU64 = AtomicU64::new(0);
static ANIMATION_REFILL_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Cumulative `(frame_decompressions, refill_windows, refill_frames)` since
/// process start. Decompressions include initial windows; refill windows and
/// frames count only prefetch-token windows that materialized at least one
/// frame after the initial window.
pub(crate) fn animation_refill_totals() -> (u64, u64, u64) {
    (
        ANIMATION_FRAME_DECOMPRESSIONS.load(Ordering::Relaxed),
        ANIMATION_REFILL_WINDOWS.load(Ordering::Relaxed),
        ANIMATION_REFILL_FRAMES.load(Ordering::Relaxed),
    )
}
/// First disk miss observed after a decoded source was evicted. A brief grace
/// period lets the detached initial disk save finish; a persistent miss then
/// invalidates stale metadata so normal network decode can recover the URL.
static ANIMATION_REFILL_DISK_MISS_SINCE: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(128));
const ANIMATION_REFILL_DISK_MISS_GRACE: Duration = Duration::from_secs(3);
const ANIMATION_REFILL_DISK_MISS_MAX: usize = 2_048;

/// Debounce interval for batching small preload requests (milliseconds)
const DEBOUNCE_INTERVAL_MS: u64 = 50;
/// Suppress enqueueing identical eager preload URLs too frequently.
const EAGER_PRELOAD_ENQUEUE_COOLDOWN_MS: u64 = 700;
/// Suppress enqueueing identical lazy preload URLs too frequently.
const LAZY_PRELOAD_ENQUEUE_COOLDOWN_MS: u64 = 1800;
/// TTL for recent enqueue tracking map.
const EAGER_PRELOAD_ENQUEUE_TTL_SECS: u64 = 30;
/// TTL for recent lazy enqueue tracking map.
const LAZY_PRELOAD_ENQUEUE_TTL_SECS: u64 = 30;

/// Minimum batch size to log at INFO level (smaller batches use TRACE)
const LOG_BATCH_SIZE_THRESHOLD: usize = 25;
const CACHE_EMOTE_STAGE_WARN_MS: u128 = 750;
const MAX_BATCH_PRELOAD_URLS_PER_CHANNEL: usize = MAX_CHANNEL_PRELOAD_URLS;
const MAX_BATCH_PRELOAD_URLS_TOTAL: usize = 96;
const BATCH_CHANNEL_EAGER_URLS_PER_CHANNEL: usize = 4;
const BATCH_CHANNEL_LAZY_URLS_PER_CHANNEL: usize = 20;
/// Warm-start eager hydration decodes a bounded set of hot static disk-cache
/// entries in background chunks. This keeps first channel/picker use responsive
/// without decoding a full emote set at startup.
const WARM_START_EAGER_URL_LIMIT_DEFAULT: usize = 50;
/// Warm-start eager hydration chunk size.
const WARM_START_EAGER_BATCH_SIZE: usize = 16;
/// Delay between warm-start eager chunks to keep startup smooth.
const WARM_START_EAGER_BATCH_DELAY_MS: u64 = 120;
/// Disk-only warm-start hydration concurrency.
const WARM_START_DISK_ONLY_INFLIGHT: usize = 4;

/// Logical source of an image preload request.
///
/// These labels are intentionally coarse. They make live logs actionable
/// without tying metrics to every call-site detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreloadSource {
    VisibleChat,
    VisibleLayout,
    History,
    IrcMessage,
    Channel,
    ChannelLazyTail,
    ChannelBatchEager,
    ChannelBatchLazy,
    DiskCacheLoad,
    /// A small transient autocomplete surface. Its decoded media stays
    /// evictable once the completion popup disappears.
    AutocompleteVisible,
    /// One hovered tooltip preview. Full-quality animation remains evictable
    /// after the pointer leaves instead of becoming channel-retained media.
    TooltipVisible,
    PickerVisible,
    PickerCell,
    EmbedThumbnail,
    StartupWarm,
}

impl PreloadSource {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VisibleChat => "visible_chat",
            Self::VisibleLayout => "visible_layout",
            Self::History => "history",
            Self::IrcMessage => "irc_message",
            Self::Channel => "channel",
            Self::ChannelLazyTail => "channel_lazy_tail",
            Self::ChannelBatchEager => "channel_batch_eager",
            Self::ChannelBatchLazy => "channel_batch_lazy",
            Self::DiskCacheLoad => "disk_cache_load",
            Self::AutocompleteVisible => "autocomplete_visible",
            Self::TooltipVisible => "tooltip_visible",
            Self::PickerVisible => "picker_visible",
            Self::PickerCell => "picker_cell",
            Self::EmbedThumbnail => "embed_thumbnail",
            Self::StartupWarm => "startup_warm",
        }
    }

    #[inline]
    fn is_picker(self) -> bool {
        matches!(self, Self::PickerVisible | Self::PickerCell)
    }

    #[inline]
    fn retains_decoded_media(self) -> bool {
        !self.is_picker() && !matches!(self, Self::AutocompleteVisible | Self::TooltipVisible)
    }

    #[inline]
    fn notifies_image_ready(self, lazy: bool) -> bool {
        if lazy {
            return matches!(self, Self::IrcMessage);
        }

        !matches!(
            self,
            Self::History
                | Self::Channel
                | Self::ChannelLazyTail
                | Self::ChannelBatchEager
                | Self::ChannelBatchLazy
                | Self::DiskCacheLoad
                | Self::StartupWarm
        )
    }

    #[inline]
    fn priority(self, lazy: bool) -> u8 {
        if !lazy {
            match self {
                Self::VisibleChat
                | Self::VisibleLayout
                | Self::AutocompleteVisible
                | Self::TooltipVisible
                | Self::PickerVisible => 0,
                Self::PickerCell | Self::IrcMessage | Self::EmbedThumbnail => 1,
                Self::History | Self::Channel | Self::ChannelBatchEager => 2,
                Self::DiskCacheLoad | Self::StartupWarm => 3,
                Self::ChannelLazyTail | Self::ChannelBatchLazy => 4,
            }
        } else {
            match self {
                Self::IrcMessage | Self::VisibleChat | Self::VisibleLayout => 0,
                Self::History | Self::Channel | Self::ChannelBatchLazy => 2,
                Self::DiskCacheLoad | Self::StartupWarm => 3,
                _ => 4,
            }
        }
    }
}

struct PendingPreload {
    url: Arc<str>,
    identity: Option<Arc<MediaSourceId>>,
    source: PreloadSource,
}

#[derive(Clone, Debug)]
pub struct MediaRequest {
    pub url: Arc<str>,
    pub identity: Option<Arc<MediaSourceId>>,
}

impl MediaRequest {
    pub fn new(url: impl Into<Arc<str>>, identity: Option<MediaSourceId>) -> Self {
        Self {
            url: url.into(),
            identity: identity.map(Arc::new),
        }
    }
}

#[inline]
fn into_owned_media_identity(identity: Option<Arc<MediaSourceId>>) -> Option<MediaSourceId> {
    identity.map(|identity| {
        Arc::try_unwrap(identity).unwrap_or_else(|identity| identity.as_ref().clone())
    })
}

/// Persistently mark media that was actually displayed in chat.
///
/// The disk layer keeps displayed emotes out of normal eviction for three days.
/// The call is intentionally fire-and-forget: rendering must never wait on
/// index IO, and not-yet-downloaded media receives a fresh timestamp when the
/// source bytes are saved.
pub fn record_displayed_media_requests(requests: &[MediaRequest]) {
    if requests.is_empty() {
        return;
    }

    let now = Instant::now();
    let mut due = Vec::new();
    for request in requests {
        let key = media_request_key_for_request(request);
        let should_record = match RECENT_DISPLAYED_MEDIA_RECORDS.entry(key) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                if now.duration_since(*entry.get()) < DISPLAYED_MEDIA_RECORD_COOLDOWN {
                    false
                } else {
                    entry.insert(now);
                    true
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        };
        if should_record {
            due.push(request.clone());
        }
    }
    if due.is_empty() {
        return;
    }

    crate::backend::bridge::get_runtime().spawn(async move {
        for request in due {
            DISK_LAYER
                .mark_source_displayed(request.identity.as_deref(), &request.url)
                .await;
        }
    });
}

#[inline]
fn media_source_key(identity: &MediaSourceId) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = ahash::AHasher::default();
    "media-source".hash(&mut hasher);
    identity.provider.hash(&mut hasher);
    identity.id.hash(&mut hasher);
    identity.variant.scale.hash(&mut hasher);
    identity.variant.format.hash(&mut hasher);
    identity.variant.animated_kind.hash(&mut hasher);
    hasher.finish()
}

#[inline]
fn media_request_key(url: &str, identity: Option<&MediaSourceId>) -> u64 {
    identity
        .map(media_source_key)
        .unwrap_or_else(|| url_key(url))
}

#[inline]
fn media_request_key_for_request(request: &MediaRequest) -> u64 {
    media_request_key(&request.url, request.identity.as_deref())
}

#[inline]
fn static_source_key(identity: Option<&MediaSourceId>) -> Option<u64> {
    identity.map(media_source_key)
}

fn alias_static_memory_if_resident(url: &str, identity: Option<&MediaSourceId>) -> bool {
    alias_static_memory_if_resident_keyed(identity, url_key(url))
}

#[inline]
fn alias_static_memory_if_resident_keyed(identity: Option<&MediaSourceId>, url_hash: u64) -> bool {
    let Some(source_key) = static_source_key(identity) else {
        return false;
    };
    if MEMORY_LAYER.alias_url_to_source_key(url_hash, source_key) {
        mark_url_loaded(url_hash);
        maybe_prune_loaded_media_keys();
        LOADED_MEDIA_KEYS.insert(source_key);
        crate::components::overlays::notify_image_loaded();
        return true;
    }
    false
}

#[inline]
fn static_fallback_resident(url_hash: u64, source_key: Option<u64>) -> bool {
    MEMORY_LAYER.contains(url_hash)
        || source_key.is_some_and(|source_key| MEMORY_LAYER.contains_source_key(source_key))
}

fn mark_animated_static_fallback(url: &str, identity: Option<&MediaSourceId>) {
    let key = url_key(url);
    let mut changed = ANIMATED_STATIC_FALLBACK_URLS.insert(key);
    if let Some(source_key) = static_source_key(identity) {
        changed |= ANIMATED_STATIC_FALLBACK_URLS.insert(source_key);
    }
    if changed {
        ANIMATED_STATE_REVISION.fetch_add(1, Ordering::AcqRel);
    }
}

/// Count one more requeue of a non-resident static fallback; true once the
/// window's budget is spent, which also forgets the entry.
fn static_fallback_requeue_exhausted(url_hash: u64) -> bool {
    let now = Instant::now();
    if STATIC_FALLBACK_REQUEUES.len() >= STATIC_FALLBACK_REQUEUE_TRACKED_MAX {
        STATIC_FALLBACK_REQUEUES.retain(|_, (_, started)| {
            now.duration_since(*started) < STATIC_FALLBACK_REQUEUE_WINDOW
        });
    }
    let exhausted = {
        let mut entry = STATIC_FALLBACK_REQUEUES.entry(url_hash).or_insert((0, now));
        if now.duration_since(entry.1) >= STATIC_FALLBACK_REQUEUE_WINDOW {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0 > STATIC_FALLBACK_MAX_REQUEUES
    };
    if exhausted {
        STATIC_FALLBACK_REQUEUES.remove(&url_hash);
    }
    exhausted
}

fn clear_animated_static_fallback(url: &str, identity: Option<&MediaSourceId>) {
    let key = url_key(url);
    ANIMATED_STATIC_FALLBACK_URLS.remove(&key);
    if let Some(source_key) = static_source_key(identity) {
        ANIMATED_STATIC_FALLBACK_URLS.remove(&source_key);
    }
}

fn mark_animated_decode_failed(url: &str, identity: Option<&MediaSourceId>) {
    maybe_prune_failed_urls();
    let mut changed = FAILED_URLS.insert(url_key(url), Instant::now()).is_none();
    if let Some(source_key) = static_source_key(identity) {
        changed |= FAILED_URLS.insert(source_key, Instant::now()).is_none();
    }
    if changed {
        ANIMATED_STATE_REVISION.fetch_add(1, Ordering::AcqRel);
    }
}

fn clear_animated_decode_failed(url: &str, identity: Option<&MediaSourceId>) {
    FAILED_URLS.remove(&url_key(url));
    if let Some(source_key) = static_source_key(identity) {
        FAILED_URLS.remove(&source_key);
    }
}

fn mark_picker_touched_media(url: &str, identity: Option<&MediaSourceId>) {
    if !url.is_empty() {
        let url_hash = url_key(url);
        let request_key = media_request_key(url, identity);
        PICKER_TOUCHED_MEDIA_KEYS.insert(request_key);
        match PICKER_TOUCHED_URLS.entry(url_hash) {
            DashEntry::Occupied(mut entry) => {
                if entry.get().identity.is_none() && identity.is_some() {
                    entry.get_mut().identity = identity.cloned().map(Arc::new);
                }
            }
            DashEntry::Vacant(entry) => {
                entry.insert(PickerTouchedMedia {
                    url: Arc::from(url),
                    identity: identity.cloned().map(Arc::new),
                });
            }
        }
    }
}

/// Retain the ownership already carried by a media request instead of
/// allocating duplicate picker-scope URL and provider identity records.
fn mark_picker_touched_request(request: &MediaRequest) {
    if request.url.is_empty() {
        return;
    }

    let url_hash = url_key(&request.url);
    let request_key = media_request_key_for_request(request);
    PICKER_TOUCHED_MEDIA_KEYS.insert(request_key);
    match PICKER_TOUCHED_URLS.entry(url_hash) {
        DashEntry::Occupied(mut entry) => {
            if entry.get().identity.is_none() && request.identity.is_some() {
                entry.get_mut().identity = request.identity.clone();
            }
        }
        DashEntry::Vacant(entry) => {
            entry.insert(PickerTouchedMedia {
                url: Arc::clone(&request.url),
                identity: request.identity.clone(),
            });
        }
    }
}

#[inline]
fn mark_picker_touched_url(url: &str) {
    mark_picker_touched_media(url, None);
}

fn mark_picker_touched_urls<T: AsRef<str>>(urls: &[T]) {
    for url in urls {
        mark_picker_touched_url(url.as_ref());
    }
}

/// Prune ~10% of RETAINED_DECODED_URLS entries when over cap.
/// Called just before inserting to keep size bounded.
#[inline]
fn maybe_prune_retained_decoded_urls() {
    if RETAINED_DECODED_URLS.len() >= RETAINED_DECODED_URLS_MAX {
        let prune_count = RETAINED_DECODED_URLS_MAX / 10;
        let to_remove: Vec<u64> = RETAINED_DECODED_URLS
            .iter()
            .take(prune_count)
            .map(|r| *r)
            .collect();
        for key in to_remove {
            RETAINED_DECODED_URLS.remove(&key);
        }
    }
}

#[inline]
fn mark_retained_decoded_url(url: &str) {
    if !url.is_empty() {
        maybe_prune_retained_decoded_urls();
        RETAINED_DECODED_URLS.insert(url_key(url));
    }
}

fn mark_retained_decoded_media(url: &str, identity: Option<&MediaSourceId>) {
    mark_retained_decoded_url(url);
    if let Some(source_key) = static_source_key(identity) {
        maybe_prune_retained_decoded_urls();
        RETAINED_DECODED_URLS.insert(source_key);
    }
}

fn mark_retained_decoded_urls<T: AsRef<str>>(urls: &[T]) {
    for url in urls {
        mark_retained_decoded_url(url.as_ref());
    }
}

#[inline]
pub(super) fn is_retained_decoded_url_key(key: u64) -> bool {
    RETAINED_DECODED_URLS.contains(&key)
}

#[inline]
fn is_retained_decoded_media(url_hash: u64, request_key: u64) -> bool {
    is_retained_decoded_url_key(url_hash) || is_retained_decoded_url_key(request_key)
}

fn decoded_animation_key(url: &str, identity: Option<&MediaSourceId>) -> u64 {
    identity
        .map(media_source_key)
        .unwrap_or_else(|| url_key(url))
}

fn resolve_decoded_animation_key(key: u64) -> u64 {
    DECODED_ANIMATION_SOURCE_ALIASES
        .get(&key)
        .map(|canonical| *canonical)
        .unwrap_or(key)
}

fn picker_scope_owns_media(url_hash: u64, request_key: u64) -> bool {
    PICKER_TOUCHED_URLS.contains_key(&url_hash) || PICKER_TOUCHED_MEDIA_KEYS.contains(&request_key)
}

fn picker_visibility_contains_media(
    visible_hashes: &std::collections::HashSet<u64>,
    url_hash: u64,
    request_key: u64,
) -> bool {
    if visible_hashes.contains(&url_hash) || visible_hashes.contains(&request_key) {
        return true;
    }
    if request_key == url_hash {
        return false;
    }

    visible_hashes.iter().any(|visible_hash| {
        PICKER_TOUCHED_URLS
            .get(visible_hash)
            .is_some_and(|media| media.request_key() == request_key)
            || resolve_decoded_animation_key(*visible_hash) == request_key
    })
}

fn remove_decoded_animation_aliases_for_canonical(canonical_key: u64) {
    DECODED_ANIMATION_SOURCE_ALIASES.retain(|_, target| *target != canonical_key);
}

fn cache_decoded_animation_source(
    url: &str,
    identity: Option<&MediaSourceId>,
    decoded: crate::image_decode::DecodedAnimationData,
) -> Arc<CachedDecodedAnimation> {
    debug_assert!(decoded.is_complete());
    let source = Arc::new(CachedDecodedAnimation {
        source: Arc::new(super::animation_source::AnimationSource::new(
            decoded.frames.into_boxed_slice(),
        )),
        charged_ram: AtomicUsize::new(0),
        charged_disk: AtomicU64::new(0),
        last_accessed: Mutex::new(Instant::now()),
    });
    let canonical_key = decoded_animation_key(url, identity);
    // Hot frames keep their source to rebuild released pixels, so a source
    // moves to disk even when this cache does not keep it.
    spill_decoded_animation_source(Arc::clone(&source), canonical_key);

    let size_bytes = source.source.ram_bytes();
    let budget = decoded_animation_source_cache_budget_bytes();
    if budget == 0 || size_bytes > budget {
        return source;
    }

    let url_key = url_key(url);
    remove_decoded_animation_aliases_for_canonical(canonical_key);
    if canonical_key != url_key {
        DECODED_ANIMATION_SOURCE_ALIASES.insert(url_key, canonical_key);
    }

    match DECODED_ANIMATION_SOURCES.entry(canonical_key) {
        DashEntry::Occupied(mut entry) => {
            entry.get().uncharge();
            source.charge();
            entry.insert(Arc::clone(&source));
        }
        DashEntry::Vacant(entry) => {
            source.charge();
            entry.insert(Arc::clone(&source));
        }
    }
    evict_decoded_animation_sources_to_budget();
    source
}

/// Move a source's frames to a sidecar file off the async workers, then charge
/// the cache with the much smaller frame table instead.
fn spill_decoded_animation_source(source: Arc<CachedDecodedAnimation>, canonical_key: u64) {
    PRELOAD_RUNTIME.spawn_blocking(move || {
        let Some(sidecar) = source.source.write_sidecar() else {
            return;
        };
        source.source.install_sidecar(sidecar);
        // Re-charge under the entry lock whether the cache inserted this
        // source before or after the swap, so its totals match what it holds.
        let cached = match DECODED_ANIMATION_SOURCES.get_mut(&canonical_key) {
            Some(entry) if Arc::ptr_eq(entry.value(), &source) => {
                source.charge();
                true
            }
            _ => false,
        };
        if cached {
            evict_decoded_animation_sources_to_budget();
        }
    });
}

fn get_decoded_animation_source(
    url: &str,
    identity: Option<&MediaSourceId>,
) -> Option<Arc<CachedDecodedAnimation>> {
    let key = resolve_decoded_animation_key(decoded_animation_key(url, identity));
    let source = DECODED_ANIMATION_SOURCES
        .get(&key)
        .map(|entry| Arc::clone(entry.value()))?;
    *source.last_accessed.lock() = Instant::now();
    Some(source)
}

fn purge_decoded_animation_source(url: &str) -> usize {
    purge_decoded_animation_source_key(url_key(url))
}

fn purge_decoded_animation_source_identity(identity: Option<&MediaSourceId>, url: &str) -> usize {
    purge_decoded_animation_source_key(decoded_animation_key(url, identity))
}

fn purge_decoded_animation_source_key(key: u64) -> usize {
    let canonical_key = resolve_decoded_animation_key(key);
    if canonical_key != key {
        DECODED_ANIMATION_SOURCE_ALIASES.remove(&key);
        return 0;
    }
    DECODED_ANIMATION_SOURCES
        .remove(&canonical_key)
        .map(|(_, source)| {
            remove_decoded_animation_aliases_for_canonical(canonical_key);
            source.uncharge()
        })
        .unwrap_or(0)
}

fn persistent_animation_refill_disk_miss(url_hash: u64) -> bool {
    let at_capacity = ANIMATION_REFILL_DISK_MISS_SINCE.len() >= ANIMATION_REFILL_DISK_MISS_MAX;
    match ANIMATION_REFILL_DISK_MISS_SINCE.entry(url_hash) {
        DashEntry::Occupied(entry) => {
            if entry.get().elapsed() < ANIMATION_REFILL_DISK_MISS_GRACE {
                false
            } else {
                entry.remove();
                true
            }
        }
        DashEntry::Vacant(entry) => {
            if at_capacity {
                true
            } else {
                entry.insert(Instant::now());
                false
            }
        }
    }
}

fn evict_decoded_animation_sources_to_budget() {
    let budget = decoded_animation_source_cache_budget_bytes();
    let disk_budget = decoded_animation_source_disk_budget_bytes();
    let mut used = DECODED_ANIMATION_SOURCE_BYTES.load(Ordering::Relaxed);
    let mut disk_used = DECODED_ANIMATION_SOURCE_DISK_BYTES.load(Ordering::Relaxed);
    if used <= budget && disk_used <= disk_budget {
        return;
    }

    let visible_canonical: std::collections::HashSet<u64> = super::picker_visible_hashes()
        .into_iter()
        .map(resolve_decoded_animation_key)
        .collect();

    let mut sources: Vec<(u64, Instant, usize, u64)> = DECODED_ANIMATION_SOURCES
        .iter()
        .map(|entry| {
            (
                *entry.key(),
                *entry.value().last_accessed.lock(),
                entry.value().charged_ram.load(Ordering::Relaxed),
                entry.value().charged_disk.load(Ordering::Relaxed),
            )
        })
        .collect();
    sources.sort_by_key(|(key, last_accessed, _, _)| {
        (visible_canonical.contains(key), *last_accessed)
    });

    let mut removed = 0usize;
    let mut freed = 0usize;
    for (key, _, ram_bytes, disk_bytes) in sources {
        if used <= budget && disk_used <= disk_budget {
            break;
        }
        if let Some((_, source)) = DECODED_ANIMATION_SOURCES.remove(&key) {
            remove_decoded_animation_aliases_for_canonical(key);
            let ram_freed = source.uncharge();
            freed += ram_freed;
            removed += 1;
        }
        used = used.saturating_sub(ram_bytes);
        disk_used = disk_used.saturating_sub(disk_bytes);
    }

    if removed > 0 {
        crate::log_cache!(
            debug,
            removed,
            freed_bytes = freed,
            budget_bytes = budget,
            "Evicted decoded animation source frames"
        );
    }
}

/// Idle retention for decoded media: decoded frames unused for 3 minutes are
/// freed and reloaded on demand.
const DECODED_MEDIA_IDLE_TTL_SECS: u64 = 180;

/// Idle retention for the CPU-decoded animation *source* cache (zstd-compressed
/// full-frame BGRA of every animated emote touched recently).
///
/// Halved relative to [`DECODED_MEDIA_IDLE_TTL_SECS`]: this is a second
/// representation beside the hot GPU frame window, so animated emotes scrolled
/// off-screen can shed their source frames sooner. Playing animations re-touch
/// their source every refill, so shortening this TTL only expires idle (not
/// visible) sources and carries no playback risk.
const DECODED_ANIMATION_SOURCE_IDLE_TTL_SECS: u64 = 90;

/// Idle retention for an animation source that has moved to a sidecar file.
/// It then costs only its frame table in RAM, and keeping it longer avoids
/// decoding the emote again when it scrolls back into view.
const DECODED_ANIMATION_DISK_SOURCE_IDLE_TTL_SECS: u64 = 600;

/// Free decoded media that has gone unused for [`DECODED_MEDIA_IDLE_TTL_SECS`].
///
/// Budget-based eviction only runs under allocation pressure, so without this
/// sweep the process holds its cache high-water mark forever (e.g. after closing
/// a busy channel or the emote picker). Called from the polling coordinator's
/// 60-second image-cleanup slot.
pub fn sweep_idle_decoded_media() {
    crate::backend::bridge::get_runtime().spawn(super::native_sizes::save());
    let ttl = std::time::Duration::from_secs(DECODED_MEDIA_IDLE_TTL_SECS);
    let source_ttl = std::time::Duration::from_secs(DECODED_ANIMATION_SOURCE_IDLE_TTL_SECS);
    let frame_bytes = FRAME_LAYER.evict_idle_frames(ttl);
    let source_bytes = evict_idle_decoded_animation_sources(source_ttl);
    let static_bytes = MEMORY_LAYER.evict_idle(ttl);
    if frame_bytes > 0 || source_bytes > 0 || static_bytes > 0 {
        crate::log_cache!(
            debug,
            frame_bytes,
            source_bytes,
            static_bytes,
            "Idle decoded media sweep complete"
        );
    }
}

fn evict_idle_decoded_animation_sources(ttl: std::time::Duration) -> usize {
    let now = Instant::now();
    let visible_canonical: std::collections::HashSet<u64> = super::picker_visible_hashes()
        .into_iter()
        .map(resolve_decoded_animation_key)
        .collect();
    let disk_ttl = std::time::Duration::from_secs(DECODED_ANIMATION_DISK_SOURCE_IDLE_TTL_SECS);
    let expired: Vec<u64> = DECODED_ANIMATION_SOURCES
        .iter()
        .filter(|entry| {
            let entry_ttl = if entry.value().charged_disk.load(Ordering::Relaxed) > 0 {
                disk_ttl
            } else {
                ttl
            };
            !visible_canonical.contains(entry.key())
                && now.duration_since(*entry.value().last_accessed.lock()) > entry_ttl
        })
        .map(|entry| *entry.key())
        .collect();

    let mut freed = 0usize;
    for key in expired {
        if let Some((_, source)) = DECODED_ANIMATION_SOURCES.remove(&key) {
            remove_decoded_animation_aliases_for_canonical(key);
            freed += source.uncharge();
        }
    }
    freed
}

fn discard_picker_decoded_url_if_scope_closed(url: &str, source: PreloadSource) -> bool {
    let key = url_key(url);
    if !source.is_picker()
        || PICKER_TOUCHED_URLS.contains_key(&key)
        || is_retained_decoded_url_key(key)
    {
        return false;
    }

    let static_bytes = MEMORY_LAYER.remove_by_url(url).unwrap_or(0);
    let frame_stats = FRAME_LAYER.purge_url(url);
    let decoded_source_bytes = purge_decoded_animation_source(url);
    ANIMATED_STATIC_FALLBACK_URLS.remove(&url_key(url));
    ANIMATED_POSTER_URLS.remove(&url_key(url));
    crate::log_cache!(
        trace,
        url = %url,
        static_bytes,
        frames_removed = frame_stats.frames_removed,
        frame_bytes = frame_stats.frame_bytes,
        decoded_source_bytes,
        "Discarded late picker decode after picker scope closed"
    );
    true
}

#[inline]
fn record_cache_emote_stage_with_source(
    stage: &str,
    source: PreloadSource,
    elapsed: Duration,
    items: usize,
) {
    crate::backend::metrics::record_emote_stage(stage, elapsed, items);
    let source = source.as_str();
    if elapsed.as_millis() >= CACHE_EMOTE_STAGE_WARN_MS {
        crate::log_perf_cache!(
            warn,
            stage = stage,
            source = source,
            elapsed_ms = elapsed.as_millis(),
            items,
            "Slow cache preload stage"
        );
    } else {
        crate::log_perf_cache!(
            debug,
            stage = stage,
            source = source,
            elapsed_ms = elapsed.as_millis(),
            items,
            "Cache preload stage"
        );
    }
}

/// Pending batch for eager preloads
static PENDING_EAGER: LazyLock<Mutex<Vec<PendingPreload>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(100)));

/// Pending batch for lazy preloads
static PENDING_LAZY: LazyLock<Mutex<Vec<PendingPreload>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(100)));

/// Hard cap on each pending queue. When a burst would push a queue past this
/// limit, the oldest (front) entries are dropped — they are re-enqueued by the
/// render path on the next frame anyway.
const PENDING_PRELOAD_MAX: usize = 1_000;

/// Recently enqueued eager-preload URLs to avoid repeated tiny cache-hit batches
/// from hot render paths.
static RECENT_EAGER_PRELOAD_ENQUEUES: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(2048));
/// Recently enqueued lazy-preload URLs to suppress repeated disk-hit batches.
static RECENT_LAZY_PRELOAD_ENQUEUES: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(4096));
/// Recently persisted visible-media touches. Render discovery can revisit the
/// same visible emotes many times per second, while disk retention is measured
/// in days; coalesce repeated request clones and index writes.
static RECENT_DISPLAYED_MEDIA_RECORDS: LazyLock<DashMap<u64, Instant>> =
    LazyLock::new(|| DashMap::with_capacity(4096));
const DISPLAYED_MEDIA_RECORD_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Last flush time for eager preloads
static LAST_EAGER_FLUSH: LazyLock<Mutex<Instant>> = LazyLock::new(|| Mutex::new(Instant::now()));

/// Last flush time for lazy preloads
static LAST_LAZY_FLUSH: LazyLock<Mutex<Instant>> = LazyLock::new(|| Mutex::new(Instant::now()));

/// Whether a flush task is already scheduled for eager preloads
static EAGER_FLUSH_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether a flush task is already scheduled for lazy preloads
static LAZY_FLUSH_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Image loader with unified cache integration
pub struct ImageLoader {
    /// Memory layer reference
    memory: Arc<MemoryLayer>,
    /// Disk layer reference
    disk: Arc<DiskLayer>,
    /// Statistics
    stats: Arc<CacheStats>,
}

/// Batch-shared state moved into each individual media download.
///
/// The batch creates this once and each buffered future clones it, keeping the
/// per-request ownership and reference-counting boundaries explicit without a
/// long positional parameter list.
#[derive(Clone)]
struct PreloadDownloadContext {
    loaded_counter: Arc<AtomicUsize>,
    memory: Arc<MemoryLayer>,
    disk: Arc<DiskLayer>,
    stats: Arc<CacheStats>,
    lazy: bool,
    source: PreloadSource,
}

impl ImageLoader {
    /// Create a new image loader
    pub fn new(memory: Arc<MemoryLayer>, disk: Arc<DiskLayer>, stats: Arc<CacheStats>) -> Self {
        Self {
            memory,
            disk,
            stats,
        }
    }

    pub async fn preload_media_batch(
        &self,
        requests: Vec<MediaRequest>,
        lazy: bool,
        source: PreloadSource,
    ) -> usize {
        use futures_util::stream::{self, StreamExt};

        if requests.is_empty() {
            return 0;
        }

        let mut seen = std::collections::HashSet::new();
        let mut unique_requests = Vec::with_capacity(requests.len());
        for request in requests {
            let key = media_request_key_for_request(&request);
            if seen.insert(key) {
                unique_requests.push((key, request));
            }
        }

        if !lazy && source.retains_decoded_media() {
            for (_, request) in &unique_requests {
                mark_retained_decoded_media(&request.url, request.identity.as_deref());
            }
        }

        let start = Instant::now();
        let total = unique_requests.len();

        if total >= LOG_BATCH_SIZE_THRESHOLD {
            crate::log_cache!(
                info,
                count = total,
                lazy,
                source = source.as_str(),
                "Starting batch preload"
            );
        } else {
            crate::log_cache!(
                trace,
                count = total,
                lazy,
                source = source.as_str(),
                "Starting small batch preload"
            );
        }

        let filter_start = Instant::now();
        let requests_to_load: Vec<(u64, MediaRequest)> = unique_requests
            .into_iter()
            .filter(|(request_key, request)| {
                let url = &request.url;
                let url_key = url_key(url);
                if !lazy && self.memory.contains_url(url) {
                    self.stats.record_hit();
                    return false;
                }
                if !lazy && alias_static_memory_if_resident(url, request.identity.as_deref()) {
                    self.stats.record_hit();
                    return false;
                }
                if lazy && (LOADED_MEDIA_KEYS.contains(request_key) || url_is_loaded(url_key)) {
                    self.stats.record_hit();
                    return false;
                }
                if url_is_loaded(url_key) && self.memory.contains_url(url) {
                    self.stats.record_hit();
                    return false;
                }
                if is_downloading_fresh_keyed(url, *request_key) {
                    return false;
                }
                if let Some(failed_at) = FAILED_URLS.get(&url_key)
                    && failed_at.elapsed().as_secs() < FAILED_URL_COOLDOWN_SECS
                {
                    return false;
                }
                // Claim last and atomically: a concurrent batch that passed the
                // same checks loses the insert and skips this key.
                try_claim_downloading_key(*request_key)
            })
            .collect();
        let filter_elapsed = filter_start.elapsed();

        let to_load = requests_to_load.len();
        record_cache_emote_stage_with_source(
            "cache.preload.filter",
            source,
            filter_elapsed,
            to_load,
        );
        if to_load == 0 {
            crate::log_cache!(
                debug,
                total,
                cached = total,
                source = source.as_str(),
                "All URLs already cached"
            );
            record_cache_emote_stage_with_source(
                "cache.preload.total",
                source,
                start.elapsed(),
                total,
            );
            return total;
        }

        let downloading_keys: Vec<u64> = requests_to_load
            .iter()
            .map(|(request_key, _)| *request_key)
            .collect();

        let per_batch_limit = if lazy {
            lazy_batch_inflight_limit()
        } else {
            eager_batch_inflight_limit()
        };
        let loaded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let download_context = PreloadDownloadContext {
            loaded_counter: Arc::clone(&loaded),
            memory: Arc::clone(&self.memory),
            disk: Arc::clone(&self.disk),
            stats: Arc::clone(&self.stats),
            lazy,
            source,
        };

        let wait_downloads_start = Instant::now();
        let batch_timeout = preload_batch_timeout(lazy, to_load, per_batch_limit);
        let batch_wait_result = tokio::time::timeout(
            batch_timeout,
            stream::iter(requests_to_load.into_iter().map(|(request_key, request)| {
                let download_context = download_context.clone();
                async move {
                    download_url(request_key, request, download_context).await;
                }
            }))
            .buffer_unordered(per_batch_limit)
            .for_each(|_| async {}),
        )
        .await;
        record_cache_emote_stage_with_source(
            "cache.preload.wait_downloads",
            source,
            wait_downloads_start.elapsed(),
            to_load,
        );
        if batch_wait_result.is_err() {
            record_cache_emote_stage_with_source(
                "cache.preload.batch_timeout",
                source,
                batch_timeout,
                to_load,
            );
            crate::log_cache!(
                warn,
                lazy,
                to_load,
                per_batch_limit,
                source = source.as_str(),
                timeout_ms = batch_timeout.as_millis(),
                "Preload batch wait timed out; clearing in-flight markers for batch"
            );
            // Started downloads cleared their own markers when the timeout
            // dropped them; clear only claims still dated before this wait,
            // so a key another batch re-claimed since keeps its marker.
            for request_key in downloading_keys {
                if DOWNLOADING_SINCE
                    .remove_if(&request_key, |_, since| *since <= wait_downloads_start)
                    .is_some()
                {
                    DOWNLOADING.remove(&request_key);
                }
            }
        }
        record_cache_emote_stage_with_source(
            if lazy {
                "cache.preload.batch_inflight.lazy"
            } else {
                "cache.preload.batch_inflight.eager"
            },
            source,
            Duration::ZERO,
            per_batch_limit,
        );
        record_cache_emote_stage_with_source(
            if lazy {
                "cache.preload.batch_inflight.lazy.active_downloads"
            } else {
                "cache.preload.batch_inflight.eager.active_downloads"
            },
            source,
            Duration::ZERO,
            ACTIVE_DOWNLOADS.load(Ordering::Relaxed) as usize,
        );

        let final_loaded = loaded.load(Ordering::Relaxed);
        let elapsed = start.elapsed();

        if total >= LOG_BATCH_SIZE_THRESHOLD {
            crate::log_cache!(
                info,
                loaded = final_loaded,
                total,
                lazy,
                source = source.as_str(),
                elapsed_ms = elapsed.as_millis(),
                rate_per_sec = if elapsed.as_secs_f64() > 0.0 {
                    (final_loaded as f64 / elapsed.as_secs_f64()) as u32
                } else {
                    0
                },
                "Batch preload complete"
            );
        } else {
            crate::log_cache!(
                trace,
                loaded = final_loaded,
                total,
                lazy,
                source = source.as_str(),
                elapsed_ms = elapsed.as_millis(),
                "Small batch complete"
            );
        }
        record_cache_emote_stage_with_source("cache.preload.total", source, elapsed, final_loaded);

        final_loaded
    }
}

#[inline]
fn extract_host_key(url: &str) -> Option<String> {
    let (_, after_scheme) = url.split_once("://")?;
    let host_port = after_scheme.split('/').next().unwrap_or_default();
    let host_port = host_port.split('@').next_back().unwrap_or(host_port);
    let host = host_port.split(':').next().unwrap_or(host_port).trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

#[inline]
fn host_download_limit(host: &str) -> usize {
    if host.ends_with("static-cdn.jtvnw.net") {
        12
    } else if host.ends_with("cdn.7tv.app") {
        10
    } else if host.ends_with("cdn.betterttv.net") || host.ends_with("cdn.frankerfacez.com") {
        8
    } else {
        MAX_CONCURRENT_DOWNLOADS_PER_HOST_DEFAULT
    }
}

#[inline]
fn host_limiter_for(host: &str) -> Arc<Semaphore> {
    if let Some(limiter) = HOST_DOWNLOAD_SEMAPHORES.get(host) {
        return Arc::clone(limiter.value());
    }

    HOST_DOWNLOAD_SEMAPHORES
        .entry(host.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(host_download_limit(host))))
        .clone()
}

#[derive(Clone, Copy)]
struct HostPressureState {
    score: u8,
    updated_at: Instant,
}

#[derive(Clone, Copy)]
enum HostPressureSignal {
    Success,
    Timeout,
    RateLimited,
    ServerError,
    TransientError,
    BytesReadError,
}

#[inline]
fn host_pressure_weight(host: &str, signal: HostPressureSignal) -> u8 {
    let host_bias = if host.ends_with("cdn.7tv.app") {
        2
    } else if host.ends_with("static-cdn.jtvnw.net") {
        1
    } else {
        0
    };

    match signal {
        HostPressureSignal::Success => 0,
        HostPressureSignal::Timeout => 6 + host_bias,
        HostPressureSignal::RateLimited => 7 + host_bias,
        HostPressureSignal::ServerError => 4 + host_bias,
        HostPressureSignal::TransientError => 3 + host_bias,
        HostPressureSignal::BytesReadError => 2 + host_bias,
    }
}

#[inline]
fn decay_host_pressure(state: &mut HostPressureState, now: Instant) {
    let step = Duration::from_secs(HOST_PRESSURE_DECAY_STEP_SECS);
    if now <= state.updated_at || state.score == 0 {
        state.updated_at = now;
        return;
    }
    let elapsed = now.saturating_duration_since(state.updated_at);
    let decay_units = (elapsed.as_secs() / step.as_secs()).min(u8::MAX as u64) as u8;
    if decay_units > 0 {
        state.score = state.score.saturating_sub(decay_units);
    }
    state.updated_at = now;
}

fn apply_host_pressure_signal(
    host: &str,
    signal: HostPressureSignal,
    state: &mut HostPressureState,
    now: Instant,
) {
    decay_host_pressure(state, now);
    match signal {
        HostPressureSignal::Success => {
            state.score = state.score.saturating_sub(2);
        }
        _ => {
            let inc = host_pressure_weight(host, signal);
            state.score = state.score.saturating_add(inc).min(HOST_PRESSURE_MAX_SCORE);
        }
    }
}

fn record_host_pressure(host: &str, signal: HostPressureSignal) {
    let now = Instant::now();
    if let Some(mut entry) = HOST_DOWNLOAD_PRESSURE.get_mut(host) {
        apply_host_pressure_signal(host, signal, entry.value_mut(), now);
        return;
    }

    let mut entry = HOST_DOWNLOAD_PRESSURE
        .entry(host.to_string())
        .or_insert(HostPressureState {
            score: 0,
            updated_at: now,
        });
    apply_host_pressure_signal(host, signal, entry.value_mut(), now);
}

fn host_pressure_score(host: &str) -> u8 {
    let now = Instant::now();
    match HOST_DOWNLOAD_PRESSURE.get_mut(host) {
        Some(mut entry) => {
            decay_host_pressure(entry.value_mut(), now);
            let score = entry.value().score;
            if score == 0 {
                drop(entry);
                HOST_DOWNLOAD_PRESSURE.remove(host);
            }
            score
        }
        _ => 0,
    }
}

fn host_preflight_backpressure_delay(host: &str, attempt: u8) -> Option<Duration> {
    let score = host_pressure_score(host);
    if score < 4 {
        return None;
    }

    let base_ms = if score >= 16 {
        120
    } else if score >= 10 {
        70
    } else {
        30
    };
    let scaled = (base_ms as u64)
        .saturating_mul((attempt as u64).min(3))
        .saturating_add(retry_jitter_ms(host, attempt))
        .min(HOST_BACKPRESSURE_MAX_DELAY_MS);
    Some(Duration::from_millis(scaled))
}

#[inline]
fn retry_jitter_ms(url: &str, attempt: u8) -> u64 {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let url_mix = url
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    (seed.wrapping_add(url_mix).wrapping_add(attempt as u64 * 97)) % 90
}

#[inline]
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.is_server_error()
}

#[inline]
fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

fn adaptive_retry_delay(url: &str, attempt: u8, status: Option<reqwest::StatusCode>) -> Duration {
    let exp = 1u64 << attempt.saturating_sub(1);
    let base = match status.map(|s| s.as_u16()) {
        Some(429) => 420,
        Some(code) if code >= 500 => 260,
        _ => DOWNLOAD_RETRY_BASE_MS,
    };
    let jitter = retry_jitter_ms(url, attempt);
    let delay_ms = base
        .saturating_mul(exp)
        .saturating_add(jitter)
        .min(DOWNLOAD_RETRY_MAX_MS);
    Duration::from_millis(delay_ms)
}

#[inline]
fn eager_batch_inflight_limit() -> usize {
    let active = ACTIVE_DOWNLOADS.load(Ordering::Relaxed) as usize;
    let in_startup_burst =
        PRELOAD_TUNER_STARTED_AT.elapsed() < Duration::from_secs(PRELOAD_STARTUP_BURST_WINDOW_SECS);

    let mut target = if in_startup_burst { 32usize } else { 28usize };
    if active >= 72 {
        target = 18;
    } else if active >= 56 {
        target = target.min(24);
    } else if active >= 40 {
        target = target.min(28);
    }
    let pressure_7tv = host_pressure_score("cdn.7tv.app");
    let pressure_twitch = host_pressure_score("static-cdn.jtvnw.net");
    if pressure_7tv >= 14 || pressure_twitch >= 14 {
        target = target.min(18);
    } else if pressure_7tv >= 8 || pressure_twitch >= 8 {
        target = target.min(24);
    }
    target.clamp(MIN_EAGER_BATCH_INFLIGHT, MAX_EAGER_BATCH_INFLIGHT)
}

#[inline]
fn lazy_batch_inflight_limit() -> usize {
    let active = ACTIVE_DOWNLOADS.load(Ordering::Relaxed) as usize;
    let target = if active >= 64 {
        MIN_LAZY_BATCH_INFLIGHT
    } else if active >= 40 {
        5usize
    } else if active >= 24 {
        6usize
    } else {
        MAX_LAZY_BATCH_INFLIGHT
    };
    let pressure_7tv = host_pressure_score("cdn.7tv.app");
    let pressure_twitch = host_pressure_score("static-cdn.jtvnw.net");
    let target = if pressure_7tv >= 14 || pressure_twitch >= 14 {
        target.min(4)
    } else if pressure_7tv >= 8 || pressure_twitch >= 8 {
        target.min(6)
    } else {
        target
    };
    target.clamp(MIN_LAZY_BATCH_INFLIGHT, MAX_LAZY_BATCH_INFLIGHT)
}

#[inline]
fn preload_batch_timeout(lazy: bool, to_load: usize, per_batch_limit: usize) -> Duration {
    let waves = to_load.saturating_add(per_batch_limit.saturating_sub(1)) / per_batch_limit.max(1);

    let growth_secs = if lazy { 2 } else { 1 };
    let scaled_secs = BATCH_TIMEOUT_SECS.saturating_add((waves as u64 / 4) * growth_secs);
    let max_secs = if lazy {
        LAZY_BATCH_TIMEOUT_MAX_SECS
    } else {
        EAGER_BATCH_TIMEOUT_MAX_SECS
    };
    Duration::from_secs(
        scaled_secs
            .min(max_secs)
            .max(minimum_download_batch_timeout_secs()),
    )
}

struct DownloadingGuard {
    key: u64,
}

impl DownloadingGuard {
    #[inline]
    fn new(key: u64) -> Self {
        Self { key }
    }
}

impl Drop for DownloadingGuard {
    fn drop(&mut self) {
        clear_downloading_key(self.key);
    }
}

fn animated_decode_ownership_keys(url: &str, identity: Option<&MediaSourceId>) -> Vec<u64> {
    let url_hash = url_key(url);
    let request_key = media_request_key(url, identity);
    if request_key == url_hash {
        vec![url_hash]
    } else {
        vec![url_hash, request_key]
    }
}

fn animated_decode_is_in_flight(url: &str, identity: Option<&MediaSourceId>) -> bool {
    animated_decode_ownership_keys(url, identity)
        .into_iter()
        .any(|key| DECODING_ANIMATED.contains_key(&key))
}

fn try_claim_animated_decode(
    url: &str,
    identity: Option<&MediaSourceId>,
) -> Option<DecodingAnimatedGuard> {
    let keys = animated_decode_ownership_keys(url, identity);
    let _claim_guard = DECODING_ANIMATED_CLAIM_LOCK.lock();
    if keys.iter().any(|key| DECODING_ANIMATED.contains_key(key)) {
        return None;
    }

    let token = NEXT_ANIMATED_DECODE_TOKEN.fetch_add(1, Ordering::Relaxed);
    DECODING_ANIMATED_STARTED_AT.insert(token, Instant::now());
    for &key in &keys {
        DECODING_ANIMATED.insert(key, token);
    }
    Some(DecodingAnimatedGuard {
        token,
        keys,
        released: false,
    })
}

fn invalidate_animated_decode(url: &str, identity: Option<&MediaSourceId>) {
    let keys = animated_decode_ownership_keys(url, identity);
    let _claim_guard = DECODING_ANIMATED_CLAIM_LOCK.lock();
    let mut invalidated_tokens = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some((_, token)) = DECODING_ANIMATED.remove(&key) {
            invalidated_tokens.push(token);
        }
    }
    for token in invalidated_tokens {
        if !DECODING_ANIMATED
            .iter()
            .any(|owner| *owner.value() == token)
        {
            DECODING_ANIMATED_STARTED_AT.remove(&token);
        }
    }
}

/// Clears every URL/identity ownership entry on drop without removing entries
/// claimed later by a replacement task.
struct DecodingAnimatedGuard {
    token: u64,
    keys: Vec<u64>,
    released: bool,
}

impl DecodingAnimatedGuard {
    fn is_current(&self) -> bool {
        self.keys.iter().all(|key| {
            DECODING_ANIMATED
                .get(key)
                .is_some_and(|owner| *owner == self.token)
        })
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        let _claim_guard = DECODING_ANIMATED_CLAIM_LOCK.lock();
        for &key in &self.keys {
            DECODING_ANIMATED.remove_if(&key, |_, owner| *owner == self.token);
        }
        DECODING_ANIMATED_STARTED_AT.remove(&self.token);
        self.released = true;
    }
}

impl Drop for DecodingAnimatedGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn picker_animation_decode_is_stale(
    visibility: &super::PickerVisibilitySnapshot,
    url_hash: u64,
    request_key: u64,
) -> bool {
    let picker_scope_still_owns_media = picker_scope_owns_media(url_hash, request_key);
    let picker_media_is_visible =
        picker_visibility_contains_media(&visibility.hashes, url_hash, request_key);
    !picker_scope_still_owns_media || (visibility.published && !picker_media_is_visible)
}

/// Wait for a full-decode permit while keeping exact picker visibility live.
///
/// The semaphore future remains pinned across viewport publications, preserving
/// its FIFO position when the media is still visible. Once it scrolls away, the
/// future is dropped before it can acquire a permit and both URL/identity decode
/// ownership keys are compare-released immediately.
async fn acquire_picker_animated_decode_permit(
    decode_semaphore: Arc<Semaphore>,
    decoding_guard: &mut DecodingAnimatedGuard,
    url_hash: u64,
    request_key: u64,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let mut visibility_changes = super::subscribe_picker_visibility_changes();
    let permit_wait = decode_semaphore.acquire_owned();
    tokio::pin!(permit_wait);

    loop {
        let visibility = super::picker_visibility_snapshot();
        if !decoding_guard.is_current()
            || picker_animation_decode_is_stale(&visibility, url_hash, request_key)
        {
            decoding_guard.release();
            crate::log_cache!(
                trace,
                url_hash,
                "Discarded stale picker animation decode while queued"
            );
            return None;
        }

        let watched_generation = { *visibility_changes.borrow_and_update() };
        if watched_generation != visibility.generation {
            continue;
        }

        tokio::select! {
            biased;
            changed = visibility_changes.changed() => {
                if changed.is_err() {
                    decoding_guard.release();
                    return None;
                }
            }
            permit = permit_wait.as_mut() => {
                let Ok(permit) = permit else {
                    decoding_guard.release();
                    return None;
                };

                let visibility = super::picker_visibility_snapshot();
                if !decoding_guard.is_current()
                    || picker_animation_decode_is_stale(&visibility, url_hash, request_key)
                {
                    drop(permit);
                    decoding_guard.release();
                    crate::log_cache!(
                        trace,
                        url_hash,
                        "Discarded stale picker animation decode at admission"
                    );
                    return None;
                }
                return Some(permit);
            }
        }
    }
}

#[inline]
fn picker_refill_is_stale(visibility: &super::PickerVisibilitySnapshot, url_hash: u64) -> bool {
    !FRAME_LAYER.picker_visibility_contains_canonical_hash(&visibility.hashes, url_hash)
}

/// Wait for a picker-owned disk-refill decode permit while cancelling work as
/// soon as its canonical animation leaves both exact picker viewports.
async fn acquire_picker_refill_decode_permit(
    decode_semaphore: Arc<Semaphore>,
    url_hash: u64,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let mut visibility_changes = super::subscribe_picker_visibility_changes();
    let permit_wait = decode_semaphore.acquire_owned();
    tokio::pin!(permit_wait);

    loop {
        let visibility = super::picker_visibility_snapshot();
        if picker_refill_is_stale(&visibility, url_hash) {
            crate::log_cache!(
                trace,
                url_hash,
                "Discarded stale picker animation refill while queued"
            );
            return None;
        }

        let watched_generation = { *visibility_changes.borrow_and_update() };
        if watched_generation != visibility.generation {
            continue;
        }

        tokio::select! {
            biased;
            changed = visibility_changes.changed() => {
                if changed.is_err() {
                    return None;
                }
            }
            permit = permit_wait.as_mut() => {
                let Ok(permit) = permit else {
                    return None;
                };
                let visibility = super::picker_visibility_snapshot();
                if picker_refill_is_stale(&visibility, url_hash) {
                    drop(permit);
                    crate::log_cache!(
                        trace,
                        url_hash,
                        "Discarded stale picker animation refill at admission"
                    );
                    return None;
                }
                return Some(permit);
            }
        }
    }
}

/// Clears a batch of in-flight prefetch markers on drop so a panic or
/// cancellation mid-decode can't permanently strand these `(url_hash, frame)`
/// entries against `trigger_prefetch`'s in-flight dedup.
struct PrefetchInFlightGuard {
    url_hash: u64,
    frame_indices: SmallVec<[u16; 4]>,
    prefetch_token: u64,
}

impl PrefetchInFlightGuard {
    #[inline]
    fn new(url_hash: u64, frame_indices: SmallVec<[u16; 4]>, prefetch_token: u64) -> Self {
        Self {
            url_hash,
            frame_indices,
            prefetch_token,
        }
    }
}

impl Drop for PrefetchInFlightGuard {
    fn drop(&mut self) {
        super::frame_layer::clear_prefetch_in_flight_batch(
            self.url_hash,
            &self.frame_indices,
            self.prefetch_token,
        );
    }
}

/// Clears the canonical animation refill guard on every task exit, including
/// cancellation or a panic while a blocking source decode is running.
struct PrefetchUrlInFlightGuard {
    url_hash: u64,
    prefetch_token: u64,
}

impl PrefetchUrlInFlightGuard {
    #[inline]
    fn new(url_hash: u64, prefetch_token: u64) -> Self {
        Self {
            url_hash,
            prefetch_token,
        }
    }
}

impl Drop for PrefetchUrlInFlightGuard {
    fn drop(&mut self) {
        super::frame_layer::clear_prefetch_url_in_flight(self.url_hash, self.prefetch_token);
    }
}

struct ActiveDownloadGuard {
    stats: Arc<CacheStats>,
}

impl ActiveDownloadGuard {
    #[inline]
    fn new(stats: Arc<CacheStats>) -> Self {
        ACTIVE_DOWNLOADS.fetch_add(1, Ordering::Relaxed);
        stats.active_downloads.fetch_add(1, Ordering::Relaxed);
        Self { stats }
    }
}

impl Drop for ActiveDownloadGuard {
    fn drop(&mut self) {
        ACTIVE_DOWNLOADS.fetch_sub(1, Ordering::Relaxed);
        self.stats.active_downloads.fetch_sub(1, Ordering::Relaxed);
    }
}

struct NetworkTransferAdmission {
    _host: Option<OwnedSemaphorePermit>,
    _global: OwnedSemaphorePermit,
    _active: ActiveDownloadGuard,
}

async fn acquire_network_transfer(
    url: &str,
    source: Option<PreloadSource>,
    stats: Arc<CacheStats>,
) -> Option<NetworkTransferAdmission> {
    let host_permit = if let Some(host) = extract_host_key(url) {
        let start = Instant::now();
        let permit = host_limiter_for(&host).acquire_owned().await.ok()?;
        if let Some(source) = source {
            record_cache_emote_stage_with_source(
                "cache.preload.wait_host_permit",
                source,
                start.elapsed(),
                1,
            );
        }
        Some(permit)
    } else {
        None
    };
    let start = Instant::now();
    let global_permit = Arc::clone(&DOWNLOAD_SEMAPHORE).acquire_owned().await.ok()?;
    if let Some(source) = source {
        record_cache_emote_stage_with_source(
            "cache.preload.wait_global_permit",
            source,
            start.elapsed(),
            1,
        );
    }
    Some(NetworkTransferAdmission {
        _host: host_permit,
        _global: global_permit,
        _active: ActiveDownloadGuard::new(stats),
    })
}

/// Download a single URL
async fn download_url(
    request_key: u64,
    request: MediaRequest,
    download_context: PreloadDownloadContext,
) {
    let PreloadDownloadContext {
        loaded_counter,
        memory,
        disk,
        stats,
        lazy,
        source,
    } = download_context;
    let url = request.url;
    let identity = request.identity;
    let source_key = static_source_key(identity.as_deref());
    let _downloading_guard = DownloadingGuard::new(request_key);
    if picker_static_request_is_stale(&url, source_key, source) {
        return;
    }
    let body_admission = if lazy {
        None
    } else {
        let Some(admission) = acquire_static_body_admission(source).await else {
            return;
        };
        if picker_static_request_is_stale(&url, source_key, source) {
            return;
        }
        Some(admission)
    };

    let disk_entry = match disk.get_source_entry(identity.as_deref(), &url).await {
        Some(entry) => Some(entry),
        None => disk.get_entry(&url).await,
    };
    if let Some(entry) = disk_entry {
        if lazy {
            mark_url_loaded(url_key(&url));
            maybe_prune_loaded_media_keys();
            LOADED_MEDIA_KEYS.insert(request_key);
            if source.notifies_image_ready(lazy) {
                crate::components::overlays::notify_image_loaded();
            }
            stats.record_hit();
            loaded_counter.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if entry.entry_type == super::disk_layer::EntryType::Static
            && let Some(admission) = body_admission.as_ref()
        {
            let hydration_start = Instant::now();
            let mut hydrated = false;
            let disk_bytes = match disk.load_source_image(identity.as_deref(), &url).await {
                Some(bytes) => Some(bytes),
                None => disk.load_image(&url).await,
            };
            let disk_bytes_len = disk_bytes.as_ref().map_or(0, |bytes| bytes.len());
            if let Some(bytes) = disk_bytes
                && decode_static_into_memory(
                    Arc::clone(&url),
                    source_key,
                    Arc::clone(&memory),
                    AdmittedStaticBytes {
                        bytes,
                        _admission: Arc::clone(admission),
                    },
                    source,
                )
                .await
                    == Some(true)
            {
                let hydration_time = hydration_start.elapsed();
                stats.record_hydration(hydration_time.as_micros() as u64);
                super::warming::record_access(
                    &url,
                    disk_bytes_len.min(u32::MAX as usize) as u32,
                    false,
                );
                crate::log_cache!(trace, url = %url, elapsed_us = hydration_time.as_micros(), "Hydrated from disk");
                hydrated = true;
            }

            if hydrated {
                mark_url_loaded(url_key(&url));
                maybe_prune_loaded_media_keys();
                LOADED_MEDIA_KEYS.insert(request_key);
                if !discard_picker_decoded_url_if_scope_closed(&url, source) {
                    crate::components::overlays::notify_image_loaded();
                }
                stats.record_hit();
                loaded_counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }

    if picker_static_request_is_stale(&url, source_key, source) {
        return;
    }
    if let Some(admission) = body_admission.as_ref()
        && let Some(copy) = static_byte_source(&url)
    {
        if install_static_copy(&url, source_key, &memory, &copy, source, admission).await {
            finish_lower_resolution_load(&url, request_key, source, &loaded_counter);
            return;
        }
        forget_copy(&STATIC_BYTE_SOURCES, &url);
    }

    let host_key = extract_host_key(&url);
    let mut success = false;
    let mut final_failure_status: Option<reqwest::StatusCode> = None;
    let mut final_failure_error: Option<String> = None;
    let mut body_too_large = false;

    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        if picker_static_request_is_stale(&url, source_key, source) {
            return;
        }
        touch_downloading_key(request_key);
        if let Some(host) = host_key.as_deref()
            && let Some(delay) = host_preflight_backpressure_delay(host, attempt)
        {
            record_cache_emote_stage_with_source(
                "cache.preload.host_backpressure_delay",
                source,
                delay,
                1,
            );
            touch_downloading_key(request_key);
            tokio::time::sleep(delay).await;
        }
        let Some(transfer) = acquire_network_transfer(&url, Some(source), Arc::clone(&stats)).await
        else {
            return;
        };
        if picker_static_request_is_stale(&url, source_key, source) {
            return;
        }
        let attempt_start = Instant::now();
        let result = media_client_for(&url).get(url.as_ref()).send().await;
        record_cache_emote_stage_with_source(
            "cache.preload.download.network_attempt",
            source,
            attempt_start.elapsed(),
            1,
        );

        match result {
            Ok(response) if response.status().is_success() => {
                let body = read_media_body(response).await;
                drop(transfer);
                match body {
                    Ok(bytes) => {
                        let len = bytes.len() as u64;

                        spawn_save_source_image(
                            Arc::clone(&disk),
                            identity.clone(),
                            url.clone(),
                            bytes.clone(),
                        );

                        let mut decoded_to_memory = lazy;
                        if let Some(admission) = body_admission.as_ref() {
                            if let Some(inserted) = decode_static_into_memory(
                                Arc::clone(&url),
                                source_key,
                                Arc::clone(&memory),
                                AdmittedStaticBytes {
                                    bytes: Arc::clone(&bytes),
                                    _admission: Arc::clone(admission),
                                },
                                source,
                            )
                            .await
                            {
                                decoded_to_memory = inserted;
                            } else if install_lower_resolution_static(
                                &url, source_key, &memory, source, admission,
                            )
                            .await
                            {
                                decoded_to_memory = true;
                            } else {
                                crate::log_cache!(debug, url = %url, "Downloaded image is undecodable");
                                maybe_prune_failed_urls();
                                FAILED_URLS.insert(url_key(&url), Instant::now());
                            }
                        }

                        if decoded_to_memory {
                            if !lazy {
                                super::warming::record_access(
                                    &url,
                                    len.min(u32::MAX as u64) as u32,
                                    false,
                                );
                            }
                            mark_url_loaded(url_key(&url));
                            maybe_prune_loaded_media_keys();
                            LOADED_MEDIA_KEYS.insert(request_key);
                            if source.notifies_image_ready(lazy)
                                && (lazy
                                    || !discard_picker_decoded_url_if_scope_closed(&url, source))
                            {
                                crate::components::overlays::notify_image_loaded();
                            }
                        }
                        if let Some(host) = host_key.as_deref() {
                            record_host_pressure(host, HostPressureSignal::Success);
                        }
                        stats.record_download(len);
                        loaded_counter.fetch_add(1, Ordering::Relaxed);

                        let n = loaded_counter.load(Ordering::Relaxed);
                        if n.is_multiple_of(500) && n > 0 {
                            crate::log_cache!(debug, loaded = n, "Download progress");
                        }
                        success = true;
                        break;
                    }
                    Err(MediaBodyError::TooLarge) => {
                        body_too_large = true;
                        final_failure_error =
                            Some("media response exceeds the cache entry limit".to_string());
                    }
                    Err(MediaBodyError::Read) => {
                        final_failure_status = None;
                        final_failure_error = Some(
                            "failed to read response bytes for successful response".to_string(),
                        );
                        if let Some(host) = host_key.as_deref() {
                            record_host_pressure(host, HostPressureSignal::BytesReadError);
                        }
                        if attempt < DOWNLOAD_MAX_ATTEMPTS {
                            let delay = adaptive_retry_delay(&url, attempt, None);
                            record_cache_emote_stage_with_source(
                                "cache.preload.download.retry_delay",
                                source,
                                delay,
                                1,
                            );
                            crate::log_cache!(
                                debug,
                                url = %url,
                                attempt,
                                delay_ms = delay.as_millis(),
                                "Retrying download after bytes-read failure"
                            );
                            touch_downloading_key(request_key);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    }
                }
            }
            Ok(response) => {
                let status = response.status();
                drop(response);
                drop(transfer);
                final_failure_status = Some(status);
                if let Some(host) = host_key.as_deref() {
                    let signal = if status.as_u16() == 429 {
                        HostPressureSignal::RateLimited
                    } else if status.is_server_error() {
                        HostPressureSignal::ServerError
                    } else {
                        HostPressureSignal::TransientError
                    };
                    record_host_pressure(host, signal);
                }
                if is_retryable_status(status) && attempt < DOWNLOAD_MAX_ATTEMPTS {
                    let delay = adaptive_retry_delay(&url, attempt, Some(status));
                    record_cache_emote_stage_with_source(
                        "cache.preload.download.retry_delay",
                        source,
                        delay,
                        1,
                    );
                    crate::log_cache!(
                        debug,
                        url = %url,
                        status = %status,
                        attempt,
                        delay_ms = delay.as_millis(),
                        "Retrying download after retryable status"
                    );
                    touch_downloading_key(request_key);
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }
            Err(err) => {
                drop(transfer);
                let retryable = is_retryable_error(&err);
                final_failure_status = None;
                final_failure_error = Some(err.to_string());
                if let Some(host) = host_key.as_deref() {
                    let signal = if err.is_timeout() {
                        HostPressureSignal::Timeout
                    } else {
                        HostPressureSignal::TransientError
                    };
                    record_host_pressure(host, signal);
                }
                if retryable && attempt < DOWNLOAD_MAX_ATTEMPTS {
                    let delay = adaptive_retry_delay(&url, attempt, None);
                    record_cache_emote_stage_with_source(
                        "cache.preload.download.retry_delay",
                        source,
                        delay,
                        1,
                    );
                    crate::log_cache!(
                        debug,
                        url = %url,
                        attempt,
                        delay_ms = delay.as_millis(),
                        error = %err,
                        "Retrying download after transient request error"
                    );
                    touch_downloading_key(request_key);
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }
        }

        break;
    }

    if !success {
        if let Some(status) = final_failure_status {
            crate::log_cache!(
                trace,
                url = %url,
                status = %status,
                "Download failed with final status"
            );
        }
        if let Some(err) = final_failure_error {
            crate::log_cache!(trace, url = %url, error = %err, "Download failed");
        }
        // The last answer refused the image for good (e.g. a missing 4x
        // copy). Transport errors, 429 and 5xx are retried later instead:
        // smaller copies from the same host would fail the same way.
        let server_refused = body_too_large
            || final_failure_status.is_some_and(|status| !is_retryable_status(status));
        if let Some(admission) = body_admission.as_ref()
            && server_refused
            && install_lower_resolution_static(&url, source_key, &memory, source, admission).await
        {
            finish_lower_resolution_load(&url, request_key, source, &loaded_counter);
        } else {
            maybe_prune_failed_urls();
            FAILED_URLS.insert(url_key(&url), Instant::now());
        }
    }
}

/// Smaller copy a still image was loaded from, keyed by the requested URL the
/// server refused, so later loads go straight to the copy.
static STATIC_BYTE_SOURCES: LazyLock<DashMap<u64, Arc<str>>> = LazyLock::new(DashMap::new);

/// Lower-resolution copy an animation was decoded from, keyed by the requested
/// URL whose own copy exceeds the decode limits or was refused. Decodes and
/// refills read this copy instead of the requested one.
static ANIMATION_BYTE_SOURCES: LazyLock<DashMap<u64, Arc<str>>> = LazyLock::new(DashMap::new);
const BYTE_SOURCES_MAX: usize = 4096;

fn static_byte_source(url: &str) -> Option<Arc<str>> {
    STATIC_BYTE_SOURCES
        .get(&url_key(url))
        .map(|entry| Arc::clone(entry.value()))
}

fn animation_byte_source(url: &str) -> Option<Arc<str>> {
    ANIMATION_BYTE_SOURCES
        .get(&url_key(url))
        .map(|entry| Arc::clone(entry.value()))
}

fn remember_copy(sources: &DashMap<u64, Arc<str>>, url: &str, copy: &Arc<str>) {
    if sources.len() >= BYTE_SOURCES_MAX {
        // Own statement: the iterator holds a shard read lock until dropped,
        // and `remove` needs the write lock on that shard.
        let evicted = sources.iter().next().map(|entry| *entry.key());
        if let Some(evicted) = evicted {
            sources.remove(&evicted);
        }
    }
    sources.insert(url_key(url), Arc::clone(copy));
}

fn forget_copy(sources: &DashMap<u64, Arc<str>>, url: &str) {
    sources.remove(&url_key(url));
}

/// Bookkeeping for a requested URL whose image came from a smaller copy,
/// matching a successful eager download.
fn finish_lower_resolution_load(
    url: &str,
    request_key: u64,
    source: PreloadSource,
    loaded_counter: &AtomicUsize,
) {
    mark_url_loaded(url_key(url));
    maybe_prune_loaded_media_keys();
    LOADED_MEDIA_KEYS.insert(request_key);
    if source.notifies_image_ready(false)
        && !discard_picker_decoded_url_if_scope_closed(url, source)
    {
        crate::components::overlays::notify_image_loaded();
    }
    loaded_counter.fetch_add(1, Ordering::Relaxed);
}

/// Store the first smaller copy of a provider emote that loads and decodes
/// under `url`, so a missing or broken large copy does not leave it blank.
async fn install_lower_resolution_static(
    url: &str,
    source_key: Option<u64>,
    memory: &Arc<MemoryLayer>,
    source: PreloadSource,
    admission: &Arc<OwnedSemaphorePermit>,
) -> bool {
    for candidate in echo_emote_core::model::lower_resolution_emote_urls(url) {
        if picker_static_request_is_stale(url, source_key, source) {
            return false;
        }
        let candidate: Arc<str> = Arc::from(candidate);
        let bytes = match load_media_copy(&candidate).await {
            Ok(bytes) => bytes,
            Err(MediaFetchFailure::Refused) => continue,
            Err(MediaFetchFailure::Transient) => return false,
        };
        let Some(inserted) = decode_static_into_memory(
            Arc::from(url),
            source_key,
            Arc::clone(memory),
            AdmittedStaticBytes {
                bytes,
                _admission: Arc::clone(admission),
            },
            source,
        )
        .await
        else {
            continue;
        };
        if !inserted {
            return false;
        }
        remember_copy(&STATIC_BYTE_SOURCES, url, &candidate);
        crate::log_cache!(debug, url = %url, copy = %candidate, "Loaded a lower-resolution copy");
        return true;
    }
    false
}

/// Store a remembered smaller copy under `url`.
async fn install_static_copy(
    url: &str,
    source_key: Option<u64>,
    memory: &Arc<MemoryLayer>,
    copy: &Arc<str>,
    source: PreloadSource,
    admission: &Arc<OwnedSemaphorePermit>,
) -> bool {
    if picker_static_request_is_stale(url, source_key, source) {
        return false;
    }
    let Ok(bytes) = load_media_copy(copy).await else {
        return false;
    };
    decode_static_into_memory(
        Arc::from(url),
        source_key,
        Arc::clone(memory),
        AdmittedStaticBytes {
            bytes,
            _admission: Arc::clone(admission),
        },
        source,
    )
    .await
        == Some(true)
}

/// Bytes of one media copy from the disk cache, else the network (then cached).
async fn load_media_copy(url: &Arc<str>) -> Result<Arc<[u8]>, MediaFetchFailure> {
    if let Some(bytes) = DISK_LAYER.load_image(url).await {
        return Ok(bytes.into());
    }
    let bytes = fetch_media_bytes(url).await?;
    spawn_save_source_image(
        Arc::clone(&DISK_LAYER),
        None::<MediaSourceId>,
        Arc::clone(url),
        Arc::clone(&bytes),
    );
    Ok(bytes)
}

async fn decode_static_into_memory<B>(
    url: Arc<str>,
    source_key: Option<u64>,
    memory: Arc<MemoryLayer>,
    bytes: B,
    source: PreloadSource,
) -> Option<bool>
where
    B: AsRef<[u8]> + Send + 'static,
{
    if picker_static_request_is_stale(&url, source_key, source) {
        return Some(false);
    }
    let lane = if source.priority(false) == 0 {
        &VISIBLE_STATIC_DECODE_SEMAPHORE
    } else {
        &BACKGROUND_STATIC_DECODE_SEMAPHORE
    };
    let permit = Arc::clone(lane).acquire_owned().await.ok()?;
    if picker_static_request_is_stale(&url, source_key, source) {
        return Some(false);
    }
    PRELOAD_RUNTIME
        .spawn_blocking(move || {
            // Hold admission through insertion even if the awaiting batch is cancelled.
            // Returning only the result also bounds decoded images awaiting publication.
            let _permit = permit;
            if picker_static_request_is_stale(&url, source_key, source) {
                return Some(false);
            }
            let image = decode_image(bytes.as_ref())?;
            let inserted = memory.insert_by_url_with_source_key(&url, source_key, image);
            Some(inserted && !discard_picker_decoded_url_if_scope_closed(&url, source))
        })
        .await
        .ok()
        .flatten()
}

/// Decode image bytes to RenderImage
fn decode_image(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    crate::image_decode::decode_to_render_image(bytes)
}

/// Mark `key` as downloading. Returns false when another caller already
/// holds it, so check-and-claim is a single atomic step.
#[inline]
fn try_claim_downloading_key(key: u64) -> bool {
    if !DOWNLOADING.insert(key) {
        return false;
    }
    DOWNLOADING_SINCE.insert(key, Instant::now());
    true
}

#[inline]
fn touch_downloading_key(key: u64) {
    if let Some(mut started_at) = DOWNLOADING_SINCE.get_mut(&key) {
        *started_at = Instant::now();
    }
}

#[inline]
fn clear_downloading_key(key: u64) {
    DOWNLOADING.remove(&key);
    DOWNLOADING_SINCE.remove(&key);
}

#[inline]
fn is_downloading_fresh(url: &str) -> bool {
    is_downloading_fresh_keyed(url, url_key(url))
}

/// `is_downloading_fresh` for a pre-computed URL key.
/// PERF: lets callers that already hashed the URL (e.g. `get_state_and_downloading`)
/// avoid re-hashing. `url` is still needed for the stale-entry warn log.
#[inline]
fn is_downloading_fresh_keyed(url: &str, k: u64) -> bool {
    if !DOWNLOADING.contains(&k) {
        DOWNLOADING_SINCE.remove(&k);
        return false;
    }

    if let Some(started_at) = DOWNLOADING_SINCE.get(&k) {
        if started_at.elapsed().as_secs() <= DOWNLOADING_STALE_TIMEOUT_SECS {
            return true;
        }

        crate::log_cache!(
            warn,
            url = %crate::log::redact_url(url),
            elapsed_s = started_at.elapsed().as_secs(),
            "Clearing stale downloading entry"
        );
    }

    DOWNLOADING.remove(&k);
    DOWNLOADING_SINCE.remove(&k);
    false
}

pub fn trigger_preload_with_source<T>(urls: Vec<T>, source: PreloadSource)
where
    T: AsRef<str> + Into<Arc<str>>,
{
    if urls.is_empty() {
        return;
    }

    if source.retains_decoded_media() {
        mark_retained_decoded_urls(&urls);
    }

    let requested = urls.len();
    let now = Instant::now();
    let mut dedup: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut filtered = Vec::with_capacity(requested.min(PENDING_PRELOAD_MAX));

    for url in urls {
        let key = url_key(url.as_ref());
        if !dedup.insert(key) {
            continue;
        }
        if should_enqueue_eager_preload_request_keyed(url.as_ref(), None, key, key, now) {
            filtered.push(url);
        }
    }

    if filtered.is_empty() {
        return;
    }

    let accepted = filtered.len();
    if source.is_picker() {
        mark_picker_touched_urls(&filtered);
    }

    {
        let mut pending = PENDING_EAGER.lock();
        pending.extend(filtered.into_iter().map(|url| PendingPreload {
            url: url.into(),
            identity: None,
            source,
        }));
        if pending.len() > PENDING_PRELOAD_MAX {
            let overflow = pending.len() - PENDING_PRELOAD_MAX;
            pending.drain(0..overflow);
        }
        crate::log_cache!(
            trace,
            requested,
            accepted,
            queued = pending.len(),
            source = source.as_str(),
            "Filtered eager preload enqueue"
        );
    }

    if !EAGER_FLUSH_SCHEDULED.swap(true, Ordering::SeqCst) {
        PRELOAD_RUNTIME.spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEBOUNCE_INTERVAL_MS)).await;

            flush_eager_batch().await;
        });
    }
}

pub fn trigger_preload_media_with_source(requests: Vec<MediaRequest>, source: PreloadSource) {
    enqueue_media_preload(requests, false, source);
}

pub fn trigger_lazy_preload_media_with_source(requests: Vec<MediaRequest>, source: PreloadSource) {
    enqueue_media_preload(requests, true, source);
}

pub fn trigger_preload_media_single_with_source(
    url: &str,
    identity: Option<MediaSourceId>,
    source: PreloadSource,
) {
    trigger_preload_media_with_source(vec![MediaRequest::new(url, identity)], source);
}

/// Queue static first-frame posters for animated picker entries.
///
/// The ordinary eager loader intentionally decodes GIF/WebP to one
/// `RenderImage`. Marking these URLs as posters lets a later hover/playback
/// request start the full animation even though that static image is resident.
pub fn trigger_animated_poster_preload_media_with_source(
    requests: Vec<MediaRequest>,
    source: PreloadSource,
) {
    for request in &requests {
        if !request.url.is_empty() {
            ANIMATED_POSTER_URLS.insert(url_key(&request.url));
            if source.is_picker() {
                mark_picker_touched_request(request);
            }
        }
    }
    trigger_preload_media_with_source(requests, source);
}

/// Persist animated poster source bytes through the bounded lazy queue while
/// marking them for later first-frame hydration/full-playback coexistence.
pub fn trigger_lazy_animated_poster_preload_media_with_source(
    requests: Vec<MediaRequest>,
    source: PreloadSource,
) {
    for request in &requests {
        if !request.url.is_empty() {
            ANIMATED_POSTER_URLS.insert(url_key(&request.url));
            if source.is_picker() {
                mark_picker_touched_request(request);
            }
        }
    }
    trigger_lazy_preload_media_with_source(requests, source);
}

/// Queue one static first-frame poster for an animated picker entry.
pub fn trigger_animated_poster_preload_media_single_with_source(
    url: &str,
    identity: Option<MediaSourceId>,
    source: PreloadSource,
) {
    if url.is_empty() {
        return;
    }
    ANIMATED_POSTER_URLS.insert(url_key(url));
    if source.is_picker() {
        mark_picker_touched_media(url, identity.as_ref());
    }
    trigger_preload_media_single_with_source(url, identity, source);
}

fn enqueue_media_preload(requests: Vec<MediaRequest>, lazy: bool, source: PreloadSource) {
    if requests.is_empty() {
        return;
    }

    let requested = requests.len();
    let now = Instant::now();
    let mut dedup = std::collections::HashSet::new();
    let mut filtered = Vec::with_capacity(requested.min(PENDING_PRELOAD_MAX));

    for request in requests {
        if request.url.is_empty() {
            continue;
        }
        let key = media_request_key_for_request(&request);
        if !dedup.insert(key) {
            continue;
        }
        let url_hash = request
            .identity
            .as_ref()
            .map_or(key, |_| url_key(&request.url));
        let should_enqueue = if lazy {
            should_enqueue_lazy_preload_request_keyed(
                &request.url,
                request.identity.as_deref(),
                key,
                url_hash,
                now,
            )
        } else {
            should_enqueue_eager_preload_request_keyed(
                &request.url,
                request.identity.as_deref(),
                key,
                url_hash,
                now,
            )
        };
        if should_enqueue {
            filtered.push(request);
        }
    }

    if filtered.is_empty() {
        return;
    }

    let accepted = filtered.len();
    if source.retains_decoded_media() && !lazy {
        for request in &filtered {
            mark_retained_decoded_media(&request.url, request.identity.as_deref());
        }
    }
    if source.is_picker() {
        for request in &filtered {
            mark_picker_touched_request(request);
        }
    }

    let target = if lazy { &PENDING_LAZY } else { &PENDING_EAGER };
    {
        let mut pending = target.lock();
        pending.extend(filtered.into_iter().map(|request| PendingPreload {
            url: request.url,
            identity: request.identity,
            source,
        }));
        if pending.len() > PENDING_PRELOAD_MAX {
            let overflow = pending.len() - PENDING_PRELOAD_MAX;
            pending.drain(0..overflow);
        }
        crate::log_cache!(
            trace,
            requested,
            accepted,
            queued = pending.len(),
            lazy,
            source = source.as_str(),
            "Filtered identity-aware preload enqueue"
        );
    }

    let scheduled = if lazy {
        &LAZY_FLUSH_SCHEDULED
    } else {
        &EAGER_FLUSH_SCHEDULED
    };
    if !scheduled.swap(true, Ordering::SeqCst) {
        PRELOAD_RUNTIME.spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEBOUNCE_INTERVAL_MS)).await;
            if lazy {
                flush_lazy_batch().await;
            } else {
                flush_eager_batch().await;
            }
        });
    }
}

#[inline]
fn should_enqueue_eager_preload_url(url: &str, now: Instant) -> bool {
    should_enqueue_eager_preload_request(url, None, now)
}

#[inline]
fn should_enqueue_eager_preload_request(
    url: &str,
    identity: Option<&MediaSourceId>,
    now: Instant,
) -> bool {
    if url.is_empty() {
        return false;
    }

    let request_key = media_request_key(url, identity);
    let url_hash = identity.map_or(request_key, |_| url_key(url));
    should_enqueue_eager_preload_request_keyed(url, identity, request_key, url_hash, now)
}

#[inline]
fn should_enqueue_eager_preload_request_keyed(
    url: &str,
    identity: Option<&MediaSourceId>,
    request_key: u64,
    url_hash: u64,
    now: Instant,
) -> bool {
    if url.is_empty() {
        return false;
    }

    if MEMORY_LAYER.contains_url(url)
        || alias_static_memory_if_resident_keyed(identity, url_hash)
        || is_downloading_fresh_keyed(url, request_key)
    {
        return false;
    }

    if let Some(failed_at) = FAILED_URLS.get(&url_hash)
        && failed_at.elapsed().as_secs() < FAILED_URL_COOLDOWN_SECS
    {
        return false;
    }

    if let Some(last) = RECENT_EAGER_PRELOAD_ENQUEUES.get(&request_key)
        && now.duration_since(*last) < Duration::from_millis(EAGER_PRELOAD_ENQUEUE_COOLDOWN_MS)
    {
        return false;
    }
    RECENT_EAGER_PRELOAD_ENQUEUES.insert(request_key, now);
    true
}

pub fn trigger_preload_single_with_source(url: &str, source: PreloadSource) {
    if source.retains_decoded_media() {
        mark_retained_decoded_url(url);
    }

    if !should_enqueue_eager_preload_url(url, Instant::now()) {
        return;
    }

    if source.is_picker() {
        mark_picker_touched_url(url);
    }

    {
        let mut pending = PENDING_EAGER.lock();
        if pending.len() >= PENDING_PRELOAD_MAX {
            pending.drain(0..PENDING_PRELOAD_MAX / 10);
        }
        pending.push(PendingPreload {
            url: url.into(),
            identity: None,
            source,
        });
    }

    if !EAGER_FLUSH_SCHEDULED.swap(true, Ordering::SeqCst) {
        PRELOAD_RUNTIME.spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEBOUNCE_INTERVAL_MS)).await;
            flush_eager_batch().await;
        });
    }
}

pub fn trigger_lazy_preload_with_source(urls: Vec<String>, source: PreloadSource) {
    if urls.is_empty() {
        return;
    }

    let requested = urls.len();
    let now = Instant::now();
    let mut dedup: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut filtered = Vec::with_capacity(requested.min(PENDING_PRELOAD_MAX));

    for url in urls {
        let key = url_key(&url);
        if !dedup.insert(key) {
            continue;
        }
        if should_enqueue_lazy_preload_request_keyed(&url, None, key, key, now) {
            filtered.push(url);
        }
    }

    if filtered.is_empty() {
        return;
    }

    let accepted = filtered.len();
    if source.is_picker() {
        mark_picker_touched_urls(&filtered);
    }

    {
        let mut pending = PENDING_LAZY.lock();
        pending.extend(filtered.into_iter().map(|url| PendingPreload {
            url: url.into(),
            identity: None,
            source,
        }));
        if pending.len() > PENDING_PRELOAD_MAX {
            let overflow = pending.len() - PENDING_PRELOAD_MAX;
            pending.drain(0..overflow);
        }
        crate::log_cache!(
            trace,
            requested,
            accepted,
            queued = pending.len(),
            source = source.as_str(),
            "Filtered lazy preload enqueue"
        );
    }

    if !LAZY_FLUSH_SCHEDULED.swap(true, Ordering::SeqCst) {
        PRELOAD_RUNTIME.spawn(async move {
            tokio::time::sleep(Duration::from_millis(DEBOUNCE_INTERVAL_MS)).await;

            flush_lazy_batch().await;
        });
    }
}

#[inline]
fn should_enqueue_lazy_preload_request_keyed(
    url: &str,
    identity: Option<&MediaSourceId>,
    request_key: u64,
    url_hash: u64,
    now: Instant,
) -> bool {
    if url.is_empty() {
        return false;
    }

    if is_downloading_fresh_keyed(url, request_key)
        || LOADED_MEDIA_KEYS.contains(&request_key)
        || url_is_loaded(url_hash)
        || MEMORY_LAYER.contains_url(url)
        || alias_static_memory_if_resident_keyed(identity, url_hash)
    {
        return false;
    }

    if let Some(failed_at) = FAILED_URLS.get(&url_hash)
        && failed_at.elapsed().as_secs() < FAILED_URL_COOLDOWN_SECS
    {
        return false;
    }

    if let Some(last) = RECENT_LAZY_PRELOAD_ENQUEUES.get(&request_key)
        && now.duration_since(*last) < Duration::from_millis(LAZY_PRELOAD_ENQUEUE_COOLDOWN_MS)
    {
        return false;
    }
    RECENT_LAZY_PRELOAD_ENQUEUES.insert(request_key, now);
    true
}

/// Flush pending eager preload batch
async fn flush_eager_batch() {
    let entries: Vec<PendingPreload> = {
        let mut pending = PENDING_EAGER.lock();
        let entries = std::mem::take(&mut *pending);
        *LAST_EAGER_FLUSH.lock() = Instant::now();
        EAGER_FLUSH_SCHEDULED.store(false, Ordering::SeqCst);
        entries
    };

    for (source, requests) in group_pending_by_source(entries, false) {
        preload_media_internal(requests, false, source).await;
    }
}

/// Flush pending lazy preload batch
async fn flush_lazy_batch() {
    let entries: Vec<PendingPreload> = {
        let mut pending = PENDING_LAZY.lock();
        let entries = std::mem::take(&mut *pending);
        *LAST_LAZY_FLUSH.lock() = Instant::now();
        LAZY_FLUSH_SCHEDULED.store(false, Ordering::SeqCst);
        entries
    };

    for (source, requests) in group_pending_by_source(entries, true) {
        preload_media_internal(requests, true, source).await;
    }
}

fn group_pending_by_source(
    entries: Vec<PendingPreload>,
    lazy: bool,
) -> Vec<(PreloadSource, Vec<MediaRequest>)> {
    let mut grouped: std::collections::HashMap<PreloadSource, Vec<MediaRequest>> =
        std::collections::HashMap::new();
    for entry in entries {
        grouped.entry(entry.source).or_default().push(MediaRequest {
            url: entry.url,
            identity: entry.identity,
        });
    }

    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_by_key(|(source, _)| source.priority(lazy));
    grouped
}

async fn preload_media_internal(requests: Vec<MediaRequest>, lazy: bool, source: PreloadSource) {
    let loader = ImageLoader::new(
        Arc::clone(&MEMORY_LAYER),
        Arc::clone(&DISK_LAYER),
        Arc::clone(&STATS),
    );
    loader.preload_media_batch(requests, lazy, source).await;
}

fn schedule_warm_start_eager_hydration(urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }

    let total = urls.len();
    PRELOAD_RUNTIME.spawn(async move {
        let total_start = Instant::now();
        let mut processed = 0usize;
        let mut chunk_index = 0usize;
        let mut urls = urls.into_iter();

        loop {
            let chunk_urls: Vec<_> = urls.by_ref().take(WARM_START_EAGER_BATCH_SIZE).collect();
            if chunk_urls.is_empty() {
                break;
            }
            let chunk_size = chunk_urls.len();
            let chunk_start = Instant::now();
            let hydrated = hydrate_from_disk_only_batch(chunk_urls).await;
            let chunk_elapsed = chunk_start.elapsed();
            processed += chunk_size;
            chunk_index += 1;

            record_cache_emote_stage_with_source(
                "cache.preload.warm_start.batch",
                PreloadSource::StartupWarm,
                chunk_elapsed,
                hydrated,
            );
            crate::log_cache!(
                debug,
                chunk_index,
                chunk_size,
                hydrated,
                processed,
                total,
                elapsed_ms = chunk_elapsed.as_millis(),
                "Warm-start eager hydration chunk complete"
            );

            if processed < total {
                tokio::time::sleep(Duration::from_millis(WARM_START_EAGER_BATCH_DELAY_MS)).await;
            }
        }

        record_cache_emote_stage_with_source(
            "cache.preload.warm_start.total",
            PreloadSource::StartupWarm,
            total_start.elapsed(),
            total,
        );
        crate::log_cache!(
            info,
            total,
            chunks = chunk_index,
            elapsed_ms = total_start.elapsed().as_millis(),
            "Warm-start eager hydration complete"
        );
    });
}

async fn hydrate_from_disk_only_batch(urls: Vec<String>) -> usize {
    use futures_util::stream::{self, StreamExt};

    if urls.is_empty() {
        return 0;
    }

    mark_retained_decoded_urls(&urls);

    let hydrated = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _ = stream::iter(urls.into_iter().map(|url| {
        let hydrated = Arc::clone(&hydrated);
        async move {
            if MEMORY_LAYER.contains_url(&url) {
                hydrated.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let Some(entry) = DISK_LAYER.get_entry(&url).await else {
                return;
            };
            if entry.entry_type != super::disk_layer::EntryType::Static {
                return;
            }
            let Some(admission) = acquire_static_body_admission(PreloadSource::StartupWarm).await
            else {
                return;
            };
            let Some(bytes) = DISK_LAYER.load_image(&url).await else {
                return;
            };
            let bytes_len = bytes.len();
            if let Some(inserted) = decode_static_into_memory(
                Arc::from(url.as_str()),
                None,
                Arc::clone(&MEMORY_LAYER),
                AdmittedStaticBytes {
                    bytes,
                    _admission: admission,
                },
                PreloadSource::StartupWarm,
            )
            .await
            {
                if inserted {
                    mark_url_loaded(url_key(&url));
                    super::warming::record_access(
                        &url,
                        bytes_len.min(u32::MAX as usize) as u32,
                        false,
                    );
                    crate::components::overlays::notify_image_loaded();
                }
                hydrated.fetch_add(1, Ordering::Relaxed);
            }
        }
    }))
    .buffer_unordered(WARM_START_DISK_ONLY_INFLIGHT)
    .for_each(|_| async {})
    .await;

    hydrated.load(Ordering::Relaxed)
}

/// Get cached render image
pub fn get_cached_render_image(url: &str) -> Option<Arc<RenderImage>> {
    MEMORY_LAYER.get_by_url(url)
}

/// Get the static first-frame poster retained for an animated picker entry.
///
/// This is deliberately the static memory layer: it never advances animation
/// phase and never schedules a frame refill.
pub fn get_animated_poster_render_image(url: &str) -> Option<Arc<RenderImage>> {
    if !ANIMATED_POSTER_URLS.contains(&url_key(url)) {
        return None;
    }
    MEMORY_LAYER.get_by_url(url)
}

/// Check if render image is cached
pub fn has_cached_render_image(url: &str) -> bool {
    MEMORY_LAYER.contains_url(url)
}

/// Eagerly download + decode a single URL into the in-memory cache, awaiting
/// completion. Returns true if a decoded image is resident afterward.
///
/// Embed thumbnails render as element-tree `img()` elements, outside the canvas
/// `image_paint_commands` path that the per-split image-load waker tracks. A
/// thumbnail that finishes downloading after its card was laid out therefore
/// never triggers a repaint on its own. Awaiting the decode lets the embed
/// pipeline re-invalidate the message layout once the image is actually
/// drawable, instead of leaving a permanently blank thumbnail in quiet channels.
pub async fn ensure_render_image_cached(url: String, source: PreloadSource) -> bool {
    if url.is_empty() {
        return false;
    }
    let url: Arc<str> = url.into();
    if source == PreloadSource::EmbedThumbnail {
        EMBED_THUMBNAIL_URLS.insert(url_key(&url));
    }
    if MEMORY_LAYER.contains_url(&url) {
        return true;
    }
    if source.retains_decoded_media() {
        mark_retained_decoded_urls(std::slice::from_ref(&url));
    }
    preload_media_internal(
        vec![MediaRequest::new(Arc::clone(&url), None)],
        false,
        source,
    )
    .await;
    if MEMORY_LAYER.contains_url(&url) {
        return true;
    }
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if MEMORY_LAYER.contains_url(&url) {
            return true;
        }
    }
    false
}

/// Check if URL is being downloaded
pub fn is_downloading(url: &str) -> bool {
    is_downloading_fresh(url)
}

/// Drop decoded media that was loaded specifically for an emote picker session.
///
/// The disk cache and URL-known hints stay intact. Closing the picker releases
/// static RenderImages, animated frame textures, animation metadata, and static
/// fallbacks that were requested by picker-visible/cell paths.
pub fn purge_picker_scope() {
    let media: Vec<PickerTouchedMedia> = PICKER_TOUCHED_URLS
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if media.is_empty() {
        PICKER_TOUCHED_MEDIA_KEYS.clear();
        return;
    }
    PICKER_TOUCHED_URLS.clear();
    PICKER_TOUCHED_MEDIA_KEYS.clear();

    let mut static_removed = 0usize;
    let mut static_bytes = 0usize;
    let mut frame_urls_removed = 0usize;
    let mut frames_removed = 0usize;
    let mut frame_bytes = 0usize;
    let mut decoded_source_bytes = 0usize;
    let mut metadata_removed = 0usize;
    let mut retained_skipped = 0usize;

    let mut cleaned_request_keys = std::collections::HashSet::new();
    for touched in media {
        let url = touched.url;
        let key = url_key(&url);
        let request_key = media_request_key(&url, touched.identity.as_deref());
        if is_retained_decoded_media(key, request_key) {
            retained_skipped += 1;
            continue;
        }

        invalidate_animated_decode(&url, touched.identity.as_deref());
        if cleaned_request_keys.insert(request_key) {
            let removed_static =
                if let Some(source_key) = static_source_key(touched.identity.as_deref()) {
                    MEMORY_LAYER
                        .remove_source_key(source_key)
                        .or_else(|| MEMORY_LAYER.remove_by_url(&url))
                } else {
                    MEMORY_LAYER.remove_by_url(&url)
                };
            if let Some(bytes) = removed_static {
                static_removed += 1;
                static_bytes += bytes;
            }

            let frame_stats =
                if let Some(source_key) = static_source_key(touched.identity.as_deref()) {
                    let by_source = FRAME_LAYER.purge_source_key(source_key);
                    if by_source.frames_removed > 0 || by_source.metadata_removed {
                        by_source
                    } else {
                        FRAME_LAYER.purge_url(&url)
                    }
                } else {
                    FRAME_LAYER.purge_url(&url)
                };
            if frame_stats.frames_removed > 0 || frame_stats.metadata_removed {
                frame_urls_removed += 1;
            }
            frames_removed += frame_stats.frames_removed;
            frame_bytes += frame_stats.frame_bytes;
            if frame_stats.metadata_removed {
                metadata_removed += 1;
            }

            decoded_source_bytes += if touched.identity.is_some() {
                purge_decoded_animation_source_identity(touched.identity.as_deref(), &url)
                    + purge_decoded_animation_source(&url)
            } else {
                purge_decoded_animation_source(&url)
            };
        }

        clear_animated_static_fallback(&url, touched.identity.as_deref());
        ANIMATED_POSTER_URLS.remove(&key);
    }

    maybe_trim_process_working_set_for_diagnostic();
    crate::backend::metrics::record_emote_stage(
        "picker.cache_scope.purge",
        Duration::ZERO,
        static_removed + frames_removed + metadata_removed,
    );
    crate::log_cache!(
        info,
        static_removed,
        static_mb = (static_bytes as f64) / (1024.0 * 1024.0),
        frame_urls_removed,
        frames_removed,
        frame_mb = (frame_bytes as f64) / (1024.0 * 1024.0),
        decoded_source_mb = (decoded_source_bytes as f64) / (1024.0 * 1024.0),
        metadata_removed,
        retained_skipped,
        "Picker media cache scope purged"
    );
}

pub async fn invalidate_media_sources(requests: Vec<MediaRequest>) {
    let mut seen = std::collections::HashSet::new();
    for request in requests {
        let key = media_request_key_for_request(&request);
        if seen.insert(key) {
            invalidate_media_source_now(&request.url, into_owned_media_identity(request.identity))
                .await;
        }
    }
}

async fn invalidate_media_source_now(url: &str, identity: Option<MediaSourceId>) {
    let key = url_key(url);
    let request_key = media_request_key(url, identity.as_ref());
    let source_key = static_source_key(identity.as_ref());
    if let Some(source_key) = source_key {
        let _ = MEMORY_LAYER.remove_source_key(source_key);
        let _ = FRAME_LAYER.purge_source_key(source_key);
        let _ = purge_decoded_animation_source_identity(identity.as_ref(), url);
        LOADED_MEDIA_KEYS.remove(&source_key);
    } else {
        let _ = MEMORY_LAYER.remove_by_url(url);
        let _ = FRAME_LAYER.purge_url(url);
        let _ = purge_decoded_animation_source(url);
    }
    forget_url(key);
    FAILED_URLS.remove(&key);
    RETRY_STATES.remove(&key);
    invalidate_animated_decode(url, identity.as_ref());
    DOWNLOADING.remove(&request_key);
    DOWNLOADING_SINCE.remove(&request_key);
    ANIMATED_POSTER_URLS.remove(&key);
    forget_copy(&STATIC_BYTE_SOURCES, url);
    forget_copy(&ANIMATION_BYTE_SOURCES, url);
    clear_animated_static_fallback(url, identity.as_ref());
    DISK_LAYER
        .remove_source_aliases(identity.as_ref(), Some(url))
        .await;
    let identity_label = identity
        .as_ref()
        .map(MediaSourceId::key_string)
        .unwrap_or_else(|| "none".to_string());
    crate::log_cache!(
        debug,
        url = %url,
        identity = %identity_label,
        "Invalidated media source aliases"
    );
}

/// Replace one picker surface's exact visible media set.
///
/// Callers should publish only the measured viewport, not prefetch-buffer rows,
/// and only when that viewport/catalog changes. Cache pressure uses the union of
/// the inline and popout owners to protect currently displayed media without
/// inflating global budgets.
pub fn publish_picker_visible_urls<I, S, A, T>(viewport: PickerViewport, urls: I, animation_urls: A)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    A: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let hashes = urls
        .into_iter()
        .filter_map(|url| {
            let url = url.as_ref();
            (!url.is_empty()).then(|| url_key(url))
        })
        .collect();
    let animation_hashes = animation_urls
        .into_iter()
        .filter_map(|url| {
            let url = url.as_ref();
            (!url.is_empty()).then(|| url_key(url))
        })
        .collect();
    super::replace_picker_visible_hashes(viewport, hashes, animation_hashes);
}

/// Clear one picker surface's visible residency ownership.
pub fn clear_picker_visible_urls(viewport: PickerViewport) {
    super::clear_picker_visible_hashes(viewport);
}

/// Whether this exact URL is currently eligible for picker animation decode
/// and refill (distinct from static poster residency).
pub fn picker_animation_url_is_visible(url: &str) -> bool {
    super::picker_url_hash_is_visible(url_key(url))
}

/// Register the provider identity associated with a picker URL before decode
/// admission. This lets exact-viewport fencing recognize two CDN URLs that
/// refer to the same provider/id/variant even when one request is already in
/// flight and the second therefore never reaches the trigger path.
pub fn register_picker_media_identity(url: &str, identity: Option<&MediaSourceId>) {
    mark_picker_touched_media(url, identity);
}

/// Start an emote-picker media scope.
///
/// While at least one picker is open, decoded image/frame budgets expand so
/// visible picker emotes do not churn through eviction and flash. Closing the
/// final picker purges picker-owned decoded media.
pub fn begin_picker_media_scope() {
    let active_scopes = BUDGET.begin_picker_scope();
    if active_scopes == 1 {
        super::reset_picker_visibility_publication();
    }
    crate::log_cache!(
        debug,
        active_scopes,
        static_budget_mb = BUDGET.budget_for(super::budget::CacheLayer::Static) / (1024 * 1024),
        frame_budget_mb = BUDGET.budget_for(super::budget::CacheLayer::Frames) / (1024 * 1024),
        "Picker media cache scope opened"
    );
}

/// End an emote-picker media scope and purge picker-owned media after the last
/// picker closes.
pub fn close_picker_media_scope() {
    let remaining_scopes = BUDGET.end_picker_scope();
    crate::log_cache!(debug, remaining_scopes, "Picker media cache scope closed");

    if remaining_scopes == 0 {
        super::clear_all_picker_visible_hashes();
        purge_picker_scope();
    }
}

/// Optionally empty Echo's process-wide resident working set after picker purge.
///
/// This is diagnostic-only because Windows trims unrelated live chat, renderer,
/// allocator, and code pages as well as the picker pages just released above.
/// The resulting working-set cliff is not evidence that private commit or GPU
/// residency fell by the same amount, and normal builds must not incur the
/// demand faults needed to repopulate those pages.
#[cfg(target_os = "windows")]
fn maybe_trim_process_working_set_for_diagnostic() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

    let enabled = std::env::var("ECHO_DIAGNOSTIC_TRIM_WORKING_SET_ON_PICKER_CLOSE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }

    // SAFETY: the pseudo-handle from GetCurrentProcess is always valid, and usize::MAX for both sizes requests a trim.
    let result = unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
    match result {
        Ok(()) => crate::log_cache!(
            info,
            "Diagnostic process-wide working-set trim completed after picker purge"
        ),
        Err(error) => crate::log_cache!(
            warn,
            %error,
            "Diagnostic process-wide working-set trim failed after picker purge"
        ),
    }
}

#[cfg(not(target_os = "windows"))]
fn maybe_trim_process_working_set_for_diagnostic() {}

/// Initialize the cache system (call at startup)
pub async fn initialize() -> Result<(), std::io::Error> {
    DISK_LAYER.initialize().await?;

    record_cache_emote_stage_with_source(
        "cache.preload.restore_loaded_urls.skipped",
        PreloadSource::DiskCacheLoad,
        Duration::ZERO,
        0,
    );

    let warm_limit = warm_start_eager_url_limit();
    if warm_limit > 0 {
        let warm_collect_start = Instant::now();
        let disk_warm_urls = DISK_LAYER
            .snapshot_static_urls_by_priority(warm_limit)
            .await;
        let warming_urls = super::warming::top_urls(warm_limit / 2);
        let mut warm_urls = Vec::with_capacity(disk_warm_urls.len() + warming_urls.len());
        let mut seen = std::collections::HashSet::with_capacity(disk_warm_urls.len() * 2 + 64);
        for url in warming_urls.into_iter().chain(disk_warm_urls) {
            if seen.insert(url.clone()) {
                warm_urls.push(url);
            }
            if warm_urls.len() >= warm_limit {
                break;
            }
        }
        record_cache_emote_stage_with_source(
            "cache.preload.warm_start.collect",
            PreloadSource::StartupWarm,
            warm_collect_start.elapsed(),
            warm_urls.len(),
        );
        if !warm_urls.is_empty() {
            schedule_warm_start_eager_hydration(warm_urls);
        }
    } else {
        record_cache_emote_stage_with_source(
            "cache.preload.warm_start.disabled",
            PreloadSource::StartupWarm,
            Duration::ZERO,
            0,
        );
    }

    start_cache_cleanup_task();

    Ok(())
}

/// Background cache cleanup interval (5 minutes)
const CACHE_CLEANUP_INTERVAL_SECS: u64 = 300;
/// JoinHandle for the background cleanup task.
static CACHE_CLEANUP_TASK: LazyLock<Mutex<Option<tokio::task::JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Start background task to periodically clean up stale cache entries
/// Prevents unbounded growth of RETRY_STATES and FAILED_URLS
fn start_cache_cleanup_task() {
    let mut guard = CACHE_CLEANUP_TASK.lock();
    if guard.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }

    *guard = Some(PRELOAD_RUNTIME.spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(CACHE_CLEANUP_INTERVAL_SECS)).await;
            cleanup_stale_cache_entries();
        }
    }));
}

/// Clean up stale entries in RETRY_STATES and FAILED_URLS
fn cleanup_stale_cache_entries() {
    let now = Instant::now();
    MEMORY_LAYER.flush_access_buffer();
    let retry_ttl = Duration::from_secs(RETRY_STATE_TTL_SECS);
    let failed_ttl = Duration::from_secs(FAILED_URL_COOLDOWN_SECS * 2);
    let recent_enqueue_ttl = Duration::from_secs(EAGER_PRELOAD_ENQUEUE_TTL_SECS);
    let recent_lazy_enqueue_ttl = Duration::from_secs(LAZY_PRELOAD_ENQUEUE_TTL_SECS);

    let retry_before = RETRY_STATES.len();
    RETRY_STATES.retain(|_, state| now.duration_since(state.last_retry) < retry_ttl);
    let retry_after = RETRY_STATES.len();

    let failed_before = FAILED_URLS.len();
    FAILED_URLS.retain(|_, timestamp| now.duration_since(*timestamp) < failed_ttl);
    let failed_after = FAILED_URLS.len();

    let recent_before = RECENT_EAGER_PRELOAD_ENQUEUES.len();
    RECENT_EAGER_PRELOAD_ENQUEUES
        .retain(|_, timestamp| now.duration_since(*timestamp) < recent_enqueue_ttl);
    let recent_after = RECENT_EAGER_PRELOAD_ENQUEUES.len();

    let recent_lazy_before = RECENT_LAZY_PRELOAD_ENQUEUES.len();
    RECENT_LAZY_PRELOAD_ENQUEUES
        .retain(|_, timestamp| now.duration_since(*timestamp) < recent_lazy_enqueue_ttl);
    let recent_lazy_after = RECENT_LAZY_PRELOAD_ENQUEUES.len();

    let displayed_before = RECENT_DISPLAYED_MEDIA_RECORDS.len();
    RECENT_DISPLAYED_MEDIA_RECORDS.retain(|_, timestamp| {
        now.duration_since(*timestamp) < DISPLAYED_MEDIA_RECORD_COOLDOWN * 2
    });
    let displayed_after = RECENT_DISPLAYED_MEDIA_RECORDS.len();

    let retry_cleaned = retry_before - retry_after;
    let failed_cleaned = failed_before - failed_after;
    let recent_cleaned = recent_before - recent_after;
    let recent_lazy_cleaned = recent_lazy_before - recent_lazy_after;
    let displayed_cleaned = displayed_before - displayed_after;
    if retry_cleaned > 0
        || failed_cleaned > 0
        || recent_cleaned > 0
        || recent_lazy_cleaned > 0
        || displayed_cleaned > 0
    {
        crate::log_cache!(
            debug,
            retry_cleaned,
            failed_cleaned,
            recent_enqueue_cleaned = recent_cleaned,
            recent_lazy_enqueue_cleaned = recent_lazy_cleaned,
            displayed_media_cleaned = displayed_cleaned,
            retry_remaining = retry_after,
            failed_remaining = failed_after,
            recent_enqueue_remaining = recent_after,
            recent_lazy_enqueue_remaining = recent_lazy_after,
            displayed_media_remaining = displayed_after,
            "Background cache cleanup complete"
        );
    }
}

/// Save cache state (call at shutdown)
pub async fn shutdown() {
    let shutdown_start = Instant::now();
    crate::log_cache!(info, "Cache loader shutdown started");

    let cleanup_start = Instant::now();
    let handle = CACHE_CLEANUP_TASK.lock().take();
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
    }
    crate::log_cache!(
        info,
        elapsed_ms = cleanup_start.elapsed().as_millis(),
        "Cache cleanup worker stopped"
    );

    let disk_start = Instant::now();
    DISK_LAYER.save_index().await;
    DISK_LAYER.save_source_index().await;
    crate::log_cache!(
        info,
        elapsed_ms = disk_start.elapsed().as_millis(),
        "Disk cache indexes saved"
    );

    let warming_start = Instant::now();
    super::warming::save_priorities().await;
    super::native_sizes::save().await;
    crate::log_cache!(
        info,
        elapsed_ms = warming_start.elapsed().as_millis(),
        "Cache warming priorities saved"
    );
    crate::log_cache!(
        info,
        elapsed_ms = shutdown_start.elapsed().as_millis(),
        "Cache loader shutdown complete"
    );
}

/// Trigger emote preload for a channel (matches old emote_loader API)
/// PERF: Bounded to MAX_CONCURRENT_PRELOADS threads to prevent resource exhaustion
pub fn trigger_channel_preload(channel: &str) {
    if channel.is_empty() {
        return;
    }

    let channel = channel.to_string();

    if !PRELOADING_CHANNELS.insert(channel.clone()) {
        crate::log_cache!(trace, channel = %channel, "Channel already being preloaded");
        return;
    }

    let prev = ACTIVE_PRELOAD_THREADS.fetch_add(1, Ordering::AcqRel);
    if prev >= MAX_CONCURRENT_PRELOADS {
        ACTIVE_PRELOAD_THREADS.fetch_sub(1, Ordering::Release);
        crate::log_cache!(debug, channel = %channel, active = prev, "Preload thread limit reached, skipping");
        PRELOADING_CHANNELS.remove(&channel);
        return;
    }

    crate::log_cache!(info, channel = %channel, active = prev + 1, "Preload requested for channel");
    record_cache_emote_stage_with_source(
        "cache.preload.channel.request",
        PreloadSource::Channel,
        Duration::ZERO,
        1,
    );

    let channel_for_cleanup = channel.clone();
    PRELOAD_RUNTIME.spawn(async move {
        do_channel_preload_async(channel).await;
        PRELOADING_CHANNELS.remove(&channel_for_cleanup);
    });
}

/// Batch preload emotes for several channels as one task on the shared preload
/// runtime; at most `MAX_CONCURRENT_PRELOADS` batches run at once.
pub fn trigger_channels_preload_batch(channels: Vec<String>) {
    let channels: Vec<String> = channels
        .into_iter()
        .filter(|ch| !ch.is_empty() && PRELOADING_CHANNELS.insert(ch.clone()))
        .collect();

    if channels.is_empty() {
        return;
    }

    let prev = ACTIVE_PRELOAD_THREADS.fetch_add(1, Ordering::AcqRel);
    if prev >= MAX_CONCURRENT_PRELOADS {
        ACTIVE_PRELOAD_THREADS.fetch_sub(1, Ordering::Release);
        crate::log_cache!(
            debug,
            count = channels.len(),
            active = prev,
            "Batch preload thread limit reached, skipping"
        );
        for ch in &channels {
            PRELOADING_CHANNELS.remove(ch);
        }
        return;
    }

    let channel_count = channels.len();
    crate::log_cache!(
        info,
        count = channel_count,
        active = prev + 1,
        "Batch preload requested for channels"
    );
    record_cache_emote_stage_with_source(
        "cache.preload.batch_channel.request",
        PreloadSource::ChannelBatchEager,
        Duration::ZERO,
        channel_count,
    );

    PRELOAD_RUNTIME.spawn(async move {
        let start = std::time::Instant::now();

        for _ in 0..50 {
            if DISK_LAYER
                .initialized
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let mut eager_requests: Vec<MediaRequest> = Vec::new();
        let mut lazy_requests: Vec<MediaRequest> = Vec::new();

        for channel in &channels {
            if eager_requests.len() + lazy_requests.len() >= MAX_BATCH_PRELOAD_URLS_TOTAL {
                crate::log_cache!(
                    debug,
                    cap = MAX_BATCH_PRELOAD_URLS_TOTAL,
                    "Batch preload URL cap reached before processing all channels"
                );
                break;
            }
            for attempt in 0..10 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }

                let categories = crate::backend::get_categorized_emotes(channel);
                let discovered_count: usize = categories
                    .iter()
                    .map(|category| category.emotes.len())
                    .sum();
                let per_channel_cap = MAX_BATCH_PRELOAD_URLS_PER_CHANNEL.min(
                    BATCH_CHANNEL_EAGER_URLS_PER_CHANNEL + BATCH_CHANNEL_LAZY_URLS_PER_CHANNEL,
                );
                let remaining_budget = MAX_BATCH_PRELOAD_URLS_TOTAL
                    .saturating_sub(eager_requests.len() + lazy_requests.len());
                let queued_count = discovered_count.min(per_channel_cap).min(remaining_budget);
                let requests: Vec<MediaRequest> = categories
                    .iter()
                    .flat_map(|cat| {
                        cat.emotes.iter().map(|e| {
                            let url = e.url_1x.as_deref().unwrap_or(e.url.as_ref());
                            crate::backend::types::media_request_for_url(e, url)
                        })
                    })
                    .take(queued_count)
                    .collect();

                if !requests.is_empty() {
                    let mut requests = requests;
                    if discovered_count > per_channel_cap {
                        crate::log_cache!(
                            debug,
                            channel = %channel,
                            cap = per_channel_cap,
                            "Truncated per-channel batch preload URLs"
                        );
                    }

                    if discovered_count.min(per_channel_cap) > remaining_budget {
                        crate::log_cache!(
                            debug,
                            channel = %channel,
                            cap = MAX_BATCH_PRELOAD_URLS_TOTAL,
                            "Truncated batch preload URLs to global cap budget"
                        );
                    }

                    crate::log_cache!(debug,
                        channel = %channel,
                        count = requests.len(),
                        eager_cap = BATCH_CHANNEL_EAGER_URLS_PER_CHANNEL,
                        "Collected URLs for batch preload"
                    );
                    let lazy_tail = if requests.len() > BATCH_CHANNEL_EAGER_URLS_PER_CHANNEL {
                        requests.split_off(BATCH_CHANNEL_EAGER_URLS_PER_CHANNEL)
                    } else {
                        Vec::new()
                    };
                    eager_requests.extend(requests);
                    lazy_requests.extend(lazy_tail);
                    break;
                }
            }
        }

        let url_count = eager_requests.len() + lazy_requests.len();
        if url_count > 0 {
            crate::log_cache!(
                info,
                channels = channel_count,
                urls = url_count,
                eager = eager_requests.len(),
                lazy = lazy_requests.len(),
                "Starting staged batch emote preload"
            );

            if !eager_requests.is_empty() {
                record_cache_emote_stage_with_source(
                    "cache.preload.batch_channel.eager_phase",
                    PreloadSource::ChannelBatchEager,
                    Duration::ZERO,
                    eager_requests.len(),
                );
                preload_media_internal(eager_requests, false, PreloadSource::ChannelBatchEager)
                    .await;
            }

            if !lazy_requests.is_empty() {
                let lazy_count = lazy_requests.len();
                record_cache_emote_stage_with_source(
                    "cache.preload.batch_channel.lazy_phase",
                    PreloadSource::ChannelBatchLazy,
                    Duration::ZERO,
                    lazy_count,
                );
                PRELOAD_RUNTIME.spawn(async move {
                    let lazy_start = Instant::now();
                    preload_media_internal(lazy_requests, true, PreloadSource::ChannelBatchLazy)
                        .await;
                    record_cache_emote_stage_with_source(
                        "cache.preload.batch_channel.lazy_phase.deferred",
                        PreloadSource::ChannelBatchLazy,
                        lazy_start.elapsed(),
                        lazy_count,
                    );
                });
            }

            let elapsed = start.elapsed();
            crate::log_cache!(
                info,
                channels = channel_count,
                urls = url_count,
                elapsed_ms = elapsed.as_millis(),
                "Batch channel preload complete"
            );
            record_cache_emote_stage_with_source(
                "cache.preload.batch_channel.total",
                PreloadSource::ChannelBatchEager,
                elapsed,
                url_count,
            );
        }

        for ch in &channels {
            PRELOADING_CHANNELS.remove(ch);
        }

        ACTIVE_PRELOAD_THREADS.fetch_sub(1, Ordering::Release);
    });
}

/// Async channel preload implementation (runs on PRELOAD_RUNTIME)
async fn do_channel_preload_async(channel: String) {
    let start = std::time::Instant::now();
    let collect_wait_start = Instant::now();

    for _ in 0..50 {
        if DISK_LAYER
            .initialized
            .load(std::sync::atomic::Ordering::Acquire)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    for attempt in 0..10 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let categories = crate::backend::get_categorized_emotes(&channel);
        crate::log_cache!(debug,
            channel = %channel,
            attempt,
            category_count = categories.len(),
            "Checking emotes for preload"
        );

        let discovered_count = categories.iter().map(|cat| cat.emotes.len()).sum();
        let preload_plan = plan_channel_preload(discovered_count);
        let requests: Vec<MediaRequest> = categories
            .iter()
            .flat_map(|cat| {
                cat.emotes.iter().map(|e| {
                    let url = e.url_1x.as_deref().unwrap_or(e.url.as_ref());
                    crate::backend::types::media_request_for_url(e, url)
                })
            })
            .take(preload_plan.queued)
            .collect();

        if !requests.is_empty() {
            record_cache_emote_stage_with_source(
                "cache.preload.channel.collect_urls",
                PreloadSource::Channel,
                collect_wait_start.elapsed(),
                preload_plan.discovered,
            );
            if preload_plan.truncated > 0 {
                record_cache_emote_stage_with_source(
                    "cache.preload.channel.truncated_urls",
                    PreloadSource::Channel,
                    Duration::ZERO,
                    preload_plan.truncated,
                );
                crate::log_cache!(
                    debug,
                    channel = %channel,
                    discovered = preload_plan.discovered,
                    cap = MAX_CHANNEL_PRELOAD_URLS,
                    "Truncated channel preload URLs"
                );
            }
            crate::log_cache!(info,
                channel = %channel,
                discovered = preload_plan.discovered,
                queued = preload_plan.queued,
                eager_cap = EAGER_CHANNEL_PRELOAD_LIMIT,
                attempt = attempt + 1,
                "Starting staged emote preload"
            );

            if preload_plan.lazy > 0 {
                let mut eager_requests = requests;
                let lazy_requests = eager_requests.split_off(EAGER_CHANNEL_PRELOAD_LIMIT);
                record_cache_emote_stage_with_source(
                    "cache.preload.channel.eager_phase",
                    PreloadSource::Channel,
                    Duration::ZERO,
                    eager_requests.len(),
                );
                preload_media_internal(eager_requests, false, PreloadSource::Channel).await;
                if !lazy_requests.is_empty() {
                    record_cache_emote_stage_with_source(
                        "cache.preload.channel.lazy_phase",
                        PreloadSource::ChannelLazyTail,
                        Duration::ZERO,
                        lazy_requests.len(),
                    );
                    let lazy_channel = channel.clone();
                    let lazy_count = lazy_requests.len();
                    PRELOAD_RUNTIME.spawn(async move {
                        let lazy_start = Instant::now();
                        preload_media_internal(lazy_requests, true, PreloadSource::ChannelLazyTail)
                            .await;
                        record_cache_emote_stage_with_source(
                            "cache.preload.channel.lazy_phase.deferred",
                            PreloadSource::ChannelLazyTail,
                            lazy_start.elapsed(),
                            lazy_count,
                        );
                        crate::log_cache!(
                            debug,
                            channel = %lazy_channel,
                            count = lazy_count,
                            elapsed_ms = lazy_start.elapsed().as_millis(),
                            "Deferred lazy preload tail complete"
                        );
                    });
                }
            } else {
                preload_media_internal(requests, false, PreloadSource::Channel).await;
            }

            let elapsed = start.elapsed();
            crate::log_cache!(info,
                channel = %channel,
                queued = preload_plan.queued,
                discovered = preload_plan.discovered,
                elapsed_ms = elapsed.as_millis(),
                "Channel preload complete"
            );
            record_cache_emote_stage_with_source(
                "cache.preload.channel.total",
                PreloadSource::Channel,
                elapsed,
                preload_plan.queued,
            );

            ACTIVE_PRELOAD_THREADS.fetch_sub(1, Ordering::Release);
            return;
        }
    }

    crate::log_cache!(debug, channel = %channel, "No emotes found to preload after retries");
    record_cache_emote_stage_with_source(
        "cache.preload.channel.collect_urls",
        PreloadSource::Channel,
        collect_wait_start.elapsed(),
        0,
    );
    record_cache_emote_stage_with_source(
        "cache.preload.channel.total",
        PreloadSource::Channel,
        start.elapsed(),
        0,
    );
    ACTIVE_PRELOAD_THREADS.fetch_sub(1, Ordering::Release);
}

/// Get the load state for a URL
pub fn get_state(url: &str) -> ImageLoadState {
    if is_downloading_fresh(url) {
        return ImageLoadState::Loading;
    }

    if MEMORY_LAYER.contains_url(url) {
        return ImageLoadState::Loaded;
    }

    if FAILED_URLS.contains_key(&url_key(url)) {
        return ImageLoadState::Failed;
    }

    if url_is_loaded(url_key(url)) {
        return ImageLoadState::Loading;
    }

    ImageLoadState::Loading
}

/// Get the load state AND the downloading flag for a URL in a single keyed pass.
///
/// PERF: the image-miss paint path computes `url_key` once here and reuses it
/// for every probe instead of hashing the URL in `is_downloading()` and
/// `get_state()` separately.
///
/// The returned `bool` is the freshly-validated downloading flag (same semantics as
/// `is_downloading`). When it is `true` the state is always `Loading`.
pub fn get_state_and_downloading(url: &str) -> (ImageLoadState, bool) {
    let key = url_key(url);

    let downloading = is_downloading_fresh_keyed(url, key);
    if downloading {
        return (ImageLoadState::Loading, true);
    }

    if MEMORY_LAYER.contains(key) {
        return (ImageLoadState::Loaded, false);
    }

    if FAILED_URLS.contains_key(&key) {
        return (ImageLoadState::Failed, false);
    }

    if url_is_loaded(key) {
        return (ImageLoadState::Loading, false);
    }

    (ImageLoadState::Loading, false)
}

/// Check if a failed URL can be retried
pub fn should_retry(url: &str) -> bool {
    if !FAILED_URLS.contains_key(&url_key(url)) {
        return false;
    }

    if let Some(state) = RETRY_STATES.get(&url_key(url))
        && state.last_retry.elapsed() < retry_backoff(state.count.load(Ordering::Relaxed))
    {
        return false;
    }

    true
}

/// When [`retry_failed_media`] will next accept `url`: now for a fresh failure,
/// the end of the backoff after an earlier retry, and `None` when the URL has
/// not failed.
pub fn failed_media_retry_deadline(url: &str) -> Option<Instant> {
    let key = url_key(url);
    let failed_at = *FAILED_URLS.get(&key)?;
    let Some(state) = RETRY_STATES.get(&key) else {
        return Some(failed_at);
    };
    Some(state.last_retry + retry_backoff(state.count.load(Ordering::Relaxed)))
}

/// Atomically claim one retry and schedule it through Echo's media loader.
/// Returns false when another renderer already claimed this failure or its
/// backoff has not elapsed.
///
/// Retries the original provider URL, which preserves identity/cache behavior
/// and reuses the loader's bounded transport, decode, disk, and repaint
/// pipeline.
pub fn retry_failed_media(url: &str, is_animated: bool, source: PreloadSource) -> bool {
    if !should_retry(url) {
        return false;
    }

    let key = url_key(url);
    if FAILED_URLS.remove(&key).is_none() {
        return false;
    }

    if RETRY_STATES.len() >= MAX_RETRY_ENTRIES {
        cleanup_retry_states();
    }

    let now = Instant::now();
    RETRY_STATES
        .entry(key)
        .and_modify(|state| {
            let count = state.count.load(Ordering::Relaxed);
            state
                .count
                .store(count.saturating_add(1), Ordering::Relaxed);
            state.last_retry = now;
        })
        .or_insert_with(|| RetryState {
            count: AtomicU8::new(1),
            last_retry: now,
        });

    if is_animated {
        trigger_animated_decode_with_source(url, source);
    } else {
        RECENT_EAGER_PRELOAD_ENQUEUES.remove(&key);
        trigger_preload_single_with_source(url, source);
    }

    crate::log_cache!(
        debug,
        url = %url,
        animated = is_animated,
        source = source.as_str(),
        "Scheduled failed media retry through Echo loader"
    );
    true
}

/// Memory optimization: Remove old retry states to prevent unbounded growth
fn cleanup_retry_states() {
    let cutoff = Duration::from_secs(RETRY_STATE_TTL_SECS);
    let now = Instant::now();

    let mut to_remove: Vec<u64> = RETRY_STATES
        .iter()
        .filter(|entry| now.duration_since(entry.value().last_retry) > cutoff)
        .map(|entry| *entry.key())
        .collect();
    // Retries no longer end, so expiry alone may free nothing; drop the
    // oldest tenth instead (those URLs are forgotten and load again when next
    // requested).
    if RETRY_STATES.len().saturating_sub(to_remove.len()) >= MAX_RETRY_ENTRIES {
        let mut by_age: Vec<(Instant, u64)> = RETRY_STATES
            .iter()
            .map(|entry| (entry.value().last_retry, *entry.key()))
            .collect();
        by_age.sort_unstable();
        to_remove.extend(
            by_age
                .into_iter()
                .take(MAX_RETRY_ENTRIES / 10)
                .map(|(_, key)| key),
        );
    }

    for key in to_remove {
        RETRY_STATES.remove(&key);
        FAILED_URLS.remove(&key);
    }

    crate::log_cache!(
        debug,
        remaining = RETRY_STATES.len(),
        "Cleaned up old retry states"
    );
}

/// Trigger animated image decode with source attribution for cache scope cleanup.
pub fn trigger_animated_decode_with_source(url: &str, source: PreloadSource) {
    trigger_animated_decode_media_with_source(url, None, source);
}

pub fn trigger_animated_decode_media_with_source(
    url: &str,
    identity: Option<MediaSourceId>,
    source: PreloadSource,
) {
    trigger_animated_decode_request_with_source(MediaRequest::new(url, identity), source);
}

pub fn trigger_animated_decode_request_with_source(request: MediaRequest, source: PreloadSource) {
    let MediaRequest { url, identity } = request;
    let key = media_request_key(&url, identity.as_deref());
    let url_hash = url_key(&url);
    let source_key = static_source_key(identity.as_deref());
    let retains_decoded_media = source.retains_decoded_media();
    let has_confirmed_static_fallback = ANIMATED_STATIC_FALLBACK_URLS.contains(&key)
        || ANIMATED_STATIC_FALLBACK_URLS.contains(&url_hash);
    if retains_decoded_media {
        mark_retained_decoded_media(&url, identity.as_deref());
        FRAME_LAYER.retain_consumer_with_frame_limit(
            &url,
            source_key,
            super::frame_layer::ANIMATION_INITIAL_DECODE_WINDOW_FRAMES as usize,
        );
    }
    if source.is_picker() {
        mark_picker_touched_request(&MediaRequest {
            url: Arc::clone(&url),
            identity: identity.clone(),
        });
    }

    if let Some(source_key) = source_key {
        if FRAME_LAYER.alias_url_to_source_key(&url, source_key) {
            crate::components::overlays::notify_image_loaded();
            return;
        }
        if alias_static_memory_if_resident(&url, identity.as_deref()) {
            crate::components::overlays::notify_image_loaded();
            if has_confirmed_static_fallback {
                return;
            }
        }
    }

    if FRAME_LAYER.has_metadata(&url) {
        return;
    }

    if has_confirmed_static_fallback {
        if !static_fallback_resident(url_hash, source_key) && !is_downloading_fresh_keyed(&url, key)
        {
            if static_fallback_requeue_exhausted(url_hash) {
                crate::log_emotes!(
                    debug,
                    "[ANIMATED] Static fallback never became resident; marking failed url={}",
                    url
                );
                mark_animated_decode_failed(&url, identity.as_deref());
            } else {
                trigger_preload_media_with_source(vec![MediaRequest { url, identity }], source);
            }
        }
        return;
    }

    let Some(decoding_guard) = try_claim_animated_decode(&url, identity.as_deref()) else {
        return;
    };

    crate::log_emotes!(debug, "[ANIMATED] Starting animated decode for url={}", url);

    let source_is_picker = source.is_picker();
    let picker_decode_semaphore = Arc::clone(&ANIMATED_PICKER_DECODE_SEMAPHORE);
    let non_picker_decode_semaphore = Arc::clone(&ANIMATED_NON_PICKER_DECODE_SEMAPHORE);
    PRELOAD_RUNTIME.spawn(async move {
        let mut decoding_guard = decoding_guard;
        let _permit = if source_is_picker {
            let Some(permit) = acquire_picker_animated_decode_permit(
                picker_decode_semaphore,
                &mut decoding_guard,
                url_hash,
                key,
            )
            .await
            else {
                crate::components::overlays::notify_image_loaded();
                return;
            };
            permit
        } else {
            match non_picker_decode_semaphore.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            }
        };
        if !decoding_guard.is_current() {
            decoding_guard.release();
            crate::components::overlays::notify_image_loaded();
            return;
        }

        let picker_visibility = super::picker_visibility_snapshot();
        if source_is_picker && picker_animation_decode_is_stale(&picker_visibility, url_hash, key) {
            crate::log_cache!(
                trace,
                url_hash,
                "Discarded stale picker animation decode before work"
            );
            decoding_guard.release();
            crate::components::overlays::notify_image_loaded();
            return;
        }

        let initial_frame_window = if is_7tv_name_paint_url(&url) {
            NAME_PAINT_ANIMATION_DECODE_WINDOW_FRAMES
        } else if source_is_picker {
            PICKER_ANIMATION_INITIAL_DECODE_WINDOW_FRAMES
        } else {
            super::frame_layer::ANIMATION_INITIAL_DECODE_WINDOW_FRAMES
        };
        decode_animated_url(&url, identity.as_deref(), initial_frame_window, source).await;
        if source_is_picker
            && !picker_scope_owns_media(url_hash, key)
            && !is_retained_decoded_media(url_hash, key)
        {
            let confirmed_static = ANIMATED_STATIC_FALLBACK_URLS.contains(&url_hash)
                || ANIMATED_STATIC_FALLBACK_URLS.contains(&key);
            if let Some(source_key) = static_source_key(identity.as_deref()) {
                if !confirmed_static {
                    let _ = MEMORY_LAYER.remove_source_key(source_key);
                }
                let _ = FRAME_LAYER.purge_source_key(source_key);
                let _ = purge_decoded_animation_source_identity(identity.as_deref(), &url);
                let _ = purge_decoded_animation_source(&url);
            } else {
                if !confirmed_static {
                    let _ = MEMORY_LAYER.remove_by_url(&url);
                }
                let _ = FRAME_LAYER.purge_url(&url);
                let _ = purge_decoded_animation_source(&url);
            }
        }
        decoding_guard.release();
        crate::components::overlays::notify_image_loaded();
    });
}
/// Cache static bytes for an emote that was advertised as animated but decoded
/// as a single-frame image.
async fn cache_animated_static_fallback<B>(
    url: &str,
    identity: Option<&MediaSourceId>,
    source_key: Option<u64>,
    bytes: B,
    source: PreloadSource,
) -> bool
where
    B: AsRef<[u8]> + Send + 'static,
{
    let Some(inserted) = decode_static_into_memory(
        Arc::from(url),
        source_key,
        Arc::clone(&MEMORY_LAYER),
        bytes,
        source,
    )
    .await
    else {
        return false;
    };

    if inserted {
        mark_url_loaded(url_key(url));
        if let Some(source_key) = source_key {
            maybe_prune_loaded_media_keys();
            LOADED_MEDIA_KEYS.insert(source_key);
        }
    }
    mark_animated_static_fallback(url, identity);
    true
}

struct InstalledAnimation {
    frame_count: usize,
    inserted_frames: usize,
    total_duration_ms: u64,
    hot_frame_limit: u16,
    raw_frame_bytes: usize,
    compressed_source_bytes: usize,
}

/// Register animation timing, retain the compact refill source, and materialize
/// the initial hot-frame window: the complete cycle when it is admitted as
/// full-cycle, otherwise the bounded sliding window.
fn install_decoded_animation(
    url: &str,
    identity: Option<&MediaSourceId>,
    decoded: crate::image_decode::DecodedAnimationData,
    initial_frame_window: u16,
) -> InstalledAnimation {
    let frame_count = decoded.frames.len();
    let raw_frame_bytes = decoded.raw_frame_bytes;
    let compressed_source_bytes = decoded.compressed_source_bytes;
    let full_frame_window = frame_count.min(u16::MAX as usize) as u16;
    let (initial_frame_window, sliding_frame_window) = if is_7tv_name_paint_url(url) {
        (
            name_paint_animation_decode_window(frame_count, raw_frame_bytes),
            NAME_PAINT_ANIMATION_DECODE_WINDOW_FRAMES,
        )
    } else if full_frame_window > initial_frame_window
        && FRAME_LAYER.admits_full_cycle(frame_count, raw_frame_bytes)
    {
        (full_frame_window, initial_frame_window)
    } else {
        (initial_frame_window, initial_frame_window)
    };
    debug_assert!(decoded.is_complete());
    let frame_durations: Vec<u32> = decoded.frames.iter().map(|frame| frame.delay_ms).collect();
    let metadata = super::frame_layer::AnimationMetadata::new(frame_durations);
    let total_duration_ms = metadata.total_duration_ms;
    let current_frame = metadata.frame_index_at(super::frame_layer::current_animation_time_ms());
    let source_key = static_source_key(identity);
    if initial_frame_window == full_frame_window && full_frame_window > sliding_frame_window {
        FRAME_LAYER.register_full_cycle_animation_with_source_key(
            url,
            source_key,
            metadata,
            sliding_frame_window as usize,
        );
    } else {
        FRAME_LAYER.register_animation_with_source_key_and_limit(
            url,
            source_key,
            metadata,
            initial_frame_window as usize,
        );
    }
    clear_animated_static_fallback(url, identity);
    clear_animated_decode_failed(url, identity);
    if let Some(source_key) = source_key {
        maybe_prune_loaded_media_keys();
        LOADED_MEDIA_KEYS.insert(source_key);
    }

    let source = cache_decoded_animation_source(url, identity, decoded);
    let inserted_frames =
        insert_decoded_frame_window(url, &source, current_frame, initial_frame_window, None);
    ANIMATION_REFILL_DISK_MISS_SINCE.remove(&super::frame_layer::FrameLayer::hash_url(url));

    InstalledAnimation {
        frame_count,
        inserted_frames,
        total_duration_ms,
        hot_frame_limit: initial_frame_window,
        raw_frame_bytes,
        compressed_source_bytes,
    }
}

fn spawn_save_source_image<I, B>(disk: Arc<DiskLayer>, identity: Option<I>, url: Arc<str>, bytes: B)
where
    I: std::borrow::Borrow<MediaSourceId> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    tokio::spawn(async move {
        let _ = disk
            .save_source_image_owned(identity.as_ref().map(|id| id.borrow()), &url, bytes, 0, 0)
            .await;
    });
}

/// Load and decode one copy of an animation for `url`.
async fn decode_animation_copy(
    copy: &Arc<str>,
) -> Result<(Arc<[u8]>, crate::image_decode::AnimatedDecodeOutcome), MediaFetchFailure> {
    let bytes = load_media_copy(copy).await?;
    tokio::task::spawn_blocking(move || {
        let outcome = crate::image_decode::decode_animated_frames_outcome(&bytes);
        (bytes, outcome)
    })
    .await
    .map_err(|_| MediaFetchFailure::Transient)
}

fn install_animation_from_copy(
    url: &Arc<str>,
    identity: Option<&MediaSourceId>,
    initial_frame_window: u16,
    copy: &Arc<str>,
    decoded: crate::image_decode::DecodedAnimationData,
) {
    remember_copy(&ANIMATION_BYTE_SOURCES, url, copy);
    let installed = install_decoded_animation(url, identity, decoded, initial_frame_window);
    crate::log_emotes!(
        debug,
        "[ANIMATED] Playing lower-resolution copy: url={}, copy={}, frames={}",
        url,
        copy,
        installed.frame_count
    );
}

/// How a walk over an emote's smaller copies ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyLadder {
    /// A copy was installed under the requested URL.
    Installed,
    /// A copy fetch failed transiently; retry later rather than settle for less.
    Transient,
    /// No copy could be used.
    Exhausted,
}

/// Decode the first smaller copy of `url` that plays completely and install
/// it under `url`, so an animation too large at the chosen scale, or one the
/// server refused, still plays. With `still_ok` (the requested copy is
/// unusable), the best copy that cannot animate is shown as a still image.
async fn install_lower_resolution_animation(
    url: &Arc<str>,
    identity: Option<&MediaSourceId>,
    initial_frame_window: u16,
    still_ok: bool,
    source: PreloadSource,
) -> CopyLadder {
    use crate::image_decode::AnimatedDecodeOutcome;

    let mut still: Option<Arc<[u8]>> = None;
    for candidate in echo_emote_core::model::lower_resolution_emote_urls(url) {
        let candidate: Arc<str> = Arc::from(candidate);
        let (bytes, outcome) = match decode_animation_copy(&candidate).await {
            Ok(result) => result,
            Err(MediaFetchFailure::Refused) => continue,
            Err(MediaFetchFailure::Transient) => return CopyLadder::Transient,
        };
        match outcome {
            AnimatedDecodeOutcome::Complete(decoded) => {
                install_animation_from_copy(
                    url,
                    identity,
                    initial_frame_window,
                    &candidate,
                    decoded,
                );
                return CopyLadder::Installed;
            }
            AnimatedDecodeOutcome::Truncated(truncation) => {
                still.get_or_insert(bytes);
                if !truncation.smaller_copy_may_fit() {
                    break;
                }
            }
            AnimatedDecodeOutcome::NotAnimated => {
                if still_ok
                    && cache_animated_static_fallback(
                        url,
                        identity,
                        static_source_key(identity),
                        bytes,
                        source,
                    )
                    .await
                {
                    return CopyLadder::Installed;
                }
            }
        }
    }
    if still_ok
        && let Some(bytes) = still
        && cache_animated_static_fallback(url, identity, static_source_key(identity), bytes, source)
            .await
    {
        CopyLadder::Installed
    } else {
        CopyLadder::Exhausted
    }
}

/// Decode an animated URL from disk cache or network
///
/// CRITICAL FIX: When animated decode fails (image is actually static despite
/// being flagged as animated), we fall back to static decoding. This handles
/// Twitch emotes that have format=["animated"] but serve static PNGs.
async fn decode_animated_url(
    url: &Arc<str>,
    identity: Option<&MediaSourceId>,
    initial_frame_window: u16,
    source: PreloadSource,
) {
    let source_key = static_source_key(identity);

    if let Some(copy) = animation_byte_source(url) {
        match decode_animation_copy(&copy).await {
            Ok((_, crate::image_decode::AnimatedDecodeOutcome::Complete(decoded))) => {
                install_animation_from_copy(url, identity, initial_frame_window, &copy, decoded);
                return;
            }
            // The network is failing: retry later instead of re-fetching the
            // copy or settling for a still frame.
            Err(MediaFetchFailure::Transient) => {
                mark_animated_decode_failed(url, identity);
                return;
            }
            _ => forget_copy(&ANIMATION_BYTE_SOURCES, url),
        }
    }
    // The smaller-copy ladder is walked at most once per decode.
    let mut ladder_walked = false;

    let disk_bytes = match DISK_LAYER.load_source_image(identity, url).await {
        Some(bytes) => Some(bytes),
        None => DISK_LAYER.load_image(url).await,
    };
    if let Some(bytes) = disk_bytes {
        crate::log_emotes!(
            debug,
            "[ANIMATED] Disk cache hit for url={}, {} bytes",
            url,
            bytes.len()
        );
        let (bytes, outcome) = match tokio::task::spawn_blocking(move || {
            let outcome = crate::image_decode::decode_animated_frames_outcome(&bytes);
            (bytes, outcome)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => return,
        };
        let (decoded, shrinkable) = animated_decode_parts(outcome);
        if let Some(decoded) = decoded {
            let installed = install_decoded_animation(url, identity, decoded, initial_frame_window);
            crate::log_emotes!(
                debug,
                "[ANIMATED] Decode SUCCESS: url={}, frames={}, duration_ms={}, hot_frame_limit={}, raw_bytes={}, source_bytes={}",
                url,
                installed.frame_count,
                installed.total_duration_ms,
                installed.hot_frame_limit,
                installed.raw_frame_bytes,
                installed.compressed_source_bytes
            );
            crate::log_cache!(
                trace,
                url = %url,
                frame_count = installed.frame_count,
                inserted_frames = installed.inserted_frames,
                raw_frame_bytes = installed.raw_frame_bytes,
                compressed_source_bytes = installed.compressed_source_bytes,
                "Decoded animated image from cache"
            );
            return;
        } else {
            if shrinkable {
                ladder_walked = true;
                match install_lower_resolution_animation(
                    url,
                    identity,
                    initial_frame_window,
                    false,
                    source,
                )
                .await
                {
                    CopyLadder::Installed => return,
                    CopyLadder::Transient => {
                        mark_animated_decode_failed(url, identity);
                        return;
                    }
                    CopyLadder::Exhausted => {}
                }
            }
            let is_animated_container = bytes.starts_with(b"GIF8")
                || (bytes.len() >= 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP");
            if cache_animated_static_fallback(url, identity, source_key, bytes, source).await {
                crate::log_emotes!(
                    debug,
                    "[ANIMATED] No multi-frame decode; using static fallback: url={}",
                    url
                );
                crate::log_cache!(trace, url = %url, "Animated decode failed, fell back to static from cache");
                return;
            }

            if is_animated_container {
                crate::log_emotes!(
                    warn,
                    "[ANIMATED] Animation and static fallback decode FAILED: url={}",
                    crate::log::redact_url(url)
                );
            } else {
                crate::log_emotes!(
                    debug,
                    "[ANIMATED] Static format in animated cache slot failed to decode: url={}",
                    url
                );
            }
        }
    }

    if url.contains("static-cdn.jtvnw.net/emoticons/v2/") && url.contains("/default/") {
        let animated_url = url.replace("/default/", "/animated/");
        crate::log_emotes!(
            debug,
            "[ANIMATED] Twitch CDN: trying animated variant url={}",
            animated_url
        );
        if let Ok(response) = HTTP_CLIENT.get(&animated_url).send().await {
            if response.status().is_success() {
                if let Ok(bytes) = read_media_body(response).await {
                    spawn_save_source_image(
                        Arc::clone(&DISK_LAYER),
                        identity.cloned(),
                        Arc::clone(url),
                        bytes.clone(),
                    );

                    let decoded = match tokio::task::spawn_blocking(move || {
                        crate::image_decode::decode_animated_frames(bytes.as_ref())
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => return,
                    };
                    if let Some(decoded) = decoded {
                        let installed =
                            install_decoded_animation(url, identity, decoded, initial_frame_window);
                        crate::log_emotes!(
                            debug,
                            "[ANIMATED] Twitch CDN animated variant SUCCESS: url={}, frames={}, duration_ms={}, hot_frame_limit={}, inserted={}, raw_bytes={}, source_bytes={}",
                            url,
                            installed.frame_count,
                            installed.total_duration_ms,
                            installed.hot_frame_limit,
                            installed.inserted_frames,
                            installed.raw_frame_bytes,
                            installed.compressed_source_bytes
                        );
                        crate::log_cache!(
                            debug,
                            url = %url,
                            frame_count = installed.frame_count,
                            inserted_frames = installed.inserted_frames,
                            raw_frame_bytes = installed.raw_frame_bytes,
                            compressed_source_bytes = installed.compressed_source_bytes,
                            "Twitch animated variant decoded"
                        );
                        return;
                    }
                    crate::log_emotes!(
                        debug,
                        "[ANIMATED] Twitch CDN animated variant decode failed, trying /default/"
                    );
                }
            } else {
                crate::log_emotes!(
                    debug,
                    "[ANIMATED] Twitch CDN animated variant 404 (not animated), trying /default/"
                );
            }
        }
    }

    let fetched = fetch_media_bytes(url).await;
    if matches!(fetched, Err(MediaFetchFailure::Transient)) {
        mark_animated_decode_failed(url, identity);
        return;
    }
    if let Ok(bytes) = fetched {
        spawn_save_source_image(
            Arc::clone(&DISK_LAYER),
            identity.cloned(),
            Arc::clone(url),
            bytes.clone(),
        );

        let (bytes, outcome) = match tokio::task::spawn_blocking(move || {
            let outcome = crate::image_decode::decode_animated_frames_outcome(bytes.as_ref());
            (bytes, outcome)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => return,
        };
        let (decoded, shrinkable) = animated_decode_parts(outcome);
        if let Some(decoded) = decoded {
            let installed = install_decoded_animation(url, identity, decoded, initial_frame_window);
            crate::log_emotes!(
                debug,
                "[ANIMATED] Network decode SUCCESS: url={}, frames={}, duration_ms={}, hot_frame_limit={}, inserted={}, raw_bytes={}, source_bytes={}",
                url,
                installed.frame_count,
                installed.total_duration_ms,
                installed.hot_frame_limit,
                installed.inserted_frames,
                installed.raw_frame_bytes,
                installed.compressed_source_bytes
            );
            crate::log_cache!(
                debug,
                url = %url,
                frame_count = installed.frame_count,
                inserted_frames = installed.inserted_frames,
                raw_frame_bytes = installed.raw_frame_bytes,
                compressed_source_bytes = installed.compressed_source_bytes,
                "Downloaded and decoded animated image"
            );
            return;
        }
        if shrinkable && !ladder_walked {
            ladder_walked = true;
            match install_lower_resolution_animation(
                url,
                identity,
                initial_frame_window,
                false,
                source,
            )
            .await
            {
                CopyLadder::Installed => return,
                CopyLadder::Transient => {
                    mark_animated_decode_failed(url, identity);
                    return;
                }
                CopyLadder::Exhausted => {}
            }
        }
        if cache_animated_static_fallback(url, identity, source_key, bytes, source).await {
            crate::log_cache!(debug, url = %url, "Animated decode failed, fell back to static from download");
            return;
        }
    }
    // Refused or unusable at the requested size: any smaller copy, even a
    // still one, beats a blank emote.
    if !ladder_walked
        && install_lower_resolution_animation(url, identity, initial_frame_window, true, source)
            .await
            == CopyLadder::Installed
    {
        return;
    }
    mark_animated_decode_failed(url, identity);
}

/// The playable animation of a decode, and whether a smaller copy could play
/// when this one stopped at a size limit.
fn animated_decode_parts(
    outcome: crate::image_decode::AnimatedDecodeOutcome,
) -> (Option<crate::image_decode::DecodedAnimationData>, bool) {
    match outcome {
        crate::image_decode::AnimatedDecodeOutcome::Complete(decoded) => (Some(decoded), false),
        crate::image_decode::AnimatedDecodeOutcome::Truncated(truncation) => {
            (None, truncation.smaller_copy_may_fit())
        }
        crate::image_decode::AnimatedDecodeOutcome::NotAnimated => (None, false),
    }
}

fn insert_decoded_frame_window(
    url: &str,
    source: &CachedDecodedAnimation,
    start_frame: u16,
    window: u16,
    prefetch_token: Option<u64>,
) -> usize {
    let frame_count = source.source.frame_count().min(u16::MAX as usize) as u16;
    if frame_count == 0 {
        return 0;
    }

    let start_frame = start_frame % frame_count;
    let window = window.min(frame_count);
    let mut wanted = Vec::with_capacity(window as usize);
    for offset in 0..window {
        wanted.push((start_frame + offset) % frame_count);
    }

    let mut inserted = 0usize;
    for idx in wanted {
        if prefetch_token.is_some_and(|token| {
            !super::frame_layer::prefetch_token_is_current(url_key(url), token)
        }) {
            break;
        }
        let Some((data, width, height)) = source.source.read_frame(idx as usize) else {
            continue;
        };
        ANIMATION_FRAME_DECOMPRESSIONS.fetch_add(1, Ordering::Relaxed);
        // Hot frames live only in GPU atlases once shown; the source rebuilds
        // them for another window, a lost device, or the CPU paint compositor.
        let pixel_source = frame_pixel_source(&source.source, idx);
        let inserted_frame = match prefetch_token {
            Some(token) => FRAME_LAYER.insert_prefetched_releasable_frame(
                url,
                idx,
                data,
                width,
                height,
                token,
                pixel_source,
            ),
            None => {
                FRAME_LAYER.insert_releasable_frame(url, idx, data, width, height, pixel_source);
                true
            }
        };
        if !inserted_frame {
            break;
        }
        inserted += 1;
    }
    if prefetch_token.is_some() && inserted > 0 {
        ANIMATION_REFILL_WINDOWS.fetch_add(1, Ordering::Relaxed);
        ANIMATION_REFILL_FRAMES.fetch_add(inserted as u64, Ordering::Relaxed);
    }
    inserted
}

/// Rebuilds a hot frame's BGRA pixels from its animation source after the
/// frame released them, for a window or device whose atlas lacks the frame.
fn frame_pixel_source(
    source: &Arc<super::animation_source::AnimationSource>,
    index: u16,
) -> gpui::RenderImagePixelSource {
    let source = Arc::clone(source);
    Arc::new(move || {
        ANIMATION_FRAME_REBUILDS.fetch_add(1, Ordering::Relaxed);
        source.read_frame(index as usize).map(|(data, _, _)| data)
    })
}

/// Get animated cache state using the same provider/id/variant identity key as
/// animated decode admission.
pub fn get_animated_cache_state_media(
    url: &str,
    identity: Option<&MediaSourceId>,
) -> AnimatedCacheState {
    let url_hash = url_key(url);
    let request_key = media_request_key(url, identity);

    if FRAME_LAYER.has_metadata_by_hash(url_hash) {
        if FRAME_LAYER.has_renderable_frame_by_hash(url_hash) {
            return AnimatedCacheState::Ready;
        }
        return AnimatedCacheState::Loading;
    }

    if animated_decode_is_in_flight(url, identity) {
        return AnimatedCacheState::Loading;
    }

    if ANIMATED_STATIC_FALLBACK_URLS.contains(&request_key)
        || ANIMATED_STATIC_FALLBACK_URLS.contains(&url_hash)
    {
        if !static_fallback_resident(url_hash, static_source_key(identity))
            && !FAILED_URLS.contains_key(&url_hash)
            && !FAILED_URLS.contains_key(&request_key)
        {
            return AnimatedCacheState::Loading;
        }
        return AnimatedCacheState::Failed;
    }

    if FAILED_URLS.contains_key(&url_hash) || FAILED_URLS.contains_key(&request_key) {
        return AnimatedCacheState::Failed;
    }

    AnimatedCacheState::NotLoaded
}

/// Elapsed time for the URL/provider-identity full decode currently holding
/// admission ownership. Picker prefetch uses this to add a smaller source
/// variant only when an unusually expensive primary decode has made no
/// progress for a bounded interval.
pub fn animated_decode_loading_age_ms_media(
    url: &str,
    identity: Option<&MediaSourceId>,
) -> Option<u128> {
    let token = animated_decode_ownership_keys(url, identity)
        .into_iter()
        .find_map(|key| DECODING_ANIMATED.get(&key).map(|owner| *owner))?;
    DECODING_ANIMATED_STARTED_AT
        .get(&token)
        .map(|started| started.elapsed().as_millis())
}

/// Get animated cache state for a URL without a stable media identity.
pub fn get_animated_cache_state(url: &str) -> AnimatedCacheState {
    get_animated_cache_state_media(url, None)
}

/// Revision used by picker viewport publication to refresh active animation
/// variants when a compact source becomes a terminal static fallback.
pub fn animated_cache_state_revision() -> u64 {
    ANIMATED_STATE_REVISION.load(Ordering::Acquire)
}

/// Get current render image for an animated URL
pub fn get_current_animated_render_image(url: &str) -> Option<Arc<RenderImage>> {
    if !crate::emote_renderer::animation_enabled() {
        return FRAME_LAYER.get_stable_frame(url);
    }
    get_current_animated_render_image_with_metadata_and_frame(url).map(|(image, _, _)| image)
}

/// Get a stable resident animation frame without advancing phase or scheduling
/// a refill. Used by non-playing picker policies as a bounded fallback when a
/// dedicated static poster is not yet available.
pub fn get_stable_animated_render_image(url: &str) -> Option<Arc<RenderImage>> {
    FRAME_LAYER.get_stable_frame(url)
}

/// Get current render image AND animation metadata together, hashing the URL once.
///
/// PERF: Use this instead of the two-call pattern (get_current_animated_render_image
/// + get_animation_metadata) to avoid a duplicate URL hash + DashMap lookup.
pub fn get_current_animated_render_image_with_metadata(
    url: &str,
) -> Option<(Arc<RenderImage>, Arc<super::frame_layer::AnimationMetadata>)> {
    if !crate::emote_renderer::animation_enabled() {
        return FRAME_LAYER
            .get_stable_frame(url)
            .zip(FRAME_LAYER.get_metadata(url));
    }
    get_current_animated_render_image_with_metadata_and_frame(url)
        .map(|(image, metadata, _)| (image, metadata))
}

/// Get the displayed animated image, its metadata, and the actual resident
/// frame index selected by the cache (including bounded fallback selection).
pub fn get_current_animated_render_image_with_metadata_and_frame(
    url: &str,
) -> Option<(
    Arc<RenderImage>,
    Arc<super::frame_layer::AnimationMetadata>,
    u16,
)> {
    if !crate::emote_renderer::animation_enabled() {
        return FRAME_LAYER.get_stable_frame_with_metadata_and_index(url);
    }
    FRAME_LAYER.get_current_frame_with_metadata_and_frame(url)
}

/// Get the displayed animated frame and report whether it is the exact
/// wall-clock target rather than a held resident fallback.
pub fn get_current_animated_render_image_with_metadata_frame_and_exactness(
    url: &str,
) -> Option<(
    Arc<RenderImage>,
    Arc<super::frame_layer::AnimationMetadata>,
    u16,
    bool,
)> {
    if !crate::emote_renderer::animation_enabled() {
        return FRAME_LAYER
            .get_stable_frame_with_metadata_and_index(url)
            .map(|(image, metadata, frame_idx)| (image, metadata, frame_idx, false));
    }
    FRAME_LAYER.get_current_frame_with_metadata_frame_and_exactness(url)
}

/// Read one resident animation frame without admitting/scheduling refill work.
/// Picker source handoff uses this strictly as a visual fallback while a single
/// authoritative URL owns decode eligibility.
pub fn get_resident_animated_render_image_with_metadata_frame_and_exactness(
    url: &str,
) -> Option<(
    Arc<RenderImage>,
    Arc<super::frame_layer::AnimationMetadata>,
    u16,
    bool,
)> {
    if !crate::emote_renderer::animation_enabled() {
        return FRAME_LAYER
            .get_stable_frame_with_metadata_and_index(url)
            .map(|(image, metadata, frame_idx)| (image, metadata, frame_idx, false));
    }
    FRAME_LAYER.get_resident_frame_with_metadata_frame_and_exactness(url)
}

/// Check whether a decoded animated frame is resident without triggering prefetch.
pub fn has_animated_frame(url: &str, frame_idx: u16) -> bool {
    FRAME_LAYER.has_frame(url, frame_idx)
}

/// Check if there are any VISIBLE animations currently being rendered
///
/// This is the key function for idle CPU optimization - it only returns true
/// when animated emotes are actually on screen, not just loaded in cache.
/// Used by FrameRateController to determine if animation ticks are needed.
pub fn has_visible_animations() -> bool {
    FRAME_LAYER.has_visible_animations()
}

/// Get animation metadata for a URL (if registered)
///
/// Returns the AnimationMetadata containing frame count, durations, and dimensions.
/// This is used by DynamicTickScheduler to calculate optimal tick intervals.
/// PERF: Returns Arc to avoid cloning Vec<u16> frame durations per-emote per-frame
pub fn get_animation_metadata(url: &str) -> Option<Arc<super::frame_layer::AnimationMetadata>> {
    FRAME_LAYER.get_metadata(url)
}

/// Prefetch a window of animated frames.
///
/// Uses the bounded CPU-decoded source cache when possible and falls back to
/// one disk decode when the source has been evicted. This is called by
/// FrameLayer::trigger_prefetch() when upcoming frames aren't in the hot cache.
pub fn prefetch_animated_frames(
    url: String,
    start_frame: u16,
    animation_frame_count: u16,
    frame_indices: SmallVec<[u16; 4]>,
    prefetch_token: u64,
    picker_visible_at_trigger: bool,
) {
    let url_hash = super::frame_layer::FrameLayer::hash_url(&url);
    if frame_indices.is_empty() {
        super::frame_layer::clear_prefetch_url_in_flight(url_hash, prefetch_token);
        return;
    }
    let picker_owned = picker_visible_at_trigger;
    let url_guard = PrefetchUrlInFlightGuard::new(url_hash, prefetch_token);
    PRELOAD_RUNTIME.spawn(async move {
        let _url_guard = url_guard;
        prefetch_animated_frames_async(
            &url,
            start_frame,
            animation_frame_count,
            frame_indices,
            prefetch_token,
            picker_owned,
        )
        .await;
    });
}

async fn prefetch_from_decoded_animation_source(
    url: &str,
    start_frame: u16,
    decode_window: u16,
    prefetch_token: u64,
    picker_owned: bool,
) -> bool {
    let url_hash = super::frame_layer::FrameLayer::hash_url(url);
    if !super::frame_layer::prefetch_token_is_current(url_hash, prefetch_token) {
        return true;
    }
    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return true;
    }
    let Some(source) = get_decoded_animation_source(url, None) else {
        return false;
    };
    ANIMATION_REFILL_DISK_MISS_SINCE.remove(&url_hash);

    // Reading (from a sidecar) and decompressing the window runs on the
    // blocking pool, not the single loader worker.
    let owned_url = url.to_string();
    let inserted = match tokio::task::spawn_blocking(move || {
        insert_decoded_frame_window(
            &owned_url,
            &source,
            start_frame,
            decode_window,
            Some(prefetch_token),
        )
    })
    .await
    {
        Ok(inserted) => inserted,
        Err(_) => return false,
    };
    if inserted > 0 {
        crate::log_cache!(
            trace,
            url = %url,
            inserted,
            "Prefetched frame window from decoded source cache"
        );
        crate::components::overlays::notify_animation_refilled();
    }
    true
}

fn invalidate_unrecoverable_animation_refill(
    url: &str,
    url_hash: u64,
    prefetch_token: u64,
    picker_owned: bool,
    reason: &'static str,
) {
    if !persistent_animation_refill_disk_miss(url_hash) {
        return;
    }
    if !super::frame_layer::prefetch_token_is_current(url_hash, prefetch_token) {
        return;
    }
    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return;
    }

    let purge = FRAME_LAYER.purge_resolved_url(url);
    let source_bytes = purge_decoded_animation_source(url);
    forget_url(url_hash);
    crate::log_cache!(
        warn,
        url = %crate::log::redact_url(url),
        frames_removed = purge.frames_removed,
        metadata_removed = purge.metadata_removed,
        source_bytes,
        reason,
        "Invalidated unrecoverable animation refill"
    );
    trigger_animated_decode_with_source(
        url,
        if picker_owned {
            PreloadSource::PickerCell
        } else {
            PreloadSource::VisibleChat
        },
    );
    crate::components::overlays::notify_image_loaded();
}

async fn prefetch_animated_frames_async(
    url: &str,
    start_frame: u16,
    animation_frame_count: u16,
    frame_indices: SmallVec<[u16; 4]>,
    prefetch_token: u64,
    picker_owned: bool,
) {
    let url_hash = super::frame_layer::FrameLayer::hash_url(url);
    let mut unique_frame_indices: SmallVec<[u16; 4]> = SmallVec::new();
    for frame_idx in frame_indices {
        if !unique_frame_indices.contains(&frame_idx) {
            unique_frame_indices.push(frame_idx);
        }
    }
    let mut cached_indices: SmallVec<[u16; 4]> = SmallVec::new();
    let mut missing_indices: SmallVec<[u16; 4]> = SmallVec::new();
    for frame_idx in unique_frame_indices {
        if FRAME_LAYER.has_frame(url, frame_idx) {
            cached_indices.push(frame_idx);
        } else {
            missing_indices.push(frame_idx);
        }
    }
    if !cached_indices.is_empty() {
        super::frame_layer::clear_prefetch_in_flight_batch(
            url_hash,
            &cached_indices,
            prefetch_token,
        );
    }
    if missing_indices.is_empty() {
        return;
    }

    let start_frame = if animation_frame_count == 0 {
        0
    } else {
        missing_indices.first().copied().unwrap_or(start_frame) % animation_frame_count
    };
    let refill_window = if is_7tv_name_paint_url(url) {
        NAME_PAINT_ANIMATION_DECODE_WINDOW_FRAMES
    } else if FRAME_LAYER.picker_animation_hash_is_visible(url_hash) {
        PICKER_ANIMATION_REFILL_DECODE_WINDOW_FRAMES
    } else {
        super::frame_layer::ANIMATION_REFILL_DECODE_WINDOW_FRAMES
    };
    let decode_window = refill_window.min(animation_frame_count.max(1));

    let _prefetch_guard = PrefetchInFlightGuard::new(url_hash, missing_indices, prefetch_token);

    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return;
    }

    if prefetch_from_decoded_animation_source(
        url,
        start_frame,
        decode_window,
        prefetch_token,
        picker_owned,
    )
    .await
    {
        return;
    }

    let _permit = if picker_owned {
        let Some(permit) = acquire_picker_refill_decode_permit(
            Arc::clone(&ANIMATED_PICKER_DECODE_SEMAPHORE),
            url_hash,
        )
        .await
        else {
            return;
        };
        permit
    } else {
        match Arc::clone(&ANIMATED_NON_PICKER_DECODE_SEMAPHORE)
            .acquire_owned()
            .await
        {
            Ok(permit) => permit,
            Err(_) => return,
        }
    };
    if prefetch_from_decoded_animation_source(
        url,
        start_frame,
        decode_window,
        prefetch_token,
        picker_owned,
    )
    .await
    {
        return;
    }

    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return;
    }

    let byte_source = animation_byte_source(url);
    let Some(bytes) = DISK_LAYER
        .load_image(byte_source.as_deref().unwrap_or(url))
        .await
    else {
        invalidate_unrecoverable_animation_refill(
            url,
            url_hash,
            prefetch_token,
            picker_owned,
            "persistent disk miss",
        );
        return;
    };
    if !super::frame_layer::prefetch_token_is_current(url_hash, prefetch_token) {
        return;
    }
    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return;
    }

    let decoded = match tokio::task::spawn_blocking(move || {
        crate::image_decode::decode_animated_frames(&bytes)
    })
    .await
    {
        Ok(Some(f)) => {
            ANIMATION_REFILL_DISK_MISS_SINCE.remove(&url_hash);
            f
        }
        Ok(None) => {
            invalidate_unrecoverable_animation_refill(
                url,
                url_hash,
                prefetch_token,
                picker_owned,
                "undecodable disk bytes",
            );
            return;
        }
        Err(_) => return,
    };

    let frame_count = decoded.frames.len().min(u16::MAX as usize) as u16;
    if frame_count == 0 {
        return;
    }
    if !super::frame_layer::prefetch_token_is_current(url_hash, prefetch_token) {
        return;
    }
    if picker_owned && picker_refill_is_stale(&super::picker_visibility_snapshot(), url_hash) {
        return;
    }

    let raw_frame_bytes = decoded.raw_frame_bytes;
    let compressed_source_bytes = decoded.compressed_source_bytes;
    let source = cache_decoded_animation_source(url, None, decoded);
    let inserted = insert_decoded_frame_window(
        url,
        &source,
        start_frame,
        decode_window,
        Some(prefetch_token),
    );

    if inserted > 0 {
        crate::log_cache!(
            trace,
            url = %url,
            inserted,
            raw_frame_bytes,
            compressed_source_bytes,
            "Prefetched frame window from disk"
        );
        crate::components::overlays::notify_animation_refilled();
    }
}
