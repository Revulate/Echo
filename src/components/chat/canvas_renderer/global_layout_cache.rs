//! Global shared layout cache for memory optimization
//!
//! This module provides a centralized layout cache that is shared across all split panes,
//! reducing memory usage by 60-80% compared to per-split caches when showing the same
//! channel in multiple splits.
//!
//! ## Architecture
//!
//! Previously: 4 splits × 2500 entries = 10,000 cached layouts (potential)
//! Now: 1 global cache, RAM-tiered capacity (see `global_cache_capacity`),
//! with evicted-message layouts reclaimed by the maintenance slot.
//!
//! Benefits:
//! - Same channel in multiple splits at same width: single layout stored
//! - Different widths still cached with composite key (message_id + quantized_width)
//! - Thread-safe with DashMap for concurrent access from multiple splits
//! - LRU eviction via parking_lot mutex (deferred from hot path)

use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Global version counter bumped whenever layouts are invalidated (settings change).
/// Painters compare against their local snapshot to detect when a full rebuild is needed.
static INVALIDATION_VERSION: AtomicU64 = AtomicU64::new(0);

/// Last observed backend layout settings version.
///
/// This lets the renderer invalidate layout caches even if the settings event broadcast
/// isn't being processed (e.g., during heavy load), ensuring sliders update live.
static LAST_SEEN_LAYOUT_SETTINGS_VERSION: AtomicU64 = AtomicU64::new(0);

/// Sequence of the newest entry in `LAYOUT_EVICTION_LOG`.
static LAYOUT_EVICTION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Recent message-scoped evictions. Each split replays the entries it has not
/// seen against its local cache instead of dropping every layout it holds.
static LAYOUT_EVICTION_LOG: Mutex<VecDeque<(u64, LayoutEviction)>> = Mutex::new(VecDeque::new());

const LAYOUT_EVICTION_LOG_CAPACITY: usize = 4_096;

#[derive(Clone, Debug)]
pub enum LayoutEviction {
    Message(Arc<str>),
    /// Every layout that renders a link to this URL.
    LinksTo(Arc<str>),
    /// Every layout of this channel, e.g. after its badge set changed.
    Channel(Arc<str>),
}

impl LayoutEviction {
    fn covers(&self, layout: &CanvasMessageLayout) -> bool {
        match self {
            Self::Message(message_id) => message_id.as_ref() == layout.message_id_shared.as_ref(),
            Self::LinksTo(url) => layout.links_to(url),
            Self::Channel(channel) => *channel == layout.channel,
        }
    }
}

pub enum LayoutEvictions {
    Replay {
        evictions: Vec<LayoutEviction>,
        latest: u64,
    },
    /// Entries after the caller's sequence were dropped; clear everything.
    Overflow { latest: u64 },
}

fn record_layout_evictions(evictions: impl IntoIterator<Item = LayoutEviction>) {
    let mut log = LAYOUT_EVICTION_LOG.lock();
    let mut seq = LAYOUT_EVICTION_SEQ.load(Ordering::Acquire);
    for eviction in evictions {
        seq += 1;
        log.push_back((seq, eviction));
    }
    while log.len() > LAYOUT_EVICTION_LOG_CAPACITY {
        log.pop_front();
    }
    LAYOUT_EVICTION_SEQ.store(seq, Ordering::Release);
}

#[inline]
pub fn layout_eviction_seq() -> u64 {
    LAYOUT_EVICTION_SEQ.load(Ordering::Acquire)
}

/// Message-scoped evictions recorded after `since`.
pub fn layout_evictions_since(since: u64) -> LayoutEvictions {
    let log = LAYOUT_EVICTION_LOG.lock();
    let latest = LAYOUT_EVICTION_SEQ.load(Ordering::Acquire);
    if since >= latest {
        return LayoutEvictions::Replay {
            evictions: Vec::new(),
            latest,
        };
    }
    if log.front().is_none_or(|(oldest, _)| *oldest > since + 1) {
        return LayoutEvictions::Overflow { latest };
    }
    let start = log.partition_point(|(seq, _)| *seq <= since);
    LayoutEvictions::Replay {
        evictions: log
            .range(start..)
            .map(|(_, eviction)| eviction.clone())
            .collect(),
        latest,
    }
}

/// Epoch that changes whenever every cached layout was invalidated. Async
/// layout jobs started before it moved must not publish; message-scoped
/// evictions are fenced per layout by `layout_valid_through`.
#[inline]
pub fn layout_content_epoch() -> u64 {
    INVALIDATION_VERSION.load(Ordering::Acquire)
}

/// Whether a layout shaped or validated at eviction sequence `since` is still
/// current: `Some(sequence it is now valid through)`, or `None` when a later
/// eviction covers it or the log no longer reaches back to `since`.
pub fn layout_valid_through(since: u64, layout: &CanvasMessageLayout) -> Option<u64> {
    if since >= LAYOUT_EVICTION_SEQ.load(Ordering::Acquire) {
        return Some(since);
    }
    let log = LAYOUT_EVICTION_LOG.lock();
    let latest = LAYOUT_EVICTION_SEQ.load(Ordering::Acquire);
    valid_through_log(&log, latest, since, layout)
}

fn valid_through_log(
    log: &VecDeque<(u64, LayoutEviction)>,
    latest: u64,
    since: u64,
    layout: &CanvasMessageLayout,
) -> Option<u64> {
    if since >= latest {
        return Some(since);
    }
    if log.front().is_none_or(|(oldest, _)| *oldest > since + 1) {
        return None;
    }
    let start = log.partition_point(|(seq, _)| *seq <= since);
    (!log
        .range(start..)
        .any(|(_, eviction)| eviction.covers(layout)))
    .then_some(latest)
}

use super::layout::{CanvasMessageLayout, LayoutCacheKey, LayoutSegment};

static LAST_INVALIDATION_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// Global layout cache capacity, tiered by total system RAM.
///
/// A `CanvasMessageLayout` entry is about 5.3 KB resident for a typical row
/// (768 B inline plus exact-size segment, text-span, hitbox and paint lists;
/// it was about 12.9 KB while those lists were inline `SmallVec`s). Live
/// demand is bounded by `open_channels × MAX_CACHED_MESSAGES × width
/// variants`, and evicted-message layouts are reclaimed eagerly by
/// `drain_evicted_message_layouts` from the maintenance slot, so this tier is
/// only a backstop rather than the primary bound:
/// - <=8GB  → 2,000 entries (~11MB at ~5.3 KB/entry)
/// - <=16GB → 4,000 entries (~21MB)
/// - >16GB  → 6,000 entries (~32MB)
///
/// The value is computed once at `LazyLock` init via the shared prewarm probe
/// from `cache::budget::prewarmed_total_memory_bytes()`, so no extra sysinfo
/// allocation is needed here.
fn global_cache_capacity() -> usize {
    let total_gb =
        (crate::cache::budget::prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
    if total_gb <= 8.5 {
        2_000
    } else if total_gb <= 16.5 {
        4_000
    } else {
        6_000
    }
}

/// Eviction target when cache is full (percentage of capacity to keep)
const EVICTION_TARGET_PERCENT: f64 = 0.85;

/// Saturating ceiling for an entry's frequency estimate.
///
/// Caps the TinyLFU-style frequency counter so a single very hot key cannot become
/// effectively un-evictable and so the periodic aging halving keeps numbers small.
const FREQ_SATURATION: u32 = 16;

/// Global insert count at which we halve every entry's frequency estimate.
///
/// This is the "aging" step of W-TinyLFU: without it, keys that were hot early in a
/// session would dominate forever. Halving on a coarse insert cadence keeps the
/// frequency signal biased toward the *recent* working set while staying cheap.
const FREQ_AGING_INSERT_INTERVAL: u64 = 8_192;

/// Global layout cache shared across all split panes
pub static GLOBAL_LAYOUT_CACHE: LazyLock<GlobalLayoutCache> =
    LazyLock::new(|| GlobalLayoutCache::new(global_cache_capacity()));

/// Monotonic access epoch source. Each cache touch (read hit or insert) claims a
/// fresh, strictly-increasing value, giving us a total recency ordering across all
/// entries without any shared lock.
static ACCESS_EPOCH: AtomicU64 = AtomicU64::new(0);

#[inline]
fn next_access_epoch() -> u64 {
    ACCESS_EPOCH.fetch_add(1, Ordering::Relaxed)
}

/// Monotonic insert counter, used only to drive periodic frequency aging.
static INSERT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Estimate the relative memory cost of a layout for cost-aware eviction.
///
/// We have no exact byte size on `CanvasMessageLayout`, but its heavy, heap-bearing
/// fields (shaped lines, paint commands, hitboxes, source text, char positions) are a
/// good proxy. Bigger layouts are cheaper to recompute-per-byte-reclaimed, so eviction
/// prefers shedding them first when frequency/recency are otherwise comparable. The
/// weight is bucketed into a small integer to keep scoring arithmetic trivial.
#[inline]
fn cost_weight_for(layout: &CanvasMessageLayout) -> u32 {
    let approx = layout.segments.len()
        + layout.shaped_lines.len() * 4
        + layout.text_paint_commands.len()
        + layout.image_paint_commands.len()
        + layout.hitbox_regions.len()
        + layout.source_char_positions.len() / 8
        + layout.source_text.len() / 32;

    match approx {
        0..=8 => 1,
        9..=24 => 2,
        25..=64 => 3,
        _ => 4,
    }
}

/// Cache entry: the shared layout plus lock-free TinyLFU-style admission signals.
///
/// The recency stamp is an `AtomicU64` rather than a locked LRU, whose
/// `try_write()` promotion would silently drop under reader contention and
/// degrade eviction to near-random. Updating an `AtomicU64` never blocks and
/// never drops, so hit recency is always recorded.
///
/// `freq` is a saturating, periodically-aged access-frequency estimate (the
/// W-TinyLFU "frequency" signal); `cost_weight` is a bucketed memory-size proxy.
/// Together they make eviction frequency- and cost-aware rather than the previous
/// size-blind pure-recency LRU.
struct CacheEntry {
    layout: Arc<CanvasMessageLayout>,
    last_access: AtomicU64,
    /// Saturating access-frequency estimate, capped at `FREQ_SATURATION`.
    freq: AtomicU32,
    /// Bucketed memory cost proxy (1..=4), computed once at insert.
    cost_weight: u32,
}

impl CacheEntry {
    /// Eviction score: lower = evict first.
    ///
    /// Frequency dominates (a frequently re-shaped layout is expensive to lose),
    /// recency breaks ties toward keeping the recent working set, and cost biases
    /// eviction toward larger layouts when the other signals are comparable.
    ///
    /// Encoded as a single sortable `u64` so the eviction snapshot can sort cheaply:
    /// `[freq:8 bits][recency:48 bits][inverse-cost:8 bits]` (most-significant first).
    #[inline]
    fn evict_score(&self) -> u64 {
        let freq = (self.freq.load(Ordering::Relaxed).min(FREQ_SATURATION) as u64) & 0xFF;
        let recency = self.last_access.load(Ordering::Relaxed) & 0x0000_FFFF_FFFF_FFFF;
        let inv_cost = (5u64.saturating_sub(self.cost_weight as u64)) & 0xFF;
        (freq << 56) | (recency << 8) | inv_cost
    }

    /// Record a hit: bump recency and saturating frequency.
    #[inline]
    fn touch(&self) {
        self.last_access
            .store(next_access_epoch(), Ordering::Relaxed);
        if self.freq.load(Ordering::Relaxed) < FREQ_SATURATION {
            self.freq.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Thread-safe global layout cache with lock-free read touches and serialized TinyLFU admission.
pub struct GlobalLayoutCache {
    /// Primary cache: (message_id, quantized_width) -> entry
    /// DashMap provides concurrent read access with minimal contention.
    /// Each value carries atomic recency + frequency stamps and a cost weight
    /// for frequency/recency/cost-aware eviction ordering.
    layouts: DashMap<LayoutCacheKey, CacheEntry>,

    /// Serializes cache admissions so a replacement can be identified before capacity
    /// eviction. Reads do not acquire this lock; it only covers new layout
    /// publication path and keeps eviction outside DashMap shard guards.
    admission_lock: Mutex<()>,

    /// Maximum capacity before eviction triggers
    capacity: usize,
}

impl GlobalLayoutCache {
    /// Create a new global layout cache with specified capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            layouts: DashMap::with_capacity(capacity),
            admission_lock: Mutex::new(()),
            capacity,
        }
    }

    /// Get a cached layout if it exists
    /// Returns None if the layout is not cached
    #[inline]
    pub fn get(&self, key: &LayoutCacheKey) -> Option<Arc<CanvasMessageLayout>> {
        let entry = self.layouts.get(key)?;
        entry.touch();
        Some(Arc::clone(&entry.layout))
    }

    /// Get a cached layout without changing its recency or frequency estimate.
    ///
    /// Use this for probes such as fallback-height lookup and selection where a read must
    /// not affect cache admission. The returned `Arc` remains valid if another thread
    /// replaces or removes this entry after the DashMap read guard is released.
    #[inline]
    pub fn peek(&self, key: &LayoutCacheKey) -> Option<Arc<CanvasMessageLayout>> {
        self.layouts.get(key).map(|entry| Arc::clone(&entry.layout))
    }

    /// Whether a layout for `key` is present, without touching recency/frequency.
    /// Telemetry-only (pinned-layout probe); do not use as a hot-path lookup.
    #[inline]
    pub fn contains(&self, key: &LayoutCacheKey) -> bool {
        self.layouts.contains_key(key)
    }

    /// Entries currently resident, and the capacity they are bounded by.
    /// Diagnostics only (memory breakdown).
    pub fn occupancy(&self) -> (usize, usize) {
        (self.layouts.len(), self.capacity)
    }

    /// Replace an existing entry while preserving its accumulated admission history.
    ///
    /// The DashMap shard guard is dropped before this returns, so callers can safely run
    /// map-wide eviction after a miss.
    #[inline]
    fn replace_existing(
        &self,
        key: &LayoutCacheKey,
        layout: &Arc<CanvasMessageLayout>,
        cost_weight: u32,
    ) -> bool {
        let Some(mut existing) = self.layouts.get_mut(key) else {
            return false;
        };
        existing.layout = Arc::clone(layout);
        existing.cost_weight = cost_weight;
        true
    }

    /// Insert a layout into the cache
    /// If cache is at capacity, triggers frequency/recency/cost-aware eviction
    /// PERF: Accepts Arc to avoid deep-cloning CanvasMessageLayout (500-2000 bytes)
    pub fn insert(&self, key: LayoutCacheKey, layout: Arc<CanvasMessageLayout>) {
        let cost_weight = cost_weight_for(&layout);

        if self.replace_existing(&key, &layout, cost_weight) {
            return;
        }

        let _admission = self.admission_lock.lock();

        if self.replace_existing(&key, &layout, cost_weight) {
            return;
        }

        if self.layouts.len() >= self.capacity {
            self.evict();
        }

        let inserts = INSERT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if inserts.is_multiple_of(FREQ_AGING_INSERT_INTERVAL) && inserts != 0 {
            self.age_frequencies();
        }

        self.layouts.insert(
            key,
            CacheEntry {
                layout,
                last_access: AtomicU64::new(next_access_epoch()),
                freq: AtomicU32::new(1),
                cost_weight,
            },
        );
    }

    /// Halve every entry's frequency estimate (the W-TinyLFU aging step).
    ///
    /// Runs off the insert cadence, not the read hot path, so it never blocks lookups.
    fn age_frequencies(&self) {
        for entry in self.layouts.iter() {
            let f = entry.value().freq.load(Ordering::Relaxed);
            if f > 1 {
                entry.value().freq.store(f >> 1, Ordering::Relaxed);
            }
        }
    }

    /// Remove a layout from the cache
    /// Called when a message is deleted or channel is closed
    pub fn remove(&self, key: &LayoutCacheKey) {
        self.layouts.remove(key);
    }

    /// Clear all cached layouts
    /// Called during major state resets (e.g., settings changes that affect layout)
    pub fn clear(&self) {
        self.layouts.clear();
    }

    /// Evict only the layouts that contain an embed preview segment.
    ///
    /// Embed display-mode settings only change how messages with embeds are laid out,
    /// so wiping the entire cache forces every split to reflow plain text/image content
    /// for no reason. This retains all non-embed layouts and removes just the ones whose
    /// geometry depends on embed settings, leaving them to rebuild on next render.
    pub fn clear_embed_layouts(&self) {
        let embed_keys: Vec<LayoutCacheKey> = self
            .layouts
            .iter()
            .filter(|entry| {
                entry
                    .value()
                    .layout
                    .segments
                    .iter()
                    .any(|segment| matches!(segment, LayoutSegment::EmbedPreview { .. }))
            })
            .map(|entry| entry.key().clone())
            .collect();

        if embed_keys.is_empty() {
            return;
        }

        for key in &embed_keys {
            self.layouts.remove(key);
        }
    }

    /// Evict every cached layout (at any width) for the given message ids.
    ///
    /// Used when embed metadata finishes loading after a message was first
    /// laid out: the cached layout was built without an EmbedPreview segment
    /// (metadata wasn't available yet), so it must be evicted to rebuild with
    /// the embed. `clear_embed_layouts` cannot cover this case — it only
    /// matches layouts that already contain an embed segment.
    pub fn clear_layouts_for_messages<T>(&self, message_ids: &std::collections::HashSet<T>)
    where
        T: std::borrow::Borrow<str> + Eq + std::hash::Hash,
    {
        let keys: Vec<LayoutCacheKey> = self
            .layouts
            .iter()
            .filter(|entry| message_ids.contains(entry.key().message_id.as_ref()))
            .map(|entry| entry.key().clone())
            .collect();

        for key in &keys {
            self.layouts.remove(key);
        }
        record_layout_evictions(
            message_ids.iter().map(|id| {
                LayoutEviction::Message(Arc::from(std::borrow::Borrow::<str>::borrow(id)))
            }),
        );
    }

    /// Evict every layout that renders a link to `url`, here and (through the
    /// eviction log) in every split's local cache.
    pub fn clear_layouts_linking_to(&self, url: &str) {
        let keys: Vec<LayoutCacheKey> = self
            .layouts
            .iter()
            .filter(|entry| entry.value().layout.links_to(url))
            .map(|entry| entry.key().clone())
            .collect();

        for key in &keys {
            self.layouts.remove(key);
        }
        record_layout_evictions([LayoutEviction::LinksTo(Arc::from(url))]);
    }

    /// Evict every layout of `channel`, here and (through the eviction log)
    /// in every split's local cache.
    pub fn clear_layouts_for_channel(&self, channel: &str) {
        self.layouts
            .retain(|_, entry| entry.layout.channel.as_ref() != channel);
        record_layout_evictions([LayoutEviction::Channel(Arc::from(channel))]);
    }

    /// Remove every cached layout (at any width) for the given message ids,
    /// returning the number of entries removed.
    ///
    /// Unlike `clear_layouts_for_messages`, this is selective and deliberately
    /// does not bump the process-wide invalidation version. Callers either
    /// reclaim messages already evicted from their buffers or remove the
    /// corresponding split-local entries and schedule a targeted rebuild. One
    /// map scan serves the whole batch, avoiding per-message scans on ingest or
    /// decoration publication.
    pub fn remove_layouts_for_message_ids(
        &self,
        ids: &std::collections::HashSet<Arc<str>>,
    ) -> usize {
        if ids.is_empty() {
            return 0;
        }
        let keys: Vec<LayoutCacheKey> = self
            .layouts
            .iter()
            .filter(|entry| ids.contains(entry.key().message_id.as_ref()))
            .map(|entry| entry.key().clone())
            .collect();

        for key in &keys {
            self.layouts.remove(key);
        }
        keys.len()
    }

    /// Evict the lowest-scoring entries until below target (TinyLFU-style).
    ///
    /// The victim score combines, in priority order, access **frequency** (a
    /// saturating, periodically-aged estimate), **recency** (each entry's lock-free
    /// `last_access` epoch stamp), and a **cost** proxy (bucketed layout size).
    /// This replaces the previous size-blind pure-recency LRU: a layout that is
    /// re-shaped often is now retained over a once-touched neighbour even if the
    /// latter was accessed more recently, and large layouts are shed first when the
    /// frequency/recency signals are otherwise comparable.
    fn evict(&self) {
        let target_size = (self.capacity as f64 * EVICTION_TARGET_PERCENT) as usize;
        let current = self.layouts.len();
        if current <= target_size {
            return;
        }
        let to_remove = current - target_size;

        let mut entries: Vec<(LayoutCacheKey, u64)> = self
            .layouts
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().evict_score()))
            .collect();

        entries.sort_unstable_by_key(|(_, score)| *score);

        for (key, _) in entries.into_iter().take(to_remove) {
            self.layouts.remove(&key);
        }

        crate::log_cache!(
            debug,
            "Evicted layouts (TinyLFU freq/recency/cost): remaining={}, capacity={}",
            self.layouts.len(),
            self.capacity
        );
    }
}

/// Get a cached layout from the global cache
#[inline]
pub fn get_layout(key: &LayoutCacheKey) -> Option<Arc<CanvasMessageLayout>> {
    GLOBAL_LAYOUT_CACHE
        .get(key)
        .filter(built_for_current_canvas)
}

/// The cache is shared by every split and window; a layout shaped for another
/// window's scale factor selected a different emote source tier, and one shaped
/// for another split's moderation mode has a different row prefix.
#[inline]
fn built_for_current_canvas(layout: &Arc<CanvasMessageLayout>) -> bool {
    layout.scale_bits == super::layout_scale::current_bits()
        && layout.mod_buttons == super::layout_mode::mod_buttons()
}

/// Peek at a cached layout without changing its recency/frequency estimate.
///
/// This is for read-only painter probes; normal render-path lookups should use
/// [`get_layout`] so the eviction policy learns the active working set.
#[inline]
pub fn peek_layout(key: &LayoutCacheKey) -> Option<Arc<CanvasMessageLayout>> {
    GLOBAL_LAYOUT_CACHE
        .peek(key)
        .filter(built_for_current_canvas)
}

/// Insert a layout into the global cache
/// PERF: Accepts Arc to avoid deep-cloning CanvasMessageLayout (500-2000 bytes)
pub fn insert_layout(key: LayoutCacheKey, layout: Arc<CanvasMessageLayout>) {
    GLOBAL_LAYOUT_CACHE.insert(key, layout);
}

/// Ensure the global layout cache is invalidated when backend layout-affecting settings change.
///
/// Returns true if invalidation occurred.
#[inline]
pub fn maybe_invalidate_for_settings_change() -> bool {
    let current = crate::backend::settings::get_layout_settings_version();
    let last = LAST_SEEN_LAYOUT_SETTINGS_VERSION.load(Ordering::Acquire);
    if current == last {
        return false;
    }

    if LAST_SEEN_LAYOUT_SETTINGS_VERSION
        .compare_exchange(last, current, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        clear_all_layouts();
        super::relayout_trace::note_layout_invalidation("settings_version", 0);
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::SettingsLayout,
        );

        let now = crate::utils::unix_now_ms();
        let prev = LAST_INVALIDATION_LOG_MS.load(Ordering::Relaxed);
        if now.saturating_sub(prev) >= 500
            && LAST_INVALIDATION_LOG_MS
                .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            crate::log_ui!(
                debug,
                backend_layout_settings_version = current,
                "Global layout cache invalidated for settings change"
            );
        }

        return true;
    }
    false
}

/// Clear the entire global layout cache and bump the invalidation version.
/// Painters polling `get_invalidation_version()` will detect the change and rebuild.
#[track_caller]
pub fn clear_all_layouts() {
    super::relayout_trace::note_layout_invalidation("clear_all", 0);
    GLOBAL_LAYOUT_CACHE.clear();
    INVALIDATION_VERSION.fetch_add(1, Ordering::Release);
    LAST_SEEN_LAYOUT_SETTINGS_VERSION.store(
        crate::backend::settings::get_layout_settings_version(),
        Ordering::Release,
    );
}

/// Evict only embed-bearing layouts and bump the invalidation version.
///
/// Used when an embed display-mode setting changes: messages that contain an embed
/// preview must reflow to pick up the new geometry, but plain text/image layouts across
/// every split are unaffected and stay cached. Painters polling `get_invalidation_version()`
/// will rebuild the evicted (now-missing) embed layouts on their next render.
#[track_caller]
pub fn clear_embed_layouts() {
    super::relayout_trace::note_layout_invalidation("clear_embed", 0);
    GLOBAL_LAYOUT_CACHE.clear_embed_layouts();
    INVALIDATION_VERSION.fetch_add(1, Ordering::Release);
}

/// Current invalidation version. Painters compare this to their local snapshot
/// to detect when settings changed and a full layout rebuild is needed.
#[inline]
pub fn get_invalidation_version() -> u64 {
    INVALIDATION_VERSION.load(Ordering::Acquire)
}

/// Evict the cached layouts for specific messages in the global cache and,
/// through the eviction log, in every split's local cache.
///
/// Used when a message changes in place (deletion, confirmed-row replacement)
/// or its embed metadata arrives after the first layout.
#[track_caller]
pub fn clear_layouts_for_messages<T>(message_ids: &std::collections::HashSet<T>)
where
    T: std::borrow::Borrow<str> + Eq + std::hash::Hash,
{
    if message_ids.is_empty() {
        return;
    }
    super::relayout_trace::note_layout_invalidation("message_ids", message_ids.len());
    GLOBAL_LAYOUT_CACHE.clear_layouts_for_messages(message_ids);
}

/// Rebuild only the layouts that render a link to `url`, e.g. to recolor it
/// once it has been opened.
#[track_caller]
pub fn clear_layouts_linking_to(url: &str) {
    super::relayout_trace::note_layout_invalidation("links_to", 0);
    GLOBAL_LAYOUT_CACHE.clear_layouts_linking_to(url);
}

/// Rebuild every layout of one channel, e.g. after its badge set changed.
#[track_caller]
pub fn clear_layouts_for_channel(channel: &str) {
    super::relayout_trace::note_layout_invalidation("channel", 0);
    GLOBAL_LAYOUT_CACHE.clear_layouts_for_channel(channel);
}

/// Remove cached layouts for selected live messages without broadcasting a
/// process-wide invalidation. The caller must evict its split-local copies and
/// schedule any visible rebuild itself. This keeps per-user decoration updates
/// scoped while ensuring an unpainted global layout cannot be reused later.
pub fn remove_layouts_for_message_ids(message_ids: &std::collections::HashSet<Arc<str>>) -> usize {
    GLOBAL_LAYOUT_CACHE.remove_layouts_for_message_ids(message_ids)
}

/// Reclaim layouts for messages that were evicted from their buffers (push
/// overflow / capacity shrink) or whose channel closed since the last pass.
///
/// Drains the ids queued by `crate::state::layout_eviction` and removes their
/// layouts in a single map scan. Runs off the periodic maintenance slot, NOT
/// the message-ingest hot path. Deliberately does not bump the invalidation
/// version: the underlying messages are gone, so no live painter references
/// these layouts and none needs to rebuild. Returns entries removed.
pub fn drain_evicted_message_layouts() -> usize {
    let ids = crate::state::layout_eviction::drain_evicted_ids();
    remove_layouts_for_message_ids(&ids)
}

/// Whether the global cache holds a layout for `key` without touching its
/// recency/frequency. Telemetry-only helper for the pinned-layout probe.
#[inline]
pub fn contains_layout(key: &LayoutCacheKey) -> bool {
    GLOBAL_LAYOUT_CACHE.contains(key)
}

/// Most recent pinned-layout probe: per-split local keys that were absent from
/// the global cache (`local_only`) out of the `sampled` keys checked. Packed
/// into one atomic (`local_only << 32 | sampled`) for a consistent read.
static PINNED_LAYOUT_PROBE: AtomicU64 = AtomicU64::new(0);

/// Record one pinned-layout probe sample. Telemetry only; called amortized from
/// the paint path's aggregate-flush slot, never per frame.
#[inline]
pub fn record_pinned_layout_probe(local_only: u32, sampled: u32) {
    PINNED_LAYOUT_PROBE.store(
        ((local_only as u64) << 32) | (sampled as u64),
        Ordering::Relaxed,
    );
}

/// Read the most recent pinned-layout probe as `(local_only, sampled)`.
#[inline]
pub fn pinned_layout_probe() -> (u32, u32) {
    let packed = PINNED_LAYOUT_PROBE.load(Ordering::Relaxed);
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn key(message_id: &str) -> LayoutCacheKey {
        LayoutCacheKey::new(message_id, 320.0, false)
    }

    fn layout(message_id: &str) -> Arc<CanvasMessageLayout> {
        Arc::new(CanvasMessageLayout::new(message_id, "cache-test"))
    }

    #[test]
    fn replacement_at_capacity_preserves_entries_and_admission_history() {
        let cache = GlobalLayoutCache::new(3);
        let first = key("first");
        let second = key("second");
        let third = key("third");
        let replacement = layout("first");

        cache.insert(first.clone(), layout("first"));
        cache.insert(second.clone(), layout("second"));
        cache.insert(third.clone(), layout("third"));
        cache.get(&first);

        let (frequency_before, recency_before) = {
            let entry = cache.layouts.get(&first).expect("first layout is cached");
            (
                entry.freq.load(Ordering::Relaxed),
                entry.last_access.load(Ordering::Relaxed),
            )
        };

        cache.insert(first.clone(), Arc::clone(&replacement));

        let entry = cache
            .layouts
            .get(&first)
            .expect("replacement remains cached");
        assert!(Arc::ptr_eq(&entry.layout, &replacement));
        assert_eq!(entry.freq.load(Ordering::Relaxed), frequency_before);
        assert_eq!(entry.last_access.load(Ordering::Relaxed), recency_before);
        assert!(cache.contains(&second));
        assert!(cache.contains(&third));
        assert_eq!(cache.occupancy(), (3, 3));
    }

    #[test]
    fn channel_clear_keeps_other_channels() {
        let cache = GlobalLayoutCache::new(4);
        let other = key("other");
        cache.insert(key("cleared"), layout("cleared"));
        cache.insert(
            other.clone(),
            Arc::new(CanvasMessageLayout::new("other", "other-channel")),
        );
        let seq_before = layout_eviction_seq();

        cache.clear_layouts_for_channel("cache-test");

        assert!(!cache.contains(&key("cleared")));
        assert!(cache.contains(&other));
        assert!(layout_eviction_seq() > seq_before);
    }

    #[test]
    fn eviction_log_fences_only_covered_layouts() {
        let log: VecDeque<(u64, LayoutEviction)> = VecDeque::from([
            (5, LayoutEviction::Message(Arc::from("evicted"))),
            (6, LayoutEviction::LinksTo(Arc::from("https://example.com"))),
            (7, LayoutEviction::Channel(Arc::from("other-channel"))),
        ]);
        let kept = layout("kept");
        let evicted = layout("evicted");

        assert_eq!(valid_through_log(&log, 7, 4, &kept), Some(7));
        assert_eq!(valid_through_log(&log, 7, 4, &evicted), None);
        assert_eq!(valid_through_log(&log, 7, 5, &evicted), Some(7));
        assert_eq!(valid_through_log(&log, 7, 7, &evicted), Some(7));
        assert_eq!(valid_through_log(&log, 7, 3, &kept), None);

        let channel_log: VecDeque<(u64, LayoutEviction)> =
            VecDeque::from([(1, LayoutEviction::Channel(Arc::from("cache-test")))]);
        assert_eq!(valid_through_log(&channel_log, 1, 0, &kept), None);
    }

    #[test]
    fn peek_does_not_change_admission_history() {
        let cache = GlobalLayoutCache::new(1);
        let cache_key = key("message");
        let cached_layout = layout("message");
        cache.insert(cache_key.clone(), Arc::clone(&cached_layout));

        let (frequency_before, recency_before) = {
            let entry = cache
                .layouts
                .get(&cache_key)
                .expect("layout is cached before peeking");
            (
                entry.freq.load(Ordering::Relaxed),
                entry.last_access.load(Ordering::Relaxed),
            )
        };

        let peeked = cache.peek(&cache_key).expect("peek returns cached layout");

        let entry = cache
            .layouts
            .get(&cache_key)
            .expect("layout remains cached after peeking");
        assert!(Arc::ptr_eq(&peeked, &cached_layout));
        assert_eq!(entry.freq.load(Ordering::Relaxed), frequency_before);
        assert_eq!(entry.last_access.load(Ordering::Relaxed), recency_before);
    }
}
