//! Animation frame layer for animated emote rendering.
//!
//! - a hot cache of decoded frames for the animations currently on screen,
//!   charged to the frames share of the shared budget;
//! - per-URL animation metadata (frame count, durations, dimensions);
//! - frame prefetching so playback does not wait on decode.
//!
//! # Architecture
//!

//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │                  FrameLayer                      │
//! │  ┌─────────────┐  ┌────────────────────────┐   │
//! │  │  Metadata   │  │     Hot Frame Cache    │   │
//! │  │  (tiny)     │  │   (frames budget)      │   │
//! │  └─────────────┘  └────────────────────────┘   │
//! │         │                    │                  │
//! │         ▼                    ▼                  │
//! │  frame_index_at()   get_current_frame()        │
//! └─────────────────────────────────────────────────┘
//! ```

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, OnceLock, Weak};
use std::time::Instant;

use dashmap::mapref::entry::Entry;
use dashmap::{DashMap, DashSet};
use gpui::{ImageId, RenderImage, RenderImagePixelSource};
use parking_lot::Mutex;
use smallvec::SmallVec;

use echo_cache_policy::budget::{BudgetManager, CacheLayer};
use echo_cache_policy::loaded_urls::url_key;
use echo_cache_policy::stats::CacheStats;
pub use echo_render_core::animation_timing::AnimationMetadata;

use crate::registry::{
    cancel_render_image_drop, picker_visible_hashes, queue_render_image_drop,
    with_picker_visible_hash_sets,
};

/// `(url, start_frame, animation_frame_count, frame_indices, prefetch_token,
/// picker_visible_at_trigger)`.
type AnimatedFramePrefetch = fn(String, u16, u16, SmallVec<[u16; 4]>, u64, bool);

static RETAINED_DECODED_URL: OnceLock<fn(u64) -> bool> = OnceLock::new();
static ANIMATED_FRAME_PREFETCH: OnceLock<AnimatedFramePrefetch> = OnceLock::new();
static VISIBLE_ANIMATION_COUNT: OnceLock<fn() -> usize> = OnceLock::new();

/// Install the loader's bounded retained-decoded-media registry probe.
///
/// Without it no URL is treated as retained, which is the same state a URL has
/// before the loader registers it.
pub fn install_retained_decoded_url_predicate(callback: fn(u64) -> bool) {
    let _ = RETAINED_DECODED_URL.set(callback);
}

/// Install the loader's animated frame-window prefetch entry point.
pub fn install_animated_frame_prefetch(callback: AnimatedFramePrefetch) {
    let _ = ANIMATED_FRAME_PREFETCH.set(callback);
}

/// Install the renderer's visible-animation counter.
pub fn install_visible_animation_count(callback: fn() -> usize) {
    let _ = VISIBLE_ANIMATION_COUNT.set(callback);
}

fn is_retained_decoded_url_key(key: u64) -> bool {
    RETAINED_DECODED_URL
        .get()
        .is_some_and(|callback| callback(key))
}

fn visible_metadata_count() -> usize {
    VISIBLE_ANIMATION_COUNT
        .get()
        .map_or(0, |callback| callback())
}

/// Global animation start time for synchronized animation
static ANIMATION_START: LazyLock<Instant> = LazyLock::new(Instant::now);

// ============================================================================
// Texture Deduplication Cache
// ============================================================================

/// Content-based texture deduplication cache.
/// Key: hash of (width, height, pixel_data)
/// Value: Weak<RenderImage> - allows automatic cleanup when all frames evicted
///
/// Textures by pixel content, so identical frames served under different URLs
/// (two providers hosting the same image) share one GPU texture.
static TEXTURE_CACHE: LazyLock<DashMap<u64, Weak<RenderImage>>> =
    LazyLock::new(|| DashMap::with_capacity(2000));

/// In-flight frame refill ownership: `(canonical URL, frame) -> refill token`.
static PREFETCH_IN_FLIGHT: LazyLock<DashMap<(u64, u16), u64>> =
    LazyLock::new(|| DashMap::with_capacity(100));

/// Canonical animations with one queued or active refill window.
///
/// Per-frame dedup alone still allows a rapidly advancing animation to enqueue
/// a new task for every phase before the single-worker preload runtime services
/// the first one. Keep one refill window per URL in flight; once it completes,
/// a later render can schedule the animation's newest missing phase.
static PREFETCH_URL_IN_FLIGHT: LazyLock<DashMap<u64, u64>> =
    LazyLock::new(|| DashMap::with_capacity(100));

/// Hot-frame window for an ordinary animation whose complete cycle is not
/// resident.
///
/// Matches the normal initial decode window so sliding-window playback keeps a
/// useful runway. Animations admitted by [`FrameLayer::admits_full_cycle`]
/// replace this limit with their frame count.
const DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION: usize =
    ANIMATION_INITIAL_DECODE_WINDOW_FRAMES as usize;

/// Hot-frame window for a picker-only animation whose complete cycle is not
/// resident.
///
/// A maximized picker can show hundreds of animated cells. Two frames keep the
/// current image plus a one-frame runway for every cell that did not fit the
/// full-cycle share.
const PICKER_MAX_RESIDENT_FRAMES_PER_ANIMATION: usize = 2;

/// Largest decoded cycle (`frame_count` x frame bytes) kept completely
/// resident.
///
/// Playback past a sliding window decompresses, hashes and uploads every
/// displayed frame again on each loop; on a real three-channel layout with a
/// 2 MiB cap that was ~225 frames (16 MB) a second. Hot frames release their
/// CPU pixels after the atlas upload, so a resident cycle costs atlas space,
/// not RAM, once it has played through. 16 MiB holds 340 frames of a 192x64
/// (wide 2x) emote; the admission share of the frame budget still bounds the
/// total, and larger GIFs keep the sliding window.
pub const ANIMATION_FULL_CYCLE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Share of the hot-frame byte budget and entry limit that visible full-cycle
/// animations may occupy at admission, as `numerator / denominator`. The rest
/// stays available for sliding windows and new arrivals.
const FULL_CYCLE_ADMISSION_SHARE: (usize, usize) = (3, 4);

/// Process-wide ceiling on hot animation frame entries.
///
/// Frame tiles are sub-allocations in shared atlas pages; the cost of an entry
/// is its `RenderImage` byte copy (already in the byte budget) plus a tile and
/// a few hundred bytes of bookkeeping. This bound caps that bookkeeping for
/// tiny frames, which the byte budget alone would admit by the ten thousand.
/// 4096 entries hold about 40 visible full-cycle emotes of 50-100 frames. The
/// displayed frame of every visible picker or chat animation remains protected
/// even if an unusually dense viewport temporarily exceeds this ceiling.
const MAX_HOT_FRAME_ENTRIES: usize = 4_096;
/// Temporary entry headroom while a picker viewport owns many visible
/// animations. The byte budget remains authoritative; this only prevents the
/// entry backstop from collapsing every visible URL to one frame.
const PICKER_MAX_HOT_FRAME_ENTRIES: usize = 8_192;
static FULL_CYCLE_DEMOTIONS: AtomicU64 = AtomicU64::new(0);
static NEXT_PREFETCH_TOKEN: AtomicU64 = AtomicU64::new(1);

/// A chat animation whose displayed frame was painted this recently is on
/// screen; eviction keeps that frame as it keeps a visible picker frame.
const CHAT_VISIBLE_FRAME_WINDOW_MS: u64 = 1_000;

/// Number of frames to keep queued ahead of the current animation frame.
///
/// A large lookahead window amplifies hot-frame churn when many animated emotes
/// are visible at once. Keeping one future frame warm preserves near-term
/// playback continuity without overwhelming the bounded frame budget.
pub const ANIMATION_PREFETCH_FRAMES: u16 = 1;
/// Number of consecutive frames to materialize from one animated decode.
///
/// Decoding an animated asset is much more expensive than keeping a few more
/// hot frames resident. A wider initial window reduces startup re-decodes, but
/// later refill windows need to stay smaller to avoid long-run cache churn.
pub const ANIMATION_INITIAL_DECODE_WINDOW_FRAMES: u16 = 8;
pub const ANIMATION_REFILL_DECODE_WINDOW_FRAMES: u16 = 4;

/// Whether playback is close enough to the loop boundary to warm frame zero.
///
/// The regular one-frame lookahead intentionally stays small to avoid hot-frame
/// churn. Near the seam, however, waiting until the final frame gives the
/// background refill thread only one frame duration to decompress frame zero.
/// Reusing the existing four-frame refill runway starts that bounded work early
/// enough to avoid visibly holding the final frame before the next cycle.
#[doc(hidden)]
pub fn animation_loop_seam_prefetch_due(current_frame: u16, frame_count: u16) -> bool {
    frame_count > ANIMATION_REFILL_DECODE_WINDOW_FRAMES
        && current_frame >= frame_count.saturating_sub(ANIMATION_REFILL_DECODE_WINDOW_FRAMES)
}

/// Hash pixel content for texture deduplication.
/// Includes dimensions to prevent false matches between different-sized images.
#[inline]
fn hash_content(data: &[u8], width: u32, height: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = ahash::AHasher::default();
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    data.hash(&mut hasher);
    hasher.finish()
}

/// Minimum interval between full texture-cache sweeps. Dead weak entries hold
/// no GPU memory (only a map slot), while an unthrottled sweep from the
/// eviction hot path was observed running 400+ full O(n) scans per second.
const TEXTURE_CLEANUP_MIN_INTERVAL_MS: u64 = 5_000;
static LAST_TEXTURE_CLEANUP_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HOT_FRAME_RENDER_IMAGES_CREATED: AtomicU64 = AtomicU64::new(0);

/// Cumulative hot-frame `RenderImage`s created since process start (texture
/// deduplication hits are not counted).
pub fn hot_frame_render_images_created() -> u64 {
    HOT_FRAME_RENDER_IMAGES_CREATED.load(Ordering::Relaxed)
}

/// Cumulative full-cycle animations returned to their sliding window under
/// byte or entry pressure since process start.
pub fn full_cycle_demotions() -> u64 {
    FULL_CYCLE_DEMOTIONS.load(Ordering::Relaxed)
}

/// Result of the fixture-only final texture-cache sweep.
///
/// `remaining` is expected to equal `live`: the sweep removes every dead weak
/// entry, while live textures remain available to the normal cache owner.
#[derive(Clone, Copy, Debug, Default)]
pub struct BenchmarkTextureCacheSnapshot {
    pub before: usize,
    pub removed: usize,
    pub remaining: usize,
    pub live: usize,
}

impl BenchmarkTextureCacheSnapshot {
    pub fn is_clean(&self) -> bool {
        self.remaining == self.live
    }
}

/// Clean up expired weak references from texture cache. Coalesced: at most one
/// full sweep per TEXTURE_CLEANUP_MIN_INTERVAL_MS regardless of call site.
pub fn cleanup_texture_cache() {
    let now = current_animation_time_ms();
    let last = LAST_TEXTURE_CLEANUP_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < TEXTURE_CLEANUP_MIN_INTERVAL_MS {
        return;
    }
    if LAST_TEXTURE_CLEANUP_MS
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return; // another caller claimed this interval
    }
    let before = TEXTURE_CACHE.len();
    TEXTURE_CACHE.retain(|_, weak| weak.strong_count() > 0);
    let removed = before.saturating_sub(TEXTURE_CACHE.len());
    if removed > 0 {
        crate::log_cache!(
            debug,
            removed,
            remaining = TEXTURE_CACHE.len(),
            "Cleaned up expired texture cache entries"
        );
    }
}

/// Force one final texture-cache sweep for a benchmark driver's snapshot.
///
/// This intentionally bypasses the normal five-second throttle only for the
/// fixture's terminal snapshot. It does not update the throttle timestamp, so
/// production eviction behavior and its hot-path coalescing remain unchanged.
#[doc(hidden)]
pub fn benchmark_final_texture_cache_snapshot() -> BenchmarkTextureCacheSnapshot {
    let before = TEXTURE_CACHE.len();
    TEXTURE_CACHE.retain(|_, weak| weak.strong_count() > 0);
    let remaining = TEXTURE_CACHE.len();
    let live = TEXTURE_CACHE
        .iter()
        .filter(|entry| entry.value().strong_count() > 0)
        .count();
    BenchmarkTextureCacheSnapshot {
        before,
        removed: before.saturating_sub(remaining),
        remaining,
        live,
    }
}

/// Get current animation time in milliseconds since app start
#[inline]
pub fn current_animation_time_ms() -> u64 {
    ANIMATION_START.elapsed().as_millis() as u64
}

/// Cached frame entry
struct CachedFrame {
    /// Pre-created RenderImage for instant rendering
    render_image: Arc<RenderImage>,
    /// Last access time for LRU eviction
    last_accessed_ms: AtomicU64,
    /// Memory size in bytes
    size_bytes: usize,
}

/// Frame key: (url_hash, frame_index)
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
struct FrameKey {
    url_hash: u64,
    frame_idx: u16,
}

impl FrameKey {
    #[inline]
    fn new(url_hash: u64, frame_idx: u16) -> Self {
        Self {
            url_hash,
            frame_idx,
        }
    }
}

/// Frame layer for animation management
pub struct FrameLayer {
    /// Animation metadata: canonical URL hash -> metadata
    metadata: DashMap<u64, Arc<AnimationMetadata>>,
    /// Hot frame cache: (canonical URL hash, frame idx) -> frame data
    frames: DashMap<FrameKey, CachedFrame>,
    /// URL alias hash -> canonical URL hash
    url_aliases: DashMap<u64, u64>,
    /// Provider/id/variant source hash -> canonical URL hash
    source_aliases: DashMap<u64, u64>,
    /// Resident frame count per canonical URL hash.
    ///
    /// Resident frames per animation, so `has_renderable_frame` answers "does
    /// this URL have *any* resident frame?" without scanning `frames`; it runs
    /// per animated emote per frame. Maintained alongside `frames` inserts and
    /// removals; entries are dropped when the count reaches zero.
    frame_counts: DashMap<u64, usize>,
    /// Bounded resident frame indices per animation. Updated under
    /// `frame_mutation_lock` with `frames`, so picker two-frame replacement
    /// never needs to scan unrelated animations for a victim.
    resident_frame_indices: DashMap<u64, SmallVec<[u16; 8]>>,
    /// Per-animation resident-frame ceiling.
    ///
    /// Sliding-window chat uses the eight-frame initial decode window and the
    /// picker two; full-cycle animations use their frame count.
    frame_limits: DashMap<u64, usize>,
    /// Canonical animations holding their complete cycle, mapped to the
    /// sliding-window limit they return to when demoted under pressure.
    full_cycle_fallback_limits: DashMap<u64, usize>,
    /// Canonical animations owned by persistent non-picker consumers.
    ///
    /// Provider-equivalent request URLs can alias one canonical animation, so
    /// ownership must follow the resolved FrameLayer key rather than a raw URL.
    /// This prevents a dense picker from reducing shared chat playback to its
    /// picker-only two-frame ceiling.
    retained_consumer_hashes: DashSet<u64>,
    /// Number of resident frame entries sharing each deduplicated GPU image.
    /// The atlas image can be retired only after the final frame entry leaves.
    texture_frame_counts: DashMap<ImageId, usize>,
    /// One known-resident frame index per animation URL.
    ///
    /// The regular bounded neighborhood probe cannot guarantee it will find the
    /// final resident frame of a long animation. This index gives picker poster
    /// and fallback rendering an O(1), no-blank path.
    fallback_frame_indices: DashMap<u64, u16>,
    /// Serializes frame-map mutation with its accounting/fallback indexes.
    ///
    /// Decode and texture creation stay outside this lock. Only the short
    /// budget-admission, insert, eviction, idle-reap, and purge transactions
    /// take it, preventing concurrent passes from each removing the frame the
    /// other expected to preserve.
    frame_mutation_lock: Mutex<()>,
    /// Current memory usage for frames
    frame_memory: AtomicUsize,
    /// Budget manager reference
    budget: Arc<BudgetManager>,
    /// Statistics
    stats: Arc<CacheStats>,
    /// Canonical URL hash to URL mapping (for prefetch requests)
    url_map: DashMap<u64, String>,
}

impl FrameLayer {
    #[inline]
    fn hot_frame_entry_limit(picker_visible_urls: usize) -> usize {
        MAX_HOT_FRAME_ENTRIES
            .saturating_add(
                picker_visible_urls.saturating_mul(PICKER_MAX_RESIDENT_FRAMES_PER_ANIMATION),
            )
            .min(PICKER_MAX_HOT_FRAME_ENTRIES)
    }
    /// How many frames to prefetch ahead
    const PREFETCH_FRAMES: u16 = ANIMATION_PREFETCH_FRAMES;

    /// Create a new frame layer
    pub fn new(budget: Arc<BudgetManager>, stats: Arc<CacheStats>) -> Self {
        Self {
            metadata: DashMap::with_capacity(500),
            frames: DashMap::with_capacity(1000),
            url_aliases: DashMap::with_capacity(500),
            source_aliases: DashMap::with_capacity(500),
            frame_counts: DashMap::with_capacity(500),
            resident_frame_indices: DashMap::with_capacity(500),
            frame_limits: DashMap::with_capacity(500),
            full_cycle_fallback_limits: DashMap::with_capacity(500),
            retained_consumer_hashes: DashSet::with_capacity(500),
            texture_frame_counts: DashMap::with_capacity(2000),
            fallback_frame_indices: DashMap::with_capacity(500),
            frame_mutation_lock: Mutex::new(()),
            frame_memory: AtomicUsize::new(0),
            budget,
            stats,
            url_map: DashMap::with_capacity(500),
        }
    }

    #[inline]
    fn resolve_hash(&self, url_hash: u64) -> u64 {
        self.url_aliases
            .get(&url_hash)
            .map(|canonical| *canonical)
            .unwrap_or(url_hash)
    }

    /// Animations whose displayed frame a paint touched within
    /// `CHAT_VISIBLE_FRAME_WINDOW_MS`. Exact when the caller holds
    /// `frame_mutation_lock`; full-cycle admission reads it as a snapshot.
    fn chat_visible_canonical_hashes(&self) -> std::collections::HashSet<u64> {
        let cutoff = echo_core::unix_now_ms().saturating_sub(CHAT_VISIBLE_FRAME_WINDOW_MS);
        self.fallback_frame_indices
            .iter()
            .filter(|entry| {
                self.frames
                    .get(&FrameKey::new(*entry.key(), *entry.value()))
                    .is_some_and(|frame| frame.last_accessed_ms.load(Ordering::Relaxed) >= cutoff)
            })
            .map(|entry| *entry.key())
            .collect()
    }

    fn picker_visible_canonical_hashes(&self) -> std::collections::HashSet<u64> {
        picker_visible_hashes()
            .into_iter()
            .map(|hash| self.resolve_hash(hash))
            .collect()
    }

    #[inline]
    fn picker_visible_animation_upper_bound() -> usize {
        // This is intentionally conservative: the same URL can appear in both
        // picker surfaces, but harmless extra headroom avoids a HashSet snapshot
        // on every ordinary frame insertion.
        with_picker_visible_hash_sets(|inline, popout| inline.len().saturating_add(popout.len()))
    }

    #[inline]
    fn picker_sets_contain_canonical(
        &self,
        inline: &std::collections::HashSet<u64>,
        popout: &std::collections::HashSet<u64>,
        canonical_hash: u64,
    ) -> bool {
        ((inline.contains(&canonical_hash) || popout.contains(&canonical_hash))
            && self.resolve_hash(canonical_hash) == canonical_hash)
            || inline
                .iter()
                .chain(popout.iter())
                .any(|hash| self.resolve_hash(*hash) == canonical_hash)
    }

    /// Whether one canonical animation is eligible in either picker viewport.
    ///
    /// Published picker URLs can be aliases of the canonical URL stored in this
    /// layer, so exact raw-hash membership is insufficient for refill ownership.
    pub fn picker_animation_hash_is_visible(&self, canonical_hash: u64) -> bool {
        with_picker_visible_hash_sets(|inline, popout| {
            self.picker_sets_contain_canonical(inline, popout, canonical_hash)
        })
    }

    /// Resolve a picker visibility snapshot through this layer's URL aliases.
    pub fn picker_visibility_contains_canonical_hash(
        &self,
        visible_hashes: &std::collections::HashSet<u64>,
        canonical_hash: u64,
    ) -> bool {
        (visible_hashes.contains(&canonical_hash)
            && self.resolve_hash(canonical_hash) == canonical_hash)
            || visible_hashes
                .iter()
                .any(|hash| self.resolve_hash(*hash) == canonical_hash)
    }

    fn remove_aliases_for_canonical(&self, canonical_hash: u64) {
        self.url_aliases
            .retain(|_, target| *target != canonical_hash);
        self.source_aliases
            .retain(|_, target| *target != canonical_hash);
    }

    /// Attach an alias URL to an already-decoded provider/id/variant animation.
    ///
    /// Returns true when metadata exists and the alias can render from the
    /// existing frame/source caches without another full decode.
    pub fn alias_url_to_source_key(&self, url: &str, source_key: u64) -> bool {
        let Some(canonical_hash) = self.source_aliases.get(&source_key).map(|entry| *entry) else {
            return false;
        };

        if self.metadata.contains_key(&canonical_hash) {
            let url_hash = url_key(url);
            if url_hash != canonical_hash {
                self.url_aliases.insert(url_hash, canonical_hash);
            }
            return true;
        }

        self.source_aliases.remove(&source_key);
        false
    }

    /// Check whether animation metadata is resident under a provider/id/variant key.
    pub fn has_source_animation(&self, source_key: u64) -> bool {
        self.source_aliases
            .get(&source_key)
            .map(|canonical_hash| self.metadata.contains_key(&*canonical_hash))
            .unwrap_or(false)
    }

    /// Remove all decoded animation data for a provider/id/variant source key.
    pub fn purge_source_key(&self, source_key: u64) -> FramePurgeStats {
        let Some(canonical_hash) = self.source_aliases.get(&source_key).map(|entry| *entry) else {
            return FramePurgeStats::default();
        };
        self.purge_canonical_hash(canonical_hash)
    }

    /// Increment the resident-frame count for a URL hash (new frame key inserted).
    #[inline]
    fn inc_frame_count(&self, hash: u64) {
        *self.frame_counts.entry(hash).or_insert(0) += 1;
    }

    /// Decrement the resident-frame count for a URL hash, dropping the entry at zero.
    #[inline]
    fn dec_frame_count(&self, hash: u64) {
        // remove_if drops the entry atomically when the count would reach zero,
        // otherwise leave it decremented. Avoids a stale-zero entry leak.
        let mut hit_zero = false;
        if let Some(mut v) = self.frame_counts.get_mut(&hash) {
            *v = v.saturating_sub(1);
            hit_zero = *v == 0;
        }
        if hit_zero {
            // Only remove if still zero (another insert may have raced in).
            self.frame_counts.remove_if(&hash, |_, &v| v == 0);
        }
    }

    #[inline]
    fn add_resident_frame_index(&self, hash: u64, frame_idx: u16) {
        let mut indices = self.resident_frame_indices.entry(hash).or_default();
        if !indices.contains(&frame_idx) {
            indices.push(frame_idx);
        }
    }

    #[inline]
    fn remove_resident_frame_index(&self, hash: u64, frame_idx: u16) {
        let mut empty = false;
        if let Some(mut indices) = self.resident_frame_indices.get_mut(&hash) {
            indices.retain(|idx| *idx != frame_idx);
            empty = indices.is_empty();
        }
        if empty {
            self.resident_frame_indices
                .remove_if(&hash, |_, indices| indices.is_empty());
        }
    }

    #[inline]
    fn retain_render_image(&self, image_id: ImageId) {
        cancel_render_image_drop(image_id);
        *self.texture_frame_counts.entry(image_id).or_insert(0) += 1;
    }

    fn release_render_image(&self, image: Arc<RenderImage>) {
        let should_retire = match self.texture_frame_counts.entry(image.id) {
            Entry::Occupied(mut entry) if *entry.get() > 1 => {
                *entry.get_mut() -= 1;
                false
            }
            Entry::Occupied(entry) => {
                entry.remove();
                true
            }
            // Repair conservatively if an older cache entry predates tracking.
            Entry::Vacant(_) => true,
        };
        if should_retire {
            queue_render_image_drop(image);
        }
    }

    fn repair_fallback_frame_index(&self, hash: u64, removed_idx: u16) {
        let points_at_removed = self
            .fallback_frame_indices
            .get(&hash)
            .is_some_and(|idx| *idx == removed_idx);
        if !points_at_removed {
            return;
        }

        let replacement = self
            .resident_frame_indices
            .get(&hash)
            .and_then(|indices| indices.first().copied());

        // Do not overwrite a newer concurrent insert. Remove only the stale
        // pointer, then fill the vacancy if another resident frame exists.
        self.fallback_frame_indices
            .remove_if(&hash, |_, idx| *idx == removed_idx);
        if let Some(frame_idx) = replacement {
            self.fallback_frame_indices.entry(hash).or_insert(frame_idx);
        }
    }

    /// Remove and account one hot frame while keeping the O(1) fallback index
    /// coherent. The caller must hold `frame_mutation_lock`. Returns the
    /// released byte count.
    fn remove_frame(&self, key: FrameKey) -> Option<usize> {
        let (removed_key, entry) = self.frames.remove(&key)?;
        self.dec_frame_count(removed_key.url_hash);
        self.remove_resident_frame_index(removed_key.url_hash, removed_key.frame_idx);
        self.repair_fallback_frame_index(removed_key.url_hash, removed_key.frame_idx);
        self.budget.release(CacheLayer::Frames, entry.size_bytes);
        self.frame_memory
            .fetch_sub(entry.size_bytes, Ordering::Relaxed);
        self.stats.hot_frames.fetch_sub(1, Ordering::Relaxed);
        self.stats
            .frame_memory
            .fetch_sub(entry.size_bytes, Ordering::Relaxed);
        self.stats.record_eviction(entry.size_bytes);
        self.release_render_image(entry.render_image);
        Some(entry.size_bytes)
    }

    /// Register an animation with the default sliding hot-frame window.
    pub fn register_animation(&self, url: &str, metadata: AnimationMetadata) {
        self.register_animation_with(
            url,
            None,
            metadata,
            AnimationResidency::Sliding {
                frames: DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION,
            },
        );
    }

    /// Register animation metadata, with an optional provider/id/variant
    /// alias, keeping `residency` frames hot.
    pub fn register_animation_with(
        &self,
        url: &str,
        source_key: Option<u64>,
        metadata: AnimationMetadata,
        residency: AnimationResidency,
    ) {
        match residency {
            AnimationResidency::Sliding { frames } => {
                self.register_sliding_animation(url, source_key, metadata, frames);
            }
            AnimationResidency::FullCycle { fallback_frames } => {
                let frame_count = metadata.frame_count as usize;
                self.register_sliding_animation(url, source_key, metadata, frame_count);
                self.full_cycle_fallback_limits
                    .insert(url_key(url), fallback_frames.clamp(1, frame_count.max(1)));
            }
        }
    }

    fn register_sliding_animation(
        &self,
        url: &str,
        source_key: Option<u64>,
        metadata: AnimationMetadata,
        max_resident_frames: usize,
    ) {
        let hash = url_key(url);
        let size = metadata.memory_size();
        let mut requested_frame_limit = max_resident_frames
            .max(1)
            .min(metadata.frame_count as usize);

        self.url_aliases.remove(&hash);
        self.remove_aliases_for_canonical(hash);
        if let Some(source_key) = source_key {
            self.source_aliases.insert(source_key, hash);
            // The loader's existing retained-media registry is hard-bounded
            // and is populated before an in-flight early return. Consulting it
            // after publishing the canonical provider alias closes both sides
            // of the registration race without another lifetime map.
            if is_retained_decoded_url_key(source_key) {
                self.retained_consumer_hashes.insert(hash);
                requested_frame_limit = requested_frame_limit
                    .max(ANIMATION_INITIAL_DECODE_WINDOW_FRAMES as usize)
                    .min(metadata.frame_count as usize);
            }
        } else if is_retained_decoded_url_key(hash) {
            self.retained_consumer_hashes.insert(hash);
            requested_frame_limit = requested_frame_limit
                .max(ANIMATION_INITIAL_DECODE_WINDOW_FRAMES as usize)
                .min(metadata.frame_count as usize);
        }
        self.url_map.insert(hash, url.to_string());
        let frame_limit = if self.retained_consumer_hashes.contains(&hash) {
            self.frame_limits
                .get(&hash)
                .map_or(requested_frame_limit, |limit| {
                    requested_frame_limit.max(*limit)
                })
                .min(metadata.frame_count as usize)
        } else {
            requested_frame_limit
        };
        // Limits are monotonic until canonical purge. A concurrent persistent
        // consumer may have promoted this URL after the retained check above;
        // entry-wise max prevents picker registration from overwriting 8 with 2.
        self.frame_limits
            .entry(hash)
            .and_modify(|limit| *limit = (*limit).max(frame_limit))
            .or_insert(frame_limit);
        let replaced = if let Some(old) = self.metadata.insert(hash, Arc::new(metadata)) {
            // Re-registration overwrote an existing entry; release the old
            // entry's accounted size so the metadata budget doesn't drift up.
            self.budget.release(CacheLayer::Metadata, old.memory_size());
            true
        } else {
            false
        };

        if !replaced {
            self.stats
                .animation_metadata
                .fetch_add(1, Ordering::Relaxed);
            self.stats.animated_images.fetch_add(1, Ordering::Relaxed);
        }
        self.budget.allocate(CacheLayer::Metadata, size);
    }

    /// Whether a newly decoded animation of `frame_count` frames totalling
    /// `cycle_bytes` may keep its complete cycle resident.
    ///
    /// The cycle must fit [`ANIMATION_FULL_CYCLE_MAX_BYTES`] and, together
    /// with the frames of animations visible now, the admission share of both
    /// the byte budget and the entry limit. Offscreen frames do not count:
    /// eviction removes them first. This is a snapshot; concurrent admissions
    /// that overshoot are demoted by eviction.
    pub fn admits_full_cycle(&self, frame_count: usize, cycle_bytes: usize) -> bool {
        if frame_count <= 1 || cycle_bytes > ANIMATION_FULL_CYCLE_MAX_BYTES {
            return false;
        }
        let picker_visible = self.picker_visible_canonical_hashes();
        let chat_visible = self.chat_visible_canonical_hashes();
        let (mut visible_entries, mut visible_bytes) = (0usize, 0usize);
        for entry in self.frames.iter() {
            let hash = entry.key().url_hash;
            if picker_visible.contains(&hash) || chat_visible.contains(&hash) {
                visible_entries += 1;
                visible_bytes += entry.value().size_bytes;
            }
        }
        let (numerator, denominator) = FULL_CYCLE_ADMISSION_SHARE;
        let entry_share = MAX_HOT_FRAME_ENTRIES * numerator / denominator;
        let byte_share = self.budget.budget_for(CacheLayer::Frames) / denominator * numerator;
        visible_entries.saturating_add(frame_count) <= entry_share
            && visible_bytes.saturating_add(cycle_bytes) <= byte_share
    }

    /// Retain canonical non-picker ownership and raise, but never reduce, its
    /// hot-frame runway. This also works before metadata registration, allowing
    /// a concurrent picker-first decode to preserve the chat runway.
    pub fn retain_consumer_with_frame_limit(
        &self,
        url: &str,
        source_key: Option<u64>,
        minimum_limit: usize,
    ) {
        let minimum_limit = minimum_limit.max(1);
        let hash = if let Some(source_key) = source_key {
            if let Some(hash) = self.source_aliases.get(&source_key).map(|hash| *hash) {
                hash
            } else {
                // The hard-bounded retained-media registry carries this claim
                // until registration publishes the provider's canonical hash.
                return;
            }
        } else {
            self.resolve_hash(url_key(url))
        };
        self.retained_consumer_hashes.insert(hash);
        let frame_count = self
            .metadata
            .get(&hash)
            .map(|metadata| metadata.frame_count as usize);
        let minimum_limit = frame_count.map_or(minimum_limit, |count| minimum_limit.min(count));
        self.frame_limits
            .entry(hash)
            .and_modify(|limit| *limit = (*limit).max(minimum_limit))
            .or_insert(minimum_limit);
    }

    #[inline]
    fn resident_frame_limit(&self, hash: u64, picker_visible: bool) -> usize {
        let registered_limit = self
            .frame_limits
            .get(&hash)
            .map_or(DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION, |limit| *limit);
        if picker_visible
            && !self.retained_consumer_hashes.contains(&hash)
            && !self.full_cycle_fallback_limits.contains_key(&hash)
        {
            PICKER_MAX_RESIDENT_FRAMES_PER_ANIMATION.min(registered_limit)
        } else {
            registered_limit
        }
    }

    /// Get animation metadata
    /// Shared, so per-frame callers do not copy the frame-duration list.
    pub fn get_metadata(&self, url: &str) -> Option<Arc<AnimationMetadata>> {
        let hash = self.resolve_hash(url_key(url));
        self.metadata.get(&hash).map(|m| Arc::clone(m.value()))
    }

    /// Check if animation metadata exists
    pub fn has_metadata(&self, url: &str) -> bool {
        self.has_metadata_by_hash(url_key(url))
    }

    /// Check if animation metadata exists for a pre-computed URL hash.
    /// For callers that already hashed the URL (or probe several maps).
    #[inline]
    pub fn has_metadata_by_hash(&self, hash: u64) -> bool {
        self.metadata.contains_key(&self.resolve_hash(hash))
    }

    /// Check whether a specific decoded animation frame is resident in the hot cache.
    pub fn has_frame(&self, url: &str, frame_idx: u16) -> bool {
        let hash = self.resolve_hash(url_key(url));
        self.frames.contains_key(&FrameKey::new(hash, frame_idx))
    }

    /// Check whether the animation has a frame that can be rendered now.
    ///
    /// This mirrors the render fallback in `get_current_frame()` without triggering
    /// frame prefetch. State/signature probes call this to avoid turning read-only
    /// readiness checks into cache-loader work.
    pub fn has_renderable_frame(&self, url: &str) -> bool {
        self.has_renderable_frame_by_hash(url_key(url))
    }

    /// `has_renderable_frame` for a pre-computed URL hash.
    /// For callers that already hashed the URL.
    pub fn has_renderable_frame_by_hash(&self, hash: u64) -> bool {
        let hash = self.resolve_hash(hash);
        let Some(metadata) = self.metadata.get(&hash) else {
            return false;
        };

        let current_frame = metadata.frame_index_at(current_animation_time_ms());
        if self
            .frames
            .contains_key(&FrameKey::new(hash, current_frame))
        {
            return true;
        }

        // O(1) "does this URL have any resident frame?" via the per-URL count index.
        self.frame_counts
            .get(&hash)
            .map(|c| *c > 0)
            .unwrap_or(false)
    }

    /// Remove all decoded animation data for one URL.
    ///
    /// Raw bytes remain in the disk layer. This releases hot frame textures,
    /// metadata, and pending prefetch markers so picker-owned animation memory
    /// can be dropped when the picker closes.
    pub fn purge_url(&self, url: &str) -> FramePurgeStats {
        let hash = url_key(url);
        let canonical_hash = self.resolve_hash(hash);
        if hash != canonical_hash {
            self.url_aliases.remove(&hash);
            return FramePurgeStats::default();
        }
        self.purge_canonical_hash(canonical_hash)
    }

    /// Purge the canonical animation resolved from a URL, even when the URL is
    /// currently registered only as a CDN alias.
    ///
    /// Refill recovery uses this after persistent source+disk loss; alias-only
    /// removal would leave canonical metadata in place and permanently block
    /// normal network decode admission.
    pub fn purge_resolved_url(&self, url: &str) -> FramePurgeStats {
        let canonical_hash = self.resolve_hash(url_key(url));
        self.purge_canonical_hash(canonical_hash)
    }

    fn purge_canonical_hash(&self, canonical_hash: u64) -> FramePurgeStats {
        let _mutation_guard = self.frame_mutation_lock.lock();
        let keys: Vec<FrameKey> = self
            .frames
            .iter()
            .filter_map(|entry| {
                let key = *entry.key();
                (key.url_hash == canonical_hash).then_some(key)
            })
            .collect();

        let mut stats = FramePurgeStats::default();
        for key in keys {
            if let Some(size_bytes) = self.remove_frame(key) {
                stats.frames_removed += 1;
                stats.frame_bytes += size_bytes;
            }
        }

        if let Some((_, metadata)) = self.metadata.remove(&canonical_hash) {
            let metadata_bytes = metadata.memory_size();
            self.budget.release(CacheLayer::Metadata, metadata_bytes);
            self.stats
                .animation_metadata
                .fetch_sub(1, Ordering::Relaxed);
            self.stats.animated_images.fetch_sub(1, Ordering::Relaxed);
            stats.metadata_removed = true;
            stats.metadata_bytes = metadata_bytes;
        }

        self.url_map.remove(&canonical_hash);
        self.frame_limits.remove(&canonical_hash);
        self.full_cycle_fallback_limits.remove(&canonical_hash);
        self.retained_consumer_hashes.remove(&canonical_hash);
        self.fallback_frame_indices.remove(&canonical_hash);
        self.remove_aliases_for_canonical(canonical_hash);
        let in_flight: Vec<(u64, u16)> = PREFETCH_IN_FLIGHT
            .iter()
            .filter_map(|entry| {
                let key = *entry.key();
                (key.0 == canonical_hash).then_some(key)
            })
            .collect();
        for key in in_flight {
            PREFETCH_IN_FLIGHT.remove(&key);
        }
        PREFETCH_URL_IN_FLIGHT.remove(&canonical_hash);

        if stats.frames_removed > 0 || stats.metadata_removed {
            cleanup_texture_cache();
        }
        stats
    }

    /// Bounded fallback probe for a resident frame of the same animation when
    /// the exact `frame_idx` isn't cached yet.
    ///
    /// Runs on the paint thread once per missed emote per paint, so it probes
    /// a bounded set of keys instead of scanning resident frames. Probe policy:
    ///   1. O(1) early-out — if the per-URL residency counter reports zero
    ///      resident frames, return immediately without probing.
    ///   2. Probe only `FrameKey`s for *this* `hash`: first a wrapping
    ///      neighborhood around the requested `frame_idx` (idx±1, idx±2, …),
    ///      since frames nearest the current playback position are the best
    ///      visual match; then the initial-decode-window indices
    ///      0..min(WINDOW, frame_count), since the first frames are always
    ///      decoded first and so are the likeliest residents when playback has
    ///      run far ahead of prefetch.
    ///   3. Hard-cap total probes at PROBE_LIMIT, then give up (return None). A
    ///      one-frame visual hold until prefetch lands is acceptable.
    fn probe_fallback_frame_with_index(
        &self,
        hash: u64,
        frame_idx: u16,
        frame_count: u16,
    ) -> Option<(Arc<RenderImage>, u16)> {
        // (1) O(1) early-out: nothing resident for this URL, so nothing to probe.
        if frame_count == 0
            || self
                .frame_counts
                .get(&hash)
                .map(|c| *c == 0)
                .unwrap_or(true)
        {
            return None;
        }

        // Past this many misses we accept a one-frame hold until prefetch lands.
        const PROBE_LIMIT: usize = 24;

        let touch_now = echo_core::unix_now_ms();
        let try_idx = |idx: u16| -> Option<(Arc<RenderImage>, u16)> {
            let key = FrameKey::new(hash, idx);
            let image = self.frames.get(&key).map(|entry| {
                entry.last_accessed_ms.store(touch_now, Ordering::Relaxed);
                Arc::clone(&entry.render_image)
            })?;

            // Re-publish only while mutation is excluded and the frame is
            // still resident. A render may safely use the cloned image after a
            // concurrent eviction, but a fallback pointer must never outlive
            // the cache entry it names.
            let _mutation_guard = self.frame_mutation_lock.lock();
            if self.frames.contains_key(&key) {
                self.fallback_frame_indices.insert(hash, idx);
            }
            Some((image, idx))
        };

        // A missing wall-clock target should hold the pose that immediately
        // preceded it, never jump ahead into a future/cyclic pose. This matters
        // most for the picker's two-frame residency window and for wide frames
        // whose refill can arrive after the target boundary.
        let previous_idx =
            ((frame_idx as u32 + frame_count as u32 - 1) % frame_count as u32) as u16;
        if let Some(image) = try_idx(previous_idx) {
            return Some(image);
        }

        // The eviction layer maintains an exact known-resident fallback index.
        // Check it after the immediate predecessor so a long animation whose
        // sole retained frame is far from both the current and initial windows
        // can never flash blank.
        if let Some(frame_idx) = self.fallback_frame_indices.get(&hash).map(|entry| *entry) {
            if let Some(image) = try_idx(frame_idx) {
                return Some(image);
            }

            // The indexed frame may be between map removal and fallback repair
            // in another mutation transaction. Join that transaction instead
            // of clearing the pointer lock-free, then retry the exact repaired
            // index so a sole resident frame outside the bounded probe window
            // remains discoverable.
            let repaired_frame_idx = {
                let _mutation_guard = self.frame_mutation_lock.lock();
                self.repair_fallback_frame_index(hash, frame_idx);
                self.fallback_frame_indices.get(&hash).map(|entry| *entry)
            };
            if let Some(repaired_frame_idx) = repaired_frame_idx
                && repaired_frame_idx != frame_idx
                && let Some(image) = try_idx(repaired_frame_idx)
            {
                return Some(image);
            }
        }

        let mut probes = 0usize;

        // Reserve enough of the probe budget for the initial-window pass below:
        // a long animation whose playback position is far ahead of decode (only
        // the initial window resident yet — the common first-appearance case)
        // must still find a frame there.
        let window = ANIMATION_INITIAL_DECODE_WINDOW_FRAMES.min(frame_count);
        let neighborhood_budget = PROBE_LIMIT.saturating_sub(window as usize);

        // (2a) Wrapping neighborhood around the requested frame (idx±1, idx±2, …).
        // The exact frame_idx was already checked by the caller, so start at d=1.
        let fc = frame_count as u32;
        let mut d = 1u32;
        while d <= fc / 2 && probes < neighborhood_budget {
            let behind = ((frame_idx as u32 + fc - d) % fc) as u16;
            probes += 1;
            if let Some(img) = try_idx(behind) {
                return Some(img);
            }
            let ahead = ((frame_idx as u32 + d) % fc) as u16;
            if ahead != behind && probes < neighborhood_budget {
                probes += 1;
                if let Some(img) = try_idx(ahead) {
                    return Some(img);
                }
            }
            d += 1;
        }

        // (2b) Initial decode window — the first frames are decoded first, so
        // when playback has run far ahead they're the likeliest residents.
        let mut i = 0u16;
        while i < window && probes < PROBE_LIMIT {
            probes += 1;
            if let Some(img) = try_idx(i) {
                return Some(img);
            }
            i += 1;
        }

        None
    }

    /// Get current frame for an animation (main render API)
    ///
    /// Returns the RenderImage for the current animation frame, or None if
    /// the frame isn't cached yet. Triggers prefetching of upcoming frames.
    ///
    /// Read-only map access (get, not get_mut), so render threads never block
    /// on background decode threads inserting frames. Recency is updated
    /// through a lock-free atomic timestamp on the cached value.
    pub fn get_current_frame(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.animated_frame(url, FrameQuery::Current)
            .map(|frame| frame.image)
    }

    /// Get a stable resident frame without advancing playback or scheduling a
    /// refill. Picker Hover/Never policies use this as a no-tick poster fallback.
    pub fn get_stable_frame(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.animated_frame(url, FrameQuery::Stable)
            .map(|frame| frame.image)
    }

    /// [`Self::get_stable_frame`] plus the animation metadata and the index
    /// of the resident frame actually returned, which is frame 0 only when
    /// frame 0 is resident (the probe falls back to nearby frames).
    fn get_stable_frame_with_metadata_and_index(
        &self,
        url: &str,
    ) -> Option<(Arc<RenderImage>, Arc<AnimationMetadata>, u16)> {
        let hash = self.resolve_hash(url_key(url));
        let metadata = self
            .metadata
            .get(&hash)
            .map(|entry| Arc::clone(entry.value()))?;
        let Some((image, frame_idx)) =
            self.probe_fallback_frame_with_index(hash, 0, metadata.frame_count)
        else {
            self.stats.record_miss();
            return None;
        };
        self.stats.record_hit();
        Some((image, metadata, frame_idx))
    }

    /// One frame of `url` as `query` selects it.
    pub fn animated_frame(&self, url: &str, query: FrameQuery) -> Option<AnimatedFrame> {
        let (image, metadata, index, exact) = match query {
            FrameQuery::Current => self.get_current_frame_with_metadata_frame_and_exactness(url)?,
            FrameQuery::Resident => {
                self.get_resident_frame_with_metadata_frame_and_exactness(url)?
            }
            FrameQuery::Stable => {
                let (image, metadata, index) =
                    self.get_stable_frame_with_metadata_and_index(url)?;
                (image, metadata, index, false)
            }
        };
        Some(AnimatedFrame {
            image,
            metadata,
            index,
            exact,
        })
    }

    /// Get the displayed frame plus whether it exactly matches the animation's
    /// current wall-clock target. Picker playback uses this to keep scheduling a
    /// high-quality primary refill without letting one held primary frame mask a
    /// healthy compact fallback animation.
    fn get_current_frame_with_metadata_frame_and_exactness(
        &self,
        url: &str,
    ) -> Option<(Arc<RenderImage>, Arc<AnimationMetadata>, u16, bool)> {
        let hash = self.resolve_hash(url_key(url));

        // Get metadata (needed for frame index + visible registration)
        let metadata_arc = self
            .metadata
            .get(&hash)
            .map(|entry| Arc::clone(entry.value()))?;

        // Calculate current frame index
        let time_ms = current_animation_time_ms();
        let frame_idx = metadata_arc.frame_index_at(time_ms);

        // Try hot cache
        let key = FrameKey::new(hash, frame_idx);
        if let Some(entry) = self.frames.get(&key) {
            entry
                .last_accessed_ms
                .store(echo_core::unix_now_ms(), Ordering::Relaxed);
            let image = Arc::clone(&entry.render_image);
            drop(entry);
            // A fallback must describe a pose the user actually saw. Refill
            // insertion can include future/seam frames, so it must not own
            // this cursor.
            self.record_displayed_frame(key);
            self.stats.record_hit();
            self.trigger_prefetch(hash, frame_idx, &metadata_arc);
            return Some((image, metadata_arc, frame_idx, true));
        }

        // Miss — hold a resident fallback, but refill the authoritative
        // wall-clock target. Advancing from the fallback would create a second,
        // cache-driven playback cursor whose speed follows refill completion and
        // UI notifications instead of the source frame durations. The bounded
        // refill layer also warms the target's lookahead after it lands.
        self.stats.record_miss();
        let fallback =
            self.probe_fallback_frame_with_index(hash, frame_idx, metadata_arc.frame_count);
        self.trigger_prefetch(hash, frame_idx, &metadata_arc);

        fallback
            .map(|(image, displayed_frame_idx)| (image, metadata_arc, displayed_frame_idx, false))
    }

    /// Read the current/resident frame without scheduling any refill work.
    ///
    /// Picker source handoff uses this to keep an already-playable alternate
    /// moving while the one authoritative decode URL hydrates. It must not make
    /// the alternate join the decode/refill working set.
    fn get_resident_frame_with_metadata_frame_and_exactness(
        &self,
        url: &str,
    ) -> Option<(Arc<RenderImage>, Arc<AnimationMetadata>, u16, bool)> {
        let hash = self.resolve_hash(url_key(url));
        let metadata = self
            .metadata
            .get(&hash)
            .map(|entry| Arc::clone(entry.value()))?;
        let frame_idx = metadata.frame_index_at(current_animation_time_ms());
        let key = FrameKey::new(hash, frame_idx);
        if let Some(entry) = self.frames.get(&key) {
            entry
                .last_accessed_ms
                .store(echo_core::unix_now_ms(), Ordering::Relaxed);
            let image = Arc::clone(&entry.render_image);
            drop(entry);
            self.record_displayed_frame(key);
            self.stats.record_hit();
            return Some((image, metadata, frame_idx, true));
        }

        self.stats.record_miss();
        self.probe_fallback_frame_with_index(hash, frame_idx, metadata.frame_count)
            .map(|(image, displayed_frame_idx)| (image, metadata, displayed_frame_idx, false))
    }

    /// Insert a frame into the hot cache
    pub fn insert_frame(&self, url: &str, frame_idx: u16, data: Vec<u8>, width: u32, height: u32) {
        let _ = self.insert_frame_with(
            url,
            frame_idx,
            FramePixels {
                data,
                width,
                height,
            },
            FrameInsertOptions::default(),
        );
    }

    /// Insert a frame into the hot cache; returns whether it was admitted.
    ///
    /// With a `prefetch_token`, the frame is inserted only while that token
    /// still owns the canonical animation's active refill generation. The
    /// check runs under `frame_mutation_lock`, the same lock used by purge, so
    /// a purge either removes the frame afterward or invalidates the token
    /// first; stale refill tasks cannot resurrect frames after scope cleanup.
    pub fn insert_frame_with(
        &self,
        url: &str,
        frame_idx: u16,
        pixels: FramePixels,
        options: FrameInsertOptions,
    ) -> bool {
        let FramePixels {
            data,
            width,
            height,
        } = pixels;
        let FrameInsertOptions {
            prefetch_token,
            pixel_source,
        } = options;
        let hash = self.resolve_hash(url_key(url));
        let key = FrameKey::new(hash, frame_idx);
        let size_bytes = data.len();

        // Reuse a texture with identical pixel content, checked before entering
        // the narrow frame-state mutation transaction.
        let content_hash = hash_content(&data, width, height);
        let existing_texture = TEXTURE_CACHE
            .get(&content_hash)
            .and_then(|weak_ref| weak_ref.upgrade());
        let render_image = if let Some(existing) = existing_texture {
            self.stats.record_texture_reuse(size_bytes);
            crate::log_cache!(
                trace,
                content_hash,
                size_bytes,
                "Reused existing GPU texture via deduplication"
            );
            existing
        } else {
            match self.create_and_cache_texture(data, width, height, content_hash, pixel_source) {
                Some(img) => img,
                None => return false,
            }
        };

        // Admission, eviction, insertion, and all related accounting are one
        // short transaction. This makes the one-frame visible overage a real
        // per-URL invariant even when several decoders finish concurrently.
        let _mutation_guard = self.frame_mutation_lock.lock();
        if prefetch_token.is_some_and(|token| !prefetch_token_is_current(hash, token)) {
            return false;
        }
        let is_picker_visible = with_picker_visible_hash_sets(|inline, popout| {
            self.picker_sets_contain_canonical(inline, popout, hash)
        });
        let replacing_same_key = self.frames.contains_key(&key);
        let resident_frames = self.frame_counts.get(&hash).map_or(0, |count| *count);
        let resident_frame_limit = self.resident_frame_limit(hash, is_picker_visible);
        let mut stale_replacement =
            if !replacing_same_key && resident_frames >= resident_frame_limit {
                self.oldest_resident_frame_except(hash, key)
            } else {
                None
            };
        if !self.budget.try_allocate(CacheLayer::Frames, size_bytes) {
            self.evict_frames_locked(size_bytes);
            if !self.budget.try_allocate(CacheLayer::Frames, size_bytes) {
                let resident_frames = self.frame_counts.get(&hash).map_or(0, |count| *count);
                if resident_frames > 0 {
                    stale_replacement = if replacing_same_key {
                        None
                    } else {
                        self.oldest_resident_frame_except(hash, key)
                    };

                    if !replacing_same_key && stale_replacement.is_none() {
                        return false;
                    }

                    // Admit the new frame before removing the stale same-URL
                    // frame. The narrow one-frame overage is serialized by the
                    // mutation lock, so an animation always has a
                    // renderable fallback while still being able to rotate when
                    // its bounded working set fills the byte budget.
                    self.budget.allocate(CacheLayer::Frames, size_bytes);
                    crate::log_cache!(
                        trace,
                        url_hash = hash,
                        frame_idx,
                        size_bytes,
                        picker_visible = is_picker_visible,
                        resident_frame_limit,
                        "Rotating animation frame at frame budget"
                    );
                } else if is_picker_visible || self.retained_consumer_hashes.contains(&hash) {
                    // A visible animation with no resident frame would render as
                    // a blank placeholder and immediately retry. Permit exactly
                    // its first frame over budget. Later replacement frames use
                    // the serialized rotation path above.
                    self.budget.allocate(CacheLayer::Frames, size_bytes);
                    crate::log_cache!(
                        debug,
                        url_hash = hash,
                        size_bytes,
                        picker_visible = is_picker_visible,
                        "Held first visible animation frame over budget"
                    );
                } else {
                    return false;
                }
            }
        }

        let render_image_id = render_image.id;
        let entry = CachedFrame {
            render_image,
            last_accessed_ms: AtomicU64::new(echo_core::unix_now_ms()),
            size_bytes,
        };

        // Handle replacement
        let inserted_new = match self.frames.insert(key, entry) {
            Some(old) => {
                if old.render_image.id != render_image_id {
                    self.retain_render_image(render_image_id);
                    self.release_render_image(old.render_image);
                }
                self.budget.release(CacheLayer::Frames, old.size_bytes);
                self.frame_memory
                    .fetch_sub(old.size_bytes, Ordering::Relaxed);
                self.stats
                    .frame_memory
                    .fetch_sub(old.size_bytes, Ordering::Relaxed);
                false
            }
            _ => {
                // New frame key for this URL — bump the per-URL resident-frame count.
                self.retain_render_image(render_image_id);
                self.inc_frame_count(hash);
                self.add_resident_frame_index(hash, frame_idx);
                true
            }
        };
        // Do not let a speculative lookahead/seam insertion replace the
        // displayed fallback pose. Initialize only when no render path has
        // selected one yet; probe_fallback and exact hits republish frames that
        // were actually returned to the caller.
        self.fallback_frame_indices.entry(hash).or_insert(frame_idx);

        self.frame_memory.fetch_add(size_bytes, Ordering::Relaxed);
        if inserted_new {
            self.stats.hot_frames.fetch_add(1, Ordering::Relaxed);
        }
        self.stats
            .frame_memory
            .fetch_add(size_bytes, Ordering::Relaxed);

        if let Some(stale_key) = stale_replacement {
            let _ = self.remove_frame(stale_key);
            // Frame dimensions are normally identical, but a malformed source
            // can change dimensions between frames. Reclaim any residual byte
            // delta while preserving the final visible fallback invariant.
            self.evict_frames_locked(0);
        }
        self.evict_frame_entries_to_limit_locked(key);
        true
    }

    /// The visible full-cycle animation holding the most resident bytes (or
    /// entries), the cheapest to return to a sliding window per unit freed.
    /// The caller holds `frame_mutation_lock`.
    fn largest_visible_full_cycle_locked(
        &self,
        picker_visible: &std::collections::HashSet<u64>,
        chat_visible: &std::collections::HashSet<u64>,
        by_bytes: bool,
    ) -> Option<u64> {
        self.full_cycle_fallback_limits
            .iter()
            .map(|entry| *entry.key())
            .filter(|hash| picker_visible.contains(hash) || chat_visible.contains(hash))
            .map(|hash| {
                let weight = if by_bytes {
                    self.resident_frame_indices.get(&hash).map_or(0, |indices| {
                        indices
                            .iter()
                            .filter_map(|idx| {
                                self.frames
                                    .get(&FrameKey::new(hash, *idx))
                                    .map(|frame| frame.size_bytes)
                            })
                            .sum()
                    })
                } else {
                    self.frame_counts.get(&hash).map_or(0, |count| *count)
                };
                (hash, weight)
            })
            .max_by_key(|(_, weight)| *weight)
            .map(|(hash, _)| hash)
    }

    /// Return a full-cycle animation to its sliding window, keeping the
    /// displayed frame and the frames due next. The caller holds
    /// `frame_mutation_lock`. Returns the released byte count.
    fn demote_full_cycle_locked(&self, hash: u64, protected_key: Option<FrameKey>) -> usize {
        let Some((_, sliding_limit)) = self.full_cycle_fallback_limits.remove(&hash) else {
            return 0;
        };
        let sliding_limit = if self.retained_consumer_hashes.contains(&hash) {
            sliding_limit.max(DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION)
        } else {
            sliding_limit
        };
        self.frame_limits.insert(hash, sliding_limit);
        FULL_CYCLE_DEMOTIONS.fetch_add(1, Ordering::Relaxed);

        let Some(metadata) = self
            .metadata
            .get(&hash)
            .map(|entry| Arc::clone(entry.value()))
        else {
            return 0;
        };
        let frame_count = u32::from(metadata.frame_count.max(1));
        let current = u32::from(metadata.frame_index_at(current_animation_time_ms()));
        let limit = self.resident_frame_limit(hash, self.picker_animation_hash_is_visible(hash));
        let displayed = self.fallback_frame_indices.get(&hash).map(|entry| *entry);
        let runway = limit.saturating_sub(usize::from(displayed.is_some())) as u32;
        let indices: SmallVec<[u16; 8]> = self
            .resident_frame_indices
            .get(&hash)
            .map(|indices| indices.iter().copied().collect())
            .unwrap_or_default();
        let mut freed = 0usize;
        for frame_idx in indices {
            let ahead = (u32::from(frame_idx) + frame_count - current) % frame_count;
            let key = FrameKey::new(hash, frame_idx);
            if ahead < runway || displayed == Some(frame_idx) || Some(key) == protected_key {
                continue;
            }
            freed += self.remove_frame(key).unwrap_or(0);
        }
        crate::log_cache!(
            debug,
            url_hash = hash,
            sliding_limit = limit,
            freed_bytes = freed,
            "Demoted full-cycle animation to a sliding window"
        );
        freed
    }

    /// Publish only a still-resident frame, using the same mutation transaction
    /// as eviction. The caller must release its frame-map guard first.
    fn record_displayed_frame(&self, key: FrameKey) {
        if self
            .fallback_frame_indices
            .get(&key.url_hash)
            .is_some_and(|index| *index == key.frame_idx)
        {
            return;
        }
        let _mutation_guard = self.frame_mutation_lock.lock();
        if self.frames.contains_key(&key) {
            self.fallback_frame_indices
                .insert(key.url_hash, key.frame_idx);
        }
    }

    fn oldest_resident_frame_except(&self, hash: u64, excluded: FrameKey) -> Option<FrameKey> {
        let displayed_fallback = self.fallback_frame_indices.get(&hash).map(|entry| *entry);
        let indices = self.resident_frame_indices.get(&hash)?;
        let candidate = indices
            .iter()
            .filter_map(|frame_idx| {
                let candidate = FrameKey::new(hash, *frame_idx);
                (candidate != excluded && displayed_fallback != Some(*frame_idx))
                    .then_some(candidate)
            })
            .filter_map(|candidate| {
                self.frames
                    .get(&candidate)
                    .map(|entry| (candidate, entry.last_accessed_ms.load(Ordering::Relaxed)))
            })
            .min_by_key(|(_, last_accessed)| *last_accessed)
            .map(|(candidate, _)| candidate);
        // Under byte pressure a due frame may replace a sole held pose, but a
        // speculative lookahead must not evict it and become visible early.
        candidate.or_else(|| {
            let due = self.metadata.get(&hash).is_some_and(|metadata| {
                metadata.frame_index_at(current_animation_time_ms()) == excluded.frame_idx
            });
            if !due {
                return None;
            }
            indices
                .iter()
                .map(|frame_idx| FrameKey::new(hash, *frame_idx))
                .filter(|candidate| *candidate != excluded)
                .filter_map(|candidate| {
                    self.frames
                        .get(&candidate)
                        .map(|entry| (candidate, entry.last_accessed_ms.load(Ordering::Relaxed)))
                })
                .min_by_key(|(_, last_accessed)| *last_accessed)
                .map(|(candidate, _)| candidate)
        })
    }

    /// Enforce the process-wide hot-frame entry ceiling. The caller holds
    /// `frame_mutation_lock` and has already inserted `protected_key`.
    ///
    /// Evict offscreen frames first, then demote visible full-cycle animations
    /// to their sliding windows, then trim visible surplus frames. Never remove
    /// the displayed frame of a visible picker or chat URL.
    /// The just-inserted frame is protected so admission cannot immediately
    /// discard the exact frame a waiting paint requested.
    fn evict_frame_entries_to_limit_locked(&self, protected_key: FrameKey) {
        let mut removed = 0usize;
        let mut freed = 0usize;
        let entry_limit = Self::hot_frame_entry_limit(Self::picker_visible_animation_upper_bound());
        if self.frames.len() <= entry_limit {
            return;
        }
        let picker_visible = self.picker_visible_canonical_hashes();
        let chat_visible = self.chat_visible_canonical_hashes();
        let mut demotion_checked = false;
        // Candidates the live picker check refused. The snapshots above are not
        // refreshed inside the loop, so without this the same refused candidate
        // would be picked again forever while holding the mutation lock that the
        // paint path also takes.
        let mut refused: SmallVec<[FrameKey; 8]> = SmallVec::new();

        while self.frames.len() > entry_limit {
            let candidate = self
                .frames
                .iter()
                .filter_map(|entry| {
                    let key = *entry.key();
                    if key == protected_key || refused.contains(&key) {
                        return None;
                    }
                    let url_frames = self
                        .frame_counts
                        .get(&key.url_hash)
                        .map_or(0, |count| *count);
                    let is_visible = picker_visible.contains(&key.url_hash)
                        || chat_visible.contains(&key.url_hash);
                    let displayed_fallback = self
                        .fallback_frame_indices
                        .get(&key.url_hash)
                        .is_some_and(|fallback| *fallback == key.frame_idx);
                    if is_visible && (url_frames <= 1 || displayed_fallback) {
                        return None;
                    }
                    Some((
                        key,
                        url_frames <= 1,
                        is_visible,
                        entry.last_accessed_ms.load(Ordering::Relaxed),
                    ))
                })
                .min_by_key(|(_, sole_fallback, visible, accessed)| {
                    (*sole_fallback, *visible, *accessed)
                });

            let Some((candidate, _, candidate_visible, _)) = candidate else {
                break;
            };
            if candidate_visible && !demotion_checked {
                demotion_checked = true;
                while self.frames.len() > entry_limit {
                    let Some(hash) = self.largest_visible_full_cycle_locked(
                        &picker_visible,
                        &chat_visible,
                        false,
                    ) else {
                        break;
                    };
                    freed += self.demote_full_cycle_locked(hash, Some(protected_key));
                }
                continue;
            }
            let size_bytes = with_picker_visible_hash_sets(|inline, popout| {
                let is_visible =
                    self.picker_sets_contain_canonical(inline, popout, candidate.url_hash)
                        || chat_visible.contains(&candidate.url_hash);
                let url_frames = self
                    .frame_counts
                    .get(&candidate.url_hash)
                    .map_or(0, |count| *count);
                let displayed_fallback = self
                    .fallback_frame_indices
                    .get(&candidate.url_hash)
                    .is_some_and(|fallback| *fallback == candidate.frame_idx);
                if is_visible && (url_frames <= 1 || displayed_fallback) {
                    None
                } else {
                    self.remove_frame(candidate)
                }
            });
            if let Some(size_bytes) = size_bytes {
                removed += 1;
                freed += size_bytes;
            } else {
                refused.push(candidate);
            }
        }

        if removed > 0 {
            cleanup_texture_cache();
            crate::log_cache!(
                trace,
                removed,
                freed_bytes = freed,
                remaining = self.frames.len(),
                entry_limit,
                "Enforced global hot-frame entry limit"
            );
        }
    }

    /// Create new RenderImage and cache it for deduplication
    fn create_and_cache_texture(
        &self,
        data: Vec<u8>,
        width: u32,
        height: u32,
        content_hash: u64,
        pixel_source: Option<RenderImagePixelSource>,
    ) -> Option<Arc<RenderImage>> {
        let img = Self::create_render_image(data, width, height, pixel_source)?;
        HOT_FRAME_RENDER_IMAGES_CREATED.fetch_add(1, Ordering::Relaxed);
        let arc = Arc::new(img);
        TEXTURE_CACHE.insert(content_hash, Arc::downgrade(&arc));
        Some(arc)
    }

    /// Create RenderImage from BGRA pixel data
    fn create_render_image(
        data: Vec<u8>,
        width: u32,
        height: u32,
        pixel_source: Option<RenderImagePixelSource>,
    ) -> Option<RenderImage> {
        use image::{Delay, Frame, RgbaImage};

        let delay = Delay::from_numer_denom_ms(0, 1);
        if let Some(pixel_source) = pixel_source {
            let expected = (width as usize)
                .checked_mul(height as usize)?
                .checked_mul(4)?;
            return (data.len() == expected)
                .then(|| RenderImage::new_releasable(width, height, delay, data, pixel_source));
        }
        // Note: data is already in BGRA format from decode
        let rgba = RgbaImage::from_raw(width, height, data)?;
        let frame = Frame::from_parts(rgba, 0, 0, delay);
        Some(RenderImage::new(vec![frame]))
    }

    /// Trigger prefetch of upcoming frames
    fn trigger_prefetch(&self, url_hash: u64, current_frame: u16, metadata: &AnimationMetadata) {
        let prefetch_token = NEXT_PREFETCH_TOKEN.fetch_add(1, Ordering::Relaxed);
        match PREFETCH_URL_IN_FLIGHT.entry(url_hash) {
            Entry::Occupied(_) => return,
            Entry::Vacant(entry) => {
                entry.insert(prefetch_token);
            }
        }

        // Keep the regular one-frame lookahead while adding frame zero only
        // inside the bounded loop-seam runway. The loader still processes the
        // same four-frame refill window; this merely gives that work more than
        // one final-frame duration to finish.
        let mut candidate_frames: SmallVec<[u16; 4]> = SmallVec::new();
        for i in 0..=Self::PREFETCH_FRAMES {
            candidate_frames.push((current_frame + i) % metadata.frame_count);
        }
        let picker_visible = self.picker_animation_hash_is_visible(url_hash);
        let picker_two_frame_runway = picker_visible
            && self.resident_frame_limit(url_hash, true)
                <= PICKER_MAX_RESIDENT_FRAMES_PER_ANIMATION;
        if !picker_two_frame_runway
            && animation_loop_seam_prefetch_due(current_frame, metadata.frame_count)
            && !candidate_frames.contains(&0)
        {
            candidate_frames.push(0);
        }

        let mut requested_frames: SmallVec<[u16; 4]> = SmallVec::new();
        for next_frame in candidate_frames {
            let key = FrameKey::new(url_hash, next_frame);

            // Skip if already cached
            if self.frames.contains_key(&key) {
                continue;
            }

            // Skip if already being prefetched (dedup)
            let flight_key = (url_hash, next_frame);
            match PREFETCH_IN_FLIGHT.entry(flight_key) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(entry) => {
                    entry.insert(prefetch_token);
                }
            }

            requested_frames.push(next_frame);
        }

        if requested_frames.is_empty() {
            clear_prefetch_url_in_flight(url_hash, prefetch_token);
            return;
        }

        match self.url_map.get(&url_hash) {
            Some(url_ref) => {
                let url = url_ref.clone();
                let frame_count = requested_frames.len();
                let start_frame = requested_frames[0];
                let picker_visible = self.picker_animation_hash_is_visible(url_hash);
                if let Some(prefetch) = ANIMATED_FRAME_PREFETCH.get() {
                    prefetch(
                        url,
                        start_frame,
                        metadata.frame_count,
                        requested_frames,
                        prefetch_token,
                        picker_visible,
                    );
                }
                crate::log_cache!(
                    trace,
                    url_hash,
                    frame_count,
                    "Prefetch spawned for frame window"
                );
            }
            _ => {
                for frame_idx in requested_frames {
                    clear_prefetch_in_flight(url_hash, frame_idx, prefetch_token);
                }
                clear_prefetch_url_in_flight(url_hash, prefetch_token);
            }
        }
    }

    /// Evict frames to make room for new data. The caller holds
    /// `frame_mutation_lock`, so the residency counts below are exact for the
    /// entire pass.
    fn evict_frames_locked(&self, needed_bytes: usize) {
        // AtomicU64 budget fast path: if there is already room for `needed_bytes`
        // under the frame budget (e.g. another thread released frames between the
        // failed try_allocate and this call), skip the iterate+sort entirely.
        let budget = self.budget.budget_for(CacheLayer::Frames);
        if self
            .frame_memory
            .load(Ordering::Relaxed)
            .saturating_add(needed_bytes)
            <= budget
        {
            return;
        }

        let used = self.frame_memory.load(Ordering::Relaxed);
        let bytes_to_free = used.saturating_add(needed_bytes).saturating_sub(budget);
        let picker_visible = self.picker_visible_canonical_hashes();
        let chat_visible = self.chat_visible_canonical_hashes();

        // Evict non-visible frames first, then surplus frames from visible URLs.
        // The displayed frame of every visible picker or chat URL is protected.
        let mut frames: Vec<(FrameKey, u64, usize)> = self
            .frames
            .iter()
            .map(|r| {
                let key = *r.key();
                let value = r.value();
                (
                    key,
                    value.last_accessed_ms.load(Ordering::Relaxed),
                    value.size_bytes,
                )
            })
            .collect();
        frames.sort_by_key(|(key, accessed, _)| {
            (
                picker_visible.contains(&key.url_hash) || chat_visible.contains(&key.url_hash),
                *accessed,
            )
        });

        // Evict until we have enough room
        let mut freed = 0usize;
        let mut demotion_checked = false;
        for (key, _, _size) in frames {
            if freed >= bytes_to_free {
                break;
            }
            // Offscreen frames are gone. Demote whole visible full-cycle
            // animations before trimming visible frames one by one: LRU over a
            // cyclic playback order evicts exactly the frames due next.
            if !demotion_checked
                && (picker_visible.contains(&key.url_hash) || chat_visible.contains(&key.url_hash))
            {
                demotion_checked = true;
                while freed < bytes_to_free {
                    let Some(hash) = self.largest_visible_full_cycle_locked(
                        &picker_visible,
                        &chat_visible,
                        true,
                    ) else {
                        break;
                    };
                    freed += self.demote_full_cycle_locked(hash, None);
                }
                if freed >= bytes_to_free {
                    break;
                }
            }

            // Revalidate against the live picker registry while holding its
            // read guard through removal. A viewport publication cannot make
            // this URL visible between the check and the map mutation.
            let size_bytes = with_picker_visible_hash_sets(|inline, popout| {
                let is_visible = self.picker_sets_contain_canonical(inline, popout, key.url_hash)
                    || chat_visible.contains(&key.url_hash);
                let displayed_fallback = self
                    .fallback_frame_indices
                    .get(&key.url_hash)
                    .is_some_and(|fallback| *fallback == key.frame_idx);
                let url_frames = self
                    .frame_counts
                    .get(&key.url_hash)
                    .map_or(0, |count| *count);
                if is_visible && (url_frames <= 1 || displayed_fallback) {
                    None
                } else {
                    self.remove_frame(key)
                }
            });
            if let Some(size_bytes) = size_bytes {
                freed += size_bytes;
            }
        }

        crate::log_cache!(
            debug,
            freed_bytes = freed,
            needed_bytes,
            protected_picker_urls = picker_visible.len(),
            protected_chat_urls = chat_visible.len(),
            "Frame eviction complete"
        );

        // Clean up expired texture cache entries after eviction
        cleanup_texture_cache();
    }

    /// Free hot frames that have not been touched for `ttl`.
    ///
    /// Idle retention: instead of holding decoded frames at the budget
    /// high-water mark forever, frames for animations that are no longer
    /// playing (scrolled away, channel closed, picker closed) are released and
    /// refilled on demand from the compressed decoded-source cache.
    pub fn evict_idle_frames(&self, ttl: std::time::Duration) -> usize {
        let _mutation_guard = self.frame_mutation_lock.lock();
        let cutoff = echo_core::unix_now_ms().saturating_sub(ttl.as_millis() as u64);
        let mut remaining_by_url = std::collections::HashMap::new();
        let mut expired = Vec::new();
        for entry in self.frames.iter() {
            let key = *entry.key();
            *remaining_by_url.entry(key.url_hash).or_insert(0usize) += 1;
            if entry.value().last_accessed_ms.load(Ordering::Relaxed) < cutoff {
                expired.push(key);
            }
        }

        let mut freed = 0usize;
        let mut removed = 0usize;
        for key in expired {
            let size_bytes = with_picker_visible_hash_sets(|inline, popout| {
                let is_visible = self.picker_sets_contain_canonical(inline, popout, key.url_hash);
                let displayed_fallback = self
                    .fallback_frame_indices
                    .get(&key.url_hash)
                    .is_some_and(|fallback| *fallback == key.frame_idx);
                if is_visible
                    && (remaining_by_url.get(&key.url_hash).copied().unwrap_or(0) <= 1
                        || displayed_fallback)
                {
                    None
                } else {
                    self.remove_frame(key)
                }
            });
            if let Some(size_bytes) = size_bytes {
                if let Some(count) = remaining_by_url.get_mut(&key.url_hash) {
                    *count = count.saturating_sub(1);
                }
                freed += size_bytes;
                removed += 1;
            }
        }

        if removed > 0 {
            cleanup_texture_cache();
            crate::log_cache!(
                debug,
                removed,
                freed_bytes = freed,
                "Idle frame TTL eviction complete"
            );
        }
        freed
    }

    /// Check if there are any VISIBLE animations currently being rendered
    ///
    /// This checks the emote_renderer's visible metadata tracking, which is populated
    /// during each render pass. Returns true only if animated emotes are actually
    /// on screen, preventing animation ticks when emotes are loaded but not visible.
    ///
    /// Animation ticks run only while this is true, i.e. while an animation is
    /// on screen, not merely loaded.
    pub fn has_visible_animations(&self) -> bool {
        visible_metadata_count() > 0
    }

    /// CPU pixel bytes hot frames still hold. A releasable frame drops its
    /// pixels after its first atlas upload, so this is mostly frames decoded
    /// ahead of display and frames that keep their pixels.
    pub fn resident_pixel_bytes(&self) -> usize {
        self.frames
            .iter()
            .filter(|entry| entry.value().render_image.has_resident_pixels())
            .map(|entry| entry.value().render_image.byte_len(0))
            .sum()
    }

    /// `(frames, animation_urls, picker_protected_frames,
    /// picker_protected_bytes, maximum_frames_per_url, urls_over_limit,
    /// render_images, global_entry_limit, entries_over_global_limit)` for the
    /// opt-in process-memory diagnostic.
    ///
    /// This scan is intentionally kept out of the normal cache-stat hot path.
    /// `ECHO_MEM_REPORT_SECS` calls it only at the requested reporting interval.
    pub fn diagnostic_residency_counts(&self) -> FrameResidencyCounts {
        let picker_visible = self.picker_visible_canonical_hashes();
        let mut picker_protected_frames = 0usize;
        let mut picker_protected_bytes = 0usize;
        for entry in self.frames.iter() {
            if picker_visible.contains(&entry.key().url_hash) {
                picker_protected_frames += 1;
                picker_protected_bytes += entry.value().size_bytes;
            }
        }
        let mut maximum_frames_per_url = 0usize;
        let mut urls_over_limit = 0usize;
        for entry in self.frame_counts.iter() {
            let frames = *entry.value();
            maximum_frames_per_url = maximum_frames_per_url.max(frames);
            let limit =
                self.resident_frame_limit(*entry.key(), picker_visible.contains(entry.key()));
            if frames > limit {
                urls_over_limit += 1;
            }
        }
        let frame_entries = self.frames.len();
        let entry_limit = Self::hot_frame_entry_limit(Self::picker_visible_animation_upper_bound());
        FrameResidencyCounts {
            frames: frame_entries,
            urls: self.frame_counts.len(),
            picker_protected_frames,
            picker_protected_bytes,
            max_frames_per_url: maximum_frames_per_url,
            urls_over_limit,
            render_images: self.texture_frame_counts.len(),
            entry_limit,
            entries_over_limit: frame_entries.saturating_sub(entry_limit),
        }
    }
}

/// Hot-frame residency for the opt-in memory diagnostic.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameResidencyCounts {
    /// Resident decoded frames.
    pub frames: usize,
    /// Animations with at least one resident frame.
    pub urls: usize,
    /// Frames of animations the open picker shows.
    pub picker_protected_frames: usize,
    pub picker_protected_bytes: usize,
    pub max_frames_per_url: usize,
    /// Animations holding more frames than their resident limit.
    pub urls_over_limit: usize,
    /// Animations with frames uploaded as GPU textures.
    pub render_images: usize,
    /// The global hot-frame entry limit at the current picker load.
    pub entry_limit: usize,
    pub entries_over_limit: usize,
}

/// How many decoded frames of an animation stay hot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationResidency {
    /// A sliding window of `frames` around playback. The loader picks it from
    /// the workload: two for dense picker playback, eight for ordinary emotes,
    /// 32 for a large name-paint window.
    Sliding { frames: usize },
    /// The complete cycle; `fallback_frames` is the sliding window it returns
    /// to if byte or entry pressure demotes it while visible.
    FullCycle { fallback_frames: usize },
}

/// Which frame [`FrameLayer::animated_frame`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameQuery {
    /// The frame for the current animation time, registering the animation
    /// as visible and scheduling refills; a nearby resident frame is held
    /// when that one is missing.
    Current,
    /// The current-time frame or a resident stand-in, without admitting the
    /// animation or scheduling refill work.
    Resident,
    /// A resident frame near frame 0, for still display; never advances.
    Stable,
}

/// One displayed frame of an animation.
#[derive(Clone)]
pub struct AnimatedFrame {
    pub image: Arc<RenderImage>,
    pub metadata: Arc<AnimationMetadata>,
    /// The resident frame actually returned, which differs from the
    /// wall-clock target when a fallback is held.
    pub index: u16,
    /// Whether `index` is the wall-clock target rather than a held fallback.
    pub exact: bool,
}

/// Decoded BGRA pixels of one animation frame.
pub struct FramePixels {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// How [`FrameLayer::insert_frame_with`] admits a frame.
#[derive(Default)]
pub struct FrameInsertOptions {
    /// Insert only while this refill token still owns the animation.
    pub prefetch_token: Option<u64>,
    /// Release the CPU pixels once a window has uploaded them to its sprite
    /// atlas; this source rebuilds them for any later upload (another window,
    /// or after the GPU device is lost).
    pub pixel_source: Option<RenderImagePixelSource>,
}

#[derive(Default)]
pub struct FramePurgeStats {
    pub frames_removed: usize,
    pub frame_bytes: usize,
    pub metadata_removed: bool,
    pub metadata_bytes: usize,
}

/// Whether `prefetch_token` still owns this canonical animation's refill window.
pub fn prefetch_token_is_current(url_hash: u64, prefetch_token: u64) -> bool {
    PREFETCH_URL_IN_FLIGHT
        .get(&url_hash)
        .is_some_and(|owner| *owner == prefetch_token)
}

/// Remove a frame marker only when it still belongs to this refill task.
pub fn clear_prefetch_in_flight(url_hash: u64, frame_idx: u16, prefetch_token: u64) {
    PREFETCH_IN_FLIGHT.remove_if(&(url_hash, frame_idx), |_, owner| *owner == prefetch_token);
}

/// Remove multiple frames from the in-flight prefetch set.
pub fn clear_prefetch_in_flight_batch(url_hash: u64, frame_indices: &[u16], prefetch_token: u64) {
    for &frame_idx in frame_indices {
        clear_prefetch_in_flight(url_hash, frame_idx, prefetch_token);
    }
}

/// Release the per-animation guard only when this task still owns it.
pub fn clear_prefetch_url_in_flight(url_hash: u64, prefetch_token: u64) {
    PREFETCH_URL_IN_FLIGHT.remove_if(&url_hash, |_, owner| *owner == prefetch_token);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{
        PickerViewport, replace_picker_visible_hashes, reset_picker_visibility_publication,
    };
    use std::collections::HashSet;

    /// Serializes tests that mutate the process-global picker visibility.
    static PICKER_VISIBILITY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PickerVisibilityReset {
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl PickerVisibilityReset {
        fn acquire() -> Self {
            let guard = PICKER_VISIBILITY_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            reset_picker_visibility_publication();
            Self { _guard: guard }
        }
    }

    impl Drop for PickerVisibilityReset {
        fn drop(&mut self) {
            reset_picker_visibility_publication();
        }
    }

    #[test]
    fn test_frame_layer_basic() {
        let budget = Arc::new(BudgetManager::with_budget(1_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = FrameLayer::new(budget, stats);

        let url = "https://cdn.7tv.app/emote/123/1x.webp";
        let metadata = AnimationMetadata::new(vec![100, 100, 100, 100]);

        layer.register_animation(url, metadata);
        assert!(layer.has_metadata(url));
    }

    #[test]
    fn test_animation_time() {
        let t1 = current_animation_time_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = current_animation_time_ms();
        assert!(t2 > t1);
    }

    #[test]
    fn picker_visibility_fast_path_preserves_alias_contract() {
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000_000)),
            Arc::new(CacheStats::new()),
        );
        let canonical = url_key("https://picker.example.test/canonical.webp");
        let alias = url_key("https://picker.example.test/alias.webp");
        let unknown = url_key("https://picker.example.test/unknown.webp");
        layer.url_aliases.insert(alias, canonical);

        let exact = HashSet::from([canonical]);
        assert!(layer.picker_visibility_contains_canonical_hash(&exact, canonical));
        assert!(!layer.picker_visibility_contains_canonical_hash(&exact, unknown));

        let alias_only = HashSet::from([alias]);
        assert!(layer.picker_visibility_contains_canonical_hash(&alias_only, canonical));
        assert!(!layer.picker_visibility_contains_canonical_hash(&alias_only, unknown));
        assert!(!layer.picker_visibility_contains_canonical_hash(&alias_only, alias));
        assert!(!layer.picker_sets_contain_canonical(&alias_only, &HashSet::new(), alias));
    }

    #[test]
    fn lookahead_insert_does_not_replace_displayed_fallback() {
        let budget = Arc::new(BudgetManager::with_budget(1_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = FrameLayer::new(budget, stats);
        let url = "https://cdn.example.test/animated.webp";
        layer.register_animation(url, AnimationMetadata::new(vec![100, 100, 100]));

        layer.insert_frame(url, 1, vec![0, 0, 0, 255], 1, 1);
        let hash = url_key(url);
        // Simulate frame 1 having been selected for display before the seam
        // lookahead arrives.
        layer.fallback_frame_indices.insert(hash, 1);
        layer.insert_frame(url, 0, vec![0, 0, 0, 255], 1, 1);

        assert_eq!(
            layer.fallback_frame_indices.get(&hash).map(|entry| *entry),
            Some(1),
            "a prefetched future/seam frame must not hijack the displayed fallback"
        );
    }

    #[test]
    fn picker_entry_limit_expands_only_while_visible() {
        assert_eq!(FrameLayer::hot_frame_entry_limit(0), MAX_HOT_FRAME_ENTRIES);
        assert_eq!(
            FrameLayer::hot_frame_entry_limit(320),
            4_736,
            "320 visible picker URLs retain a two-frame runway without crossing the hard cap"
        );
        assert_eq!(
            FrameLayer::hot_frame_entry_limit(10_000),
            PICKER_MAX_HOT_FRAME_ENTRIES
        );
    }

    #[test]
    fn dense_visible_picker_keeps_two_frame_runways_without_entry_eviction() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let budget = Arc::new(BudgetManager::with_budget(10_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = FrameLayer::new(budget, stats);
        let urls = (0..320)
            .map(|index| format!("https://picker.example.test/{index}.webp"))
            .collect::<Vec<_>>();
        let hashes = urls.iter().map(|url| url_key(url)).collect::<HashSet<_>>();
        replace_picker_visible_hashes(PickerViewport::Inline, HashSet::new(), hashes);

        for (index, url) in urls.iter().enumerate() {
            layer.register_animation(url, AnimationMetadata::new(vec![100, 100]));
            layer.insert_frame(url, 0, vec![index as u8, 0, 0, 255], 1, 1);
            layer.insert_frame(url, 1, vec![index as u8, 1, 0, 255], 1, 1);
        }

        assert_eq!(layer.frames.len(), 640);
        for url in &urls {
            assert!(layer.has_frame(url, 0));
            assert!(layer.has_frame(url, 1));
            assert_eq!(
                layer
                    .resident_frame_indices
                    .get(&url_key(url))
                    .map(|indices| indices.len()),
                Some(2),
                "every dense picker URL keeps a bounded two-entry replacement index"
            );
        }
        assert_eq!(layer.frame_memory.load(Ordering::Relaxed), 640 * 4);
        replace_picker_visible_hashes(PickerViewport::Inline, HashSet::new(), HashSet::new());
        {
            let _mutation_guard = layer.frame_mutation_lock.lock();
            layer.evict_frame_entries_to_limit_locked(FrameKey::new(url_key(&urls[0]), 0));
        }
        assert_eq!(layer.frames.len(), 640);
        println!("dense_picker urls=320 ready_frames=640 below_baseline_limit=4096");
    }

    #[test]
    fn entry_limit_keeps_displayed_frame_of_visible_chat_animation() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(10_000_000)),
            Arc::new(CacheStats::new()),
        );
        for index in 0..MAX_HOT_FRAME_ENTRIES {
            let url = format!("https://chat.example.test/offscreen-{index}.webp");
            layer.register_animation(&url, AnimationMetadata::new(vec![100]));
            layer.insert_frame(&url, 0, vec![index as u8, 0, 0, 255], 1, 1);
            layer
                .frames
                .get(&FrameKey::new(url_key(&url), 0))
                .unwrap()
                .last_accessed_ms
                .store(0, Ordering::Relaxed);
        }
        let visible = "https://chat.example.test/visible.webp";
        layer.register_animation(visible, AnimationMetadata::new(vec![100, 100]));
        layer.insert_frame(visible, 0, vec![0, 1, 0, 255], 1, 1);
        layer.insert_frame(visible, 1, vec![1, 1, 0, 255], 1, 1);

        assert_eq!(layer.frames.len(), MAX_HOT_FRAME_ENTRIES);
        assert!(
            layer.has_frame(visible, 0),
            "the displayed frame of an on-screen chat animation outlives offscreen frames"
        );
        assert!(layer.has_frame(visible, 1));
    }

    #[test]
    fn byte_pressure_demotes_largest_visible_full_cycle_whole() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        // 40% of 1_000 bytes: a hundred 1x1 frames.
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000)),
            Arc::new(CacheStats::new()),
        );
        let insert_cycle = |url: &str, frames: u16| {
            layer.register_animation_with(
                url,
                None,
                AnimationMetadata::new(vec![100; frames as usize]),
                AnimationResidency::FullCycle {
                    fallback_frames: DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION,
                },
            );
            for frame in 0..frames {
                layer.insert_frame(url, frame, vec![frame as u8, 0, 0, 255], 1, 1);
            }
        };
        let large = "https://chat.example.test/large.webp";
        let small = "https://chat.example.test/small.webp";
        let arriving = "https://chat.example.test/arriving.webp";
        insert_cycle(large, 60);
        insert_cycle(small, 30);
        insert_cycle(arriving, 30);

        let resident = |url: &str| {
            layer
                .frame_counts
                .get(&url_key(url))
                .map_or(0, |count| *count)
        };
        assert!(resident(large) <= DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION);
        assert_eq!(resident(small), 30, "smaller cycles stay whole");
        assert_eq!(resident(arriving), 30);
        assert_eq!(
            layer.frame_limits.get(&url_key(large)).map(|limit| *limit),
            Some(DEFAULT_MAX_RESIDENT_FRAMES_PER_ANIMATION)
        );
        assert!(
            !layer
                .full_cycle_fallback_limits
                .contains_key(&url_key(large))
        );
    }

    #[test]
    fn full_cycle_picker_animation_keeps_every_frame() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000_000)),
            Arc::new(CacheStats::new()),
        );
        let url = "https://picker.example.test/full-cycle.webp";
        let hash = url_key(url);
        replace_picker_visible_hashes(
            PickerViewport::Inline,
            HashSet::new(),
            HashSet::from([hash]),
        );
        assert!(layer.admits_full_cycle(12, 12 * 4));
        assert!(!layer.admits_full_cycle(12, ANIMATION_FULL_CYCLE_MAX_BYTES + 1));
        layer.register_animation_with(
            url,
            None,
            AnimationMetadata::new(vec![100; 12]),
            AnimationResidency::FullCycle {
                fallback_frames: PICKER_MAX_RESIDENT_FRAMES_PER_ANIMATION,
            },
        );
        for frame in 0..12u16 {
            layer.insert_frame(url, frame, vec![frame as u8, 0, 0, 255], 1, 1);
        }
        assert_eq!(layer.frame_counts.get(&hash).map(|count| *count), Some(12));
        assert_eq!(layer.resident_frame_limit(hash, true), 12);
    }

    #[test]
    fn two_frame_seam_probe_never_jumps_to_early_frame_zero() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let budget = Arc::new(BudgetManager::with_budget(1_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = FrameLayer::new(budget, stats);
        let url = "https://picker.example.test/seam.webp";
        let hash = url_key(url);
        replace_picker_visible_hashes(
            PickerViewport::Popout,
            HashSet::new(),
            HashSet::from([hash]),
        );
        layer.register_animation(url, AnimationMetadata::new(vec![100; 10]));
        layer.insert_frame(url, 5, vec![5, 0, 0, 255], 1, 1);
        layer.fallback_frame_indices.insert(hash, 5);
        layer.insert_frame(url, 6, vec![6, 0, 0, 255], 1, 1);
        layer.insert_frame(url, 0, vec![0, 0, 0, 255], 1, 1);

        let (_, displayed) = layer
            .probe_fallback_frame_with_index(hash, 8, 10)
            .expect("the held pre-seam frame remains renderable");
        assert_eq!(layer.frames.len(), 2);
        assert_eq!(
            displayed, 5,
            "hold the displayed pose instead of an early loop restart"
        );
        assert!(
            layer.has_frame(url, 0),
            "the actual loop-wrap frame remains available"
        );
        // Give frame zero a long authored duration so the normal clock-based
        // resident getter deterministically selects it for this wrap check.
        layer.register_animation(url, AnimationMetadata::new(vec![u32::MAX; 10]));
        let (_, _, wrapped_frame, exact) = layer
            .get_resident_frame_with_metadata_frame_and_exactness(url)
            .unwrap();
        assert_eq!(wrapped_frame, 0);
        assert!(exact);
        println!("seam held_frame=5 early_zero_transitions=0 actual_wrap_frame=0 exact=true");
    }

    #[test]
    fn stable_frame_reports_the_resident_index_it_returns() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000_000)),
            Arc::new(CacheStats::new()),
        );
        let url = "https://paint.example.test/stable.webp";
        layer.register_animation(url, AnimationMetadata::new(vec![100; 10]));
        assert!(
            layer
                .get_stable_frame_with_metadata_and_index(url)
                .is_none()
        );

        layer.insert_frame(url, 9, vec![9, 0, 0, 255], 1, 1);
        let (_, metadata, frame_idx) = layer
            .get_stable_frame_with_metadata_and_index(url)
            .expect("a resident frame serves as the stable poster");
        assert_eq!(metadata.frame_count, 10);
        assert_eq!(
            frame_idx, 9,
            "the probe falls back from frame 0, so the index must name the frame returned"
        );
    }

    #[test]
    fn idle_cleanup_preserves_visible_held_pose_over_future_frame() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000_000)),
            Arc::new(CacheStats::new()),
        );
        let url = "https://picker.example.test/idle.webp";
        let hash = url_key(url);
        replace_picker_visible_hashes(
            PickerViewport::Popout,
            HashSet::new(),
            HashSet::from([hash]),
        );
        layer.register_animation(url, AnimationMetadata::new(vec![100; 10]));
        layer.insert_frame(url, 5, vec![5, 0, 0, 255], 1, 1);
        layer.insert_frame(url, 0, vec![0, 0, 0, 255], 1, 1);
        // Only the held pose expires; the newer speculative frame must not
        // replace it merely because cleanup visits the older entry first.
        layer
            .frames
            .get(&FrameKey::new(hash, 5))
            .unwrap()
            .last_accessed_ms
            .store(0, Ordering::Relaxed);
        layer
            .frames
            .get(&FrameKey::new(hash, 0))
            .unwrap()
            .last_accessed_ms
            .store(u64::MAX, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(2));
        layer.evict_idle_frames(std::time::Duration::ZERO);
        assert!(layer.has_frame(url, 5));
        assert_eq!(
            layer
                .probe_fallback_frame_with_index(hash, 8, 10)
                .unwrap()
                .1,
            5
        );
    }

    #[test]
    #[ignore = "deterministic local rotation microbenchmark"]
    fn picker_rotation_microbenchmark_320_urls() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(10_000_000)),
            Arc::new(CacheStats::new()),
        );
        let urls = (0..320)
            .map(|index| format!("https://picker-bench.example.test/{index}.webp"))
            .collect::<Vec<_>>();
        let hashes = urls.iter().map(|url| url_key(url)).collect::<HashSet<_>>();
        replace_picker_visible_hashes(PickerViewport::Inline, HashSet::new(), hashes);
        for (index, url) in urls.iter().enumerate() {
            layer.register_animation(url, AnimationMetadata::new(vec![100; 128]));
            layer.insert_frame(url, 0, vec![index as u8, 0, 0, 255], 1, 1);
            layer.insert_frame(url, 1, vec![index as u8, 1, 0, 255], 1, 1);
            layer.record_displayed_frame(FrameKey::new(url_key(url), 1));
        }
        let started = Instant::now();
        for step in 0..64u16 {
            for (index, url) in urls.iter().enumerate() {
                let hash = url_key(url);
                let previous = (step + 1) % 128;
                let next = (step + 2) % 128;
                layer.record_displayed_frame(FrameKey::new(hash, previous));
                layer.insert_frame(url, next, vec![index as u8, next as u8, 0, 255], 1, 1);
            }
        }
        let elapsed = started.elapsed();
        assert_eq!(layer.frames.len(), 640);
        assert_eq!(layer.frame_counts.len(), 320);
        assert_eq!(layer.resident_frame_indices.len(), 320);
        println!(
            "picker_rotation_microbenchmark urls=320 steps=64 updates={} elapsed_ms={:.3}",
            320 * 64,
            elapsed.as_secs_f64() * 1_000.0
        );
    }

    #[test]
    fn resident_index_stays_coherent_through_replacement_and_purge() {
        let _visibility_reset = PickerVisibilityReset::acquire();
        let layer = FrameLayer::new(
            Arc::new(BudgetManager::with_budget(1_000_000)),
            Arc::new(CacheStats::new()),
        );
        let url = "https://picker.example.test/index-lifecycle.webp";
        let hash = url_key(url);
        replace_picker_visible_hashes(
            PickerViewport::Inline,
            HashSet::new(),
            HashSet::from([hash]),
        );
        layer.register_animation(url, AnimationMetadata::new(vec![100; 3]));
        for frame in [0, 1, 1] {
            layer.insert_frame(url, frame, vec![frame as u8, 0, 0, 255], 1, 1);
        }
        assert_eq!(layer.resident_frame_indices.get(&hash).unwrap().len(), 2);
        layer.record_displayed_frame(FrameKey::new(hash, 1));
        layer.insert_frame(url, 2, vec![2, 0, 0, 255], 1, 1);
        assert!(!layer.has_frame(url, 0));
        assert!(layer.has_frame(url, 1));
        assert!(layer.has_frame(url, 2));
        assert_eq!(
            layer.resident_frame_indices.get(&hash).unwrap().as_slice(),
            &[1, 2]
        );
        assert_eq!(layer.purge_url(url).frames_removed, 2);
        assert!(!layer.resident_frame_indices.contains_key(&hash));
        assert!(!layer.frame_counts.contains_key(&hash));
        assert!(!layer.fallback_frame_indices.contains_key(&hash));
    }
}
