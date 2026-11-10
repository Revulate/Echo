//! In-memory decoded-image cache.
//!
//! - DashMap shard reads on the lookup path
//! - Deferred access tracking via a periodically flushed ring buffer
//! - Shared budget accounting with the other cache layers
//! - Synchronous budget enforcement when inserts require eviction
//!
//! # Performance Characteristics
//!
//! - Read: expected O(1) through a DashMap shard read lock
//! - Write: O(1) with potential eviction trigger
//! - Eviction: candidate selection scans resident entries

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use dashmap::DashMap;
use gpui::RenderImage;

use echo_cache_policy::budget::{BudgetManager, CacheLayer};
use echo_cache_policy::eviction::{AccessBuffer, EvictableEntry, EvictionPolicy};
use echo_cache_policy::stats::CacheStats;

use crate::registry::{
    cancel_render_image_drop, picker_visible_media_hashes, queue_render_image_drop,
    with_picker_visible_media_hash_sets,
};

/// Memory layer for decoded images
///
/// Stores `Arc<RenderImage>` for instant rendering.
/// Uses u64 URL hashes as primary keys and optional provider/id aliases for
/// emotes that appear under different URLs in different splits/channels.
pub struct MemoryLayer {
    /// Image storage: canonical URL hash -> cached entry
    cache: DashMap<u64, CachedImage>,
    /// URL alias hash -> canonical URL hash
    url_aliases: DashMap<u64, u64>,
    /// Provider/id/variant source hash -> canonical URL hash
    source_aliases: DashMap<u64, u64>,
    /// Global access counter for LRU ordering
    access_counter: AtomicU64,
    /// Current memory usage (bytes)
    memory_used: AtomicUsize,
    /// Budget manager reference
    budget: Arc<BudgetManager>,
    /// Eviction policy
    eviction_policy: EvictionPolicy,
    /// Access buffer for deferred LRU updates
    access_buffer: AccessBuffer,
    /// Statistics
    stats: Arc<CacheStats>,
}

static EPOCH: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);

/// Monotonic milliseconds for freshness and idle-sweep comparisons, so a wall
/// clock step (NTP, sleep/resume, manual change) cannot pin or mass-expire
/// entries. Offset by a day so `saturating_sub` cutoffs never clamp to zero
/// early in the process.
#[inline]
fn now_ms() -> u64 {
    const EPOCH_OFFSET_MS: u64 = 24 * 60 * 60 * 1000;
    EPOCH_OFFSET_MS + EPOCH.elapsed().as_millis() as u64
}

/// Window during which a decoded image counts as part of the visible
/// working set. An entry `get()`-touched (painted) within this window is treated
/// as on-screen and is NEVER evicted to satisfy allocation pressure — this is
/// the core no-flicker guarantee: the cache may only discard images that are no
/// longer visible.
///
/// When the visible working set alone exceeds the static budget (e.g. an emote
/// picker showing hundreds of decoded emotes at once), the offending allocation
/// is instead allowed to exceed the ceiling (see `insert_canonical`) rather than
/// thrash currently-painted textures. The elastic ceiling absorbs the common
/// case; this window guarantees correctness even past it. The idle TTL sweep
/// (`evict_idle`) and the next over-budget allocation reclaim the headroom once
/// these emotes stop being displayed (scroll away / picker closed), so a texture
/// stays protected for at most ~2s after it was last on screen.
const VISIBLE_FRESHNESS_MS: u64 = 2_000;

/// Cached image entry
struct CachedImage {
    /// The decoded image
    image: Arc<RenderImage>,
    /// Memory size in bytes
    size_bytes: usize,
    /// Access count for LFU
    access_count: AtomicU64,
    /// Last access (access counter value, not timestamp)
    last_access: AtomicU64,
    /// Monotonic ms of the last recorded access. The counter-based `last_access`
    /// cannot express a time budget, so the idle TTL sweep uses this instead.
    last_touch_ms: AtomicU64,
}

impl CachedImage {
    fn new(image: Arc<RenderImage>, size_bytes: usize, access_counter: u64) -> Self {
        Self {
            image,
            size_bytes,
            access_count: AtomicU64::new(1),
            last_access: AtomicU64::new(access_counter),
            last_touch_ms: AtomicU64::new(now_ms()),
        }
    }
}

impl MemoryLayer {
    const INITIAL_IMAGE_CAPACITY: usize = 512;
    const INITIAL_ALIAS_CAPACITY: usize = 256;

    /// Create a new memory layer
    pub fn new(budget: Arc<BudgetManager>, stats: Arc<CacheStats>) -> Self {
        Self {
            // Warm startup currently admits a small viewport-first batch. Let
            // the maps grow with actual residency instead of reserving space
            // for a fully saturated cache before the first image is decoded.
            cache: DashMap::with_capacity(Self::INITIAL_IMAGE_CAPACITY),
            url_aliases: DashMap::with_capacity(Self::INITIAL_ALIAS_CAPACITY),
            source_aliases: DashMap::with_capacity(Self::INITIAL_ALIAS_CAPACITY),
            access_counter: AtomicU64::new(0),
            memory_used: AtomicUsize::new(0),
            budget,
            eviction_policy: EvictionPolicy::new(),
            access_buffer: AccessBuffer::new(1024),
            stats,
        }
    }

    #[inline]
    fn canonical_hash(&self, url_hash: u64) -> u64 {
        self.url_aliases
            .get(&url_hash)
            .map(|canonical| *canonical)
            .unwrap_or(url_hash)
    }

    fn picker_visible_canonical_hashes(&self) -> std::collections::HashSet<u64> {
        picker_visible_media_hashes()
            .into_iter()
            .map(|hash| self.canonical_hash(hash))
            .collect()
    }

    fn remove_aliases_for_canonical(&self, canonical_hash: u64) {
        self.url_aliases
            .retain(|_, target| *target != canonical_hash);
        self.source_aliases
            .retain(|_, target| *target != canonical_hash);
    }

    /// Attach an alias URL to an already-decoded provider/id/variant image.
    ///
    /// Returns true when the alias is now renderable from memory.
    pub fn alias_url_to_source_key(&self, url_hash: u64, source_key: u64) -> bool {
        let Some(canonical_hash) = self.source_aliases.get(&source_key).map(|entry| *entry) else {
            return false;
        };

        if self.cache.contains_key(&canonical_hash) {
            if canonical_hash != url_hash {
                self.url_aliases.insert(url_hash, canonical_hash);
            }
            return true;
        }

        self.source_aliases.remove(&source_key);
        false
    }

    /// Check whether a provider/id/variant image is resident under any URL.
    #[inline]
    pub fn contains_source_key(&self, source_key: u64) -> bool {
        self.source_aliases
            .get(&source_key)
            .map(|canonical_hash| self.cache.contains_key(&*canonical_hash))
            .unwrap_or(false)
    }

    /// Remove the decoded image owned by a provider/id/variant source key.
    pub fn remove_source_key(&self, source_key: u64) -> Option<usize> {
        let canonical_hash = self.source_aliases.get(&source_key).map(|entry| *entry)?;
        self.remove(canonical_hash)
    }

    /// Get an image by URL hash (lock-free read path)
    ///
    /// This is the hot path called during rendering.
    /// Uses read-only access to avoid contention.
    #[inline]
    pub fn get(&self, url_hash: u64) -> Option<Arc<RenderImage>> {
        let canonical_hash = self.canonical_hash(url_hash);
        let entry = self.cache.get(&canonical_hash)?;

        // Freshness is a correctness boundary: a fixed-size deferred buffer can
        // drop accesses from a maximized picker viewport, making genuinely
        // visible textures appear stale and evictable. Stamp it directly with a
        // lock-free atomic; keep the buffer only for approximate LRU/LFU counts.
        entry.last_touch_ms.store(now_ms(), Ordering::Relaxed);
        self.access_buffer.push(canonical_hash);
        self.stats.record_hit();

        Some(Arc::clone(&entry.image))
    }

    /// Get an image by URL string
    #[inline]
    pub fn get_by_url(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.get(Self::hash_url(url))
    }

    /// Check if an image is cached
    #[inline]
    pub fn contains(&self, url_hash: u64) -> bool {
        let canonical_hash = self.canonical_hash(url_hash);
        self.cache.contains_key(&canonical_hash)
    }

    /// Check if an image is cached by URL
    #[inline]
    pub fn contains_url(&self, url: &str) -> bool {
        self.contains(Self::hash_url(url))
    }

    /// Insert an image into the cache
    ///
    /// Returns true if the image was inserted, false if eviction failed.
    pub fn insert(&self, url_hash: u64, image: Arc<RenderImage>) -> bool {
        self.insert_with_source_key(url_hash, None, image)
    }

    /// Insert an image and register a provider/id/variant alias for cross-channel reuse.
    ///
    /// Returns true if the requested URL is renderable, either because it was
    /// inserted or because it was aliased to an existing decoded image.
    pub fn insert_with_source_key(
        &self,
        url_hash: u64,
        source_key: Option<u64>,
        image: Arc<RenderImage>,
    ) -> bool {
        if let Some(source_key) = source_key
            && self.alias_url_to_source_key(url_hash, source_key)
        {
            return true;
        }

        self.url_aliases.remove(&url_hash);
        self.remove_aliases_for_canonical(url_hash);

        if let Some(source_key) = source_key {
            self.source_aliases.insert(source_key, url_hash);
        }

        self.insert_canonical(url_hash, image)
    }

    fn insert_canonical(&self, url_hash: u64, image: Arc<RenderImage>) -> bool {
        let image_id = image.id;
        cancel_render_image_drop(image_id);
        let size = image.size(0);
        let size_bytes = (size.width.0 as usize) * (size.height.0 as usize) * 4;

        // Check budget
        if !self.budget.try_allocate(CacheLayer::Static, size_bytes) {
            // Over budget: reclaim space from entries that are no longer visible.
            // `evict_for_size` protects anything painted within
            // `VISIBLE_FRESHNESS_MS`, so this only frees genuinely idle textures.
            self.evict_for_size(size_bytes);
            if !self.budget.try_allocate(CacheLayer::Static, size_bytes) {
                // Eviction could not free enough WITHOUT evicting a currently
                // visible (freshly painted) entry: the on-screen working set
                // alone exceeds the static budget. Evicting a visible texture
                // here would reload it next frame (flicker/thrash), so instead
                // allow this allocation to exceed the ceiling. Temporarily
                // exceeding the cap is preferred to thrashing visible content;
                // the idle TTL sweep and the next over-budget allocation reclaim
                // the headroom once these emotes stop being displayed.
                self.budget.allocate(CacheLayer::Static, size_bytes);
                self.stats.record_freshness_blocked_eviction();
            }
        }

        let access_counter = self.access_counter.fetch_add(1, Ordering::Relaxed);
        let entry = CachedImage::new(image, size_bytes, access_counter);

        // Handle replacement
        let inserted_new = match self.cache.insert(url_hash, entry) {
            Some(old) => {
                if old.image.id != image_id {
                    queue_render_image_drop(old.image);
                }
                self.budget.release(CacheLayer::Static, old.size_bytes);
                self.memory_used
                    .fetch_sub(old.size_bytes, Ordering::Relaxed);
                self.stats
                    .memory_used
                    .fetch_sub(old.size_bytes, Ordering::Relaxed);
                false
            }
            _ => true,
        };

        self.memory_used.fetch_add(size_bytes, Ordering::Relaxed);
        if inserted_new {
            self.stats.static_images.fetch_add(1, Ordering::Relaxed);
        }
        self.stats
            .memory_used
            .fetch_add(size_bytes, Ordering::Relaxed);

        true
    }

    /// Insert an image by URL string
    pub fn insert_by_url(&self, url: &str, image: Arc<RenderImage>) -> bool {
        self.insert(Self::hash_url(url), image)
    }

    /// Insert an image by URL string and provider/id/variant source key.
    pub fn insert_by_url_with_source_key(
        &self,
        url: &str,
        source_key: Option<u64>,
        image: Arc<RenderImage>,
    ) -> bool {
        self.insert_with_source_key(Self::hash_url(url), source_key, image)
    }

    /// Remove one decoded image from memory.
    ///
    /// This is used by UI-scoped caches such as the emote picker. Disk cache
    /// entries remain intact, so reopening can hydrate from disk instead of
    /// redownloading.
    pub fn remove(&self, url_hash: u64) -> Option<usize> {
        self.flush_access_buffer();
        if let Some(size) = self.remove_evicted_entry(url_hash) {
            return Some(size);
        }
        self.url_aliases.remove(&url_hash).map(|_| 0)
    }

    /// Remove one decoded image by URL string.
    pub fn remove_by_url(&self, url: &str) -> Option<usize> {
        self.remove(Self::hash_url(url))
    }

    fn remove_evicted_entry(&self, url_hash: u64) -> Option<usize> {
        let (_, entry) = self.cache.remove(&url_hash)?;
        self.remove_aliases_for_canonical(url_hash);
        self.budget.release(CacheLayer::Static, entry.size_bytes);
        self.memory_used
            .fetch_sub(entry.size_bytes, Ordering::Relaxed);
        self.stats
            .memory_used
            .fetch_sub(entry.size_bytes, Ordering::Relaxed);
        self.stats.static_images.fetch_sub(1, Ordering::Relaxed);
        self.stats.record_eviction(entry.size_bytes);
        queue_render_image_drop(entry.image);
        Some(entry.size_bytes)
    }

    /// Remove an eviction candidate only while it remains outside both picker
    /// media viewports. The registry read guard stays held through removal, so
    /// a concurrent viewport publication cannot turn a stale candidate into a
    /// newly visible poster between revalidation and eviction.
    fn remove_evicted_entry_if_picker_invisible(
        &self,
        url_hash: u64,
        minimum_touch_ms: u64,
    ) -> Option<usize> {
        with_picker_visible_media_hash_sets(|inline, popout| {
            let is_visible = inline
                .iter()
                .chain(popout.iter())
                .any(|hash| self.canonical_hash(*hash) == url_hash);
            // Recheck chat visibility after scoring as well. `get()` refreshes
            // this timestamp synchronously, so an entry painted between the
            // candidate scan and this guarded removal must survive.
            let was_touched_since_scan = self.cache.get(&url_hash).is_some_and(|entry| {
                entry.last_touch_ms.load(Ordering::Relaxed) >= minimum_touch_ms
            });
            if is_visible || was_touched_since_scan {
                None
            } else {
                self.remove_evicted_entry(url_hash)
            }
        })
    }

    fn evict_single_entry_for_size(
        &self,
        needed_bytes: usize,
        current_counter: u64,
    ) -> Option<usize> {
        let fresh_cutoff = now_ms().saturating_sub(VISIBLE_FRESHNESS_MS);
        let picker_visible = self.picker_visible_canonical_hashes();
        let mut best_key = None;
        let mut best_score = f64::MAX;

        for entry in self.cache.iter() {
            let value = entry.value();
            if value.size_bytes < needed_bytes {
                continue;
            }
            if picker_visible.contains(entry.key()) {
                continue;
            }
            // Visible-set protection: skip entries painted within the freshness
            // window. Evicting a currently-visible texture to make room for
            // another only reloads it next frame (flicker).
            if value.last_touch_ms.load(Ordering::Relaxed) >= fresh_cutoff {
                continue;
            }

            let candidate = EvictableEntry {
                key: *entry.key(),
                size_bytes: value.size_bytes,
                access_count: value.access_count.load(Ordering::Relaxed),
                age: std::time::Duration::from_secs(
                    current_counter.saturating_sub(value.last_access.load(Ordering::Relaxed)),
                ),
                protected: false,
            };
            let score = self.eviction_policy.score(&candidate);
            if score < best_score {
                best_score = score;
                best_key = Some(candidate.key);
            }
        }

        best_key.and_then(|key| self.remove_evicted_entry_if_picker_invisible(key, fresh_cutoff))
    }

    pub fn flush_access_buffer(&self) {
        let accesses = self.access_buffer.drain();
        if accesses.is_empty() {
            return;
        }

        let touch_now = now_ms();
        let base_counter = self
            .access_counter
            .fetch_add(accesses.len() as u64, Ordering::Relaxed);
        let mut applied = 0usize;
        for (offset, key) in accesses.into_iter().enumerate() {
            if let Some(entry) = self.cache.get(&key) {
                entry.access_count.fetch_add(1, Ordering::Relaxed);
                entry
                    .last_access
                    .store(base_counter + offset as u64 + 1, Ordering::Relaxed);
                // Refresh the monotonic touch so currently-displayed emotes are
                // never reaped by the idle TTL sweep (they are `get()`-ed every
                // frame, so their hash is always in the just-drained buffer).
                entry.last_touch_ms.store(touch_now, Ordering::Relaxed);
                applied += 1;
            }
        }

        if applied > 0 {
            crate::log_cache!(trace, applied, "Flushed memory-cache access buffer");
        }
    }

    /// Release decoded static images not displayed for `ttl` (idle retention
    /// for the static layer; frames and sources have their own sweeps).
    ///
    /// The budget-pressure LRU only runs when allocating, so without this sweep a
    /// busy channel's emote textures stay resident at the high-water mark after
    /// the emotes scroll away. Flushes pending accesses first so visible emotes
    /// keep a fresh touch and are not evicted.
    pub fn evict_idle(&self, ttl: std::time::Duration) -> usize {
        self.flush_access_buffer();
        let cutoff = now_ms().saturating_sub(ttl.as_millis() as u64);
        let picker_visible = self.picker_visible_canonical_hashes();
        let expired: Vec<u64> = self
            .cache
            .iter()
            .filter(|entry| {
                !picker_visible.contains(entry.key())
                    && entry.value().last_touch_ms.load(Ordering::Relaxed) < cutoff
            })
            .map(|entry| *entry.key())
            .collect();

        let mut freed = 0usize;
        for key in expired {
            if let Some(size) = self.remove_evicted_entry_if_picker_invisible(key, cutoff) {
                freed += size;
            }
        }

        if freed > 0 {
            crate::log_cache!(debug, freed_bytes = freed, "Idle static-image TTL eviction");
        }
        freed
    }

    /// Evict entries to make room for new data
    fn evict_for_size(&self, needed_bytes: usize) {
        self.flush_access_buffer();
        let current_counter = self.access_counter.load(Ordering::Relaxed);
        if let Some(evicted_bytes) = self.evict_single_entry_for_size(needed_bytes, current_counter)
        {
            crate::log_cache!(
                debug,
                evicted_bytes,
                needed_bytes,
                "Memory layer single-entry eviction complete"
            );
            return;
        }

        let fresh_cutoff = now_ms().saturating_sub(VISIBLE_FRESHNESS_MS);
        let picker_visible = self.picker_visible_canonical_hashes();
        // Collect entries for eviction scoring. Currently-visible (freshly
        // painted) entries are excluded up front so they can never be scored for
        // eviction — the on-screen working set is protected even when the LRU/LFU
        // score would otherwise rank a visible emote below an idle one.
        let entries: Vec<EvictableEntry> = self
            .cache
            .iter()
            .filter(|r| {
                !picker_visible.contains(r.key())
                    && r.value().last_touch_ms.load(Ordering::Relaxed) < fresh_cutoff
            })
            .map(|r| {
                let key = *r.key();
                let value = r.value();
                EvictableEntry {
                    key,
                    size_bytes: value.size_bytes,
                    access_count: value.access_count.load(Ordering::Relaxed),
                    age: std::time::Duration::from_secs(
                        current_counter.saturating_sub(value.last_access.load(Ordering::Relaxed)),
                    ),
                    protected: false,
                }
            })
            .collect();

        // Use probabilistic sampling for large caches
        let candidates = if entries.len() > 1000 {
            self.eviction_policy
                .sample_candidates(entries, 10.0, needed_bytes)
        } else {
            self.eviction_policy
                .get_candidates(entries.into_iter(), needed_bytes)
        };

        // Evict candidates
        let mut evicted_bytes = 0usize;
        for candidate in candidates {
            if evicted_bytes >= needed_bytes {
                break;
            }

            if let Some(size_bytes) =
                self.remove_evicted_entry_if_picker_invisible(candidate.key, fresh_cutoff)
            {
                evicted_bytes += size_bytes;
            }
        }

        crate::log_cache!(
            debug,
            evicted_bytes,
            needed_bytes,
            "Memory layer eviction complete"
        );
    }

    /// Hash a URL for fast lookup
    #[inline]
    pub fn hash_url(url: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        url.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, RgbaImage};

    fn create_test_image(width: u32, height: u32) -> Arc<RenderImage> {
        let rgba = RgbaImage::new(width, height);
        let delay = Delay::from_numer_denom_ms(0, 1);
        let frame = Frame::from_parts(rgba, 0, 0, delay);
        Arc::new(RenderImage::new(vec![frame]))
    }

    #[test]
    fn test_memory_layer_basic() {
        let budget = Arc::new(BudgetManager::with_budget(1_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = MemoryLayer::new(budget, stats);

        let url = "https://example.com/image.png";
        let hash = MemoryLayer::hash_url(url);
        let image = create_test_image(100, 100);

        assert!(!layer.contains(hash));
        assert!(layer.insert(hash, image.clone()));
        assert!(layer.contains(hash));

        let retrieved = layer.get(hash);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_memory_layer_over_budget_protects_visible() {
        // Small budget: 10KB. A 100x100 image = 40KB exceeds it.
        let budget = Arc::new(BudgetManager::with_budget(10_000));
        let stats = Arc::new(CacheStats::new());
        let layer = MemoryLayer::new(Arc::clone(&budget), Arc::clone(&stats));

        let image = create_test_image(100, 100);

        // Visible-set protection: refusing to cache a currently-needed image
        // would make it reload every frame (flicker). Instead the allocation is
        // allowed to exceed the budget; the image must be resident afterward and
        // the over-budget counter must have fired.
        assert!(layer.insert(0, image));
        assert!(layer.contains(0));
        assert_eq!(stats.freshness_blocked_evictions.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_stale_entry_evicted_over_fresh() {
        // Budget fits ~one 100x100 (40KB) image with headroom, but not two.
        let budget = Arc::new(BudgetManager::with_budget(120_000));
        let stats = Arc::new(CacheStats::new());
        let layer = MemoryLayer::new(Arc::clone(&budget), Arc::clone(&stats));

        // Insert an entry and backdate its touch so it is no longer "visible".
        assert!(layer.insert(1, create_test_image(100, 100)));
        if let Some(entry) = layer.cache.get(&1) {
            let stale = now_ms().saturating_sub(VISIBLE_FRESHNESS_MS * 4);
            entry.last_touch_ms.store(stale, Ordering::Relaxed);
        }

        // Fill the budget with fresh (just-inserted) entries.
        assert!(layer.insert(2, create_test_image(100, 100)));

        // A further insert under pressure must reclaim the STALE entry (1),
        // never the fresh ones, and must not force over-budget (counter stays 0).
        assert!(layer.insert(3, create_test_image(100, 100)));
        assert!(!layer.contains(1), "stale entry should be evicted");
        assert!(layer.contains(2), "fresh entry must be protected");
        assert!(layer.contains(3), "new visible entry must be resident");
        assert_eq!(
            stats.freshness_blocked_evictions.load(Ordering::Relaxed),
            0,
            "a stale entry was available, so no over-budget force was needed"
        );
    }

    #[test]
    fn test_hash_consistency() {
        let url = "https://cdn.7tv.app/emote/123/1x.webp";
        let hash1 = MemoryLayer::hash_url(url);
        let hash2 = MemoryLayer::hash_url(url);
        assert_eq!(hash1, hash2);

        let different = MemoryLayer::hash_url("https://cdn.7tv.app/emote/456/1x.webp");
        assert_ne!(hash1, different);
    }

    #[test]
    fn test_access_buffer() {
        let budget = Arc::new(BudgetManager::with_budget(1_000_000));
        let stats = Arc::new(CacheStats::new());
        let layer = MemoryLayer::new(budget, stats);

        let image = create_test_image(10, 10);
        layer.insert(42, image);

        // Multiple reads should buffer accesses
        for _ in 0..5 {
            layer.get(42);
        }

        // Flush should update access count
        layer.flush_access_buffer();

        // Verify entry was touched (access count increased)
        if let Some(entry) = layer.cache.get(&42) {
            assert!(entry.access_count.load(Ordering::Relaxed) >= 1);
        }
    }
}
