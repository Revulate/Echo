//! Unified cache statistics
//!
//! Counters from every cache layer in one UI-independent struct, read by the
//! metrics overlay, diagnostics and logs.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Unified cache statistics
#[derive(Debug, Default)]
pub struct CacheStats {
    // Memory layer stats
    /// Number of static images in memory
    pub static_images: AtomicUsize,
    /// Number of animated images in memory
    pub animated_images: AtomicUsize,
    /// Total memory used by images (bytes)
    pub memory_used: AtomicUsize,

    // Frame layer stats
    /// Number of hot frames in memory
    pub hot_frames: AtomicUsize,
    /// Number of animation metadata entries
    pub animation_metadata: AtomicUsize,
    /// Memory used by frames (bytes)
    pub frame_memory: AtomicUsize,

    // Disk layer stats
    /// Number of entries in disk cache
    pub disk_entries: AtomicUsize,
    /// Disk cache size (alternate name for compat)
    pub disk_bytes: AtomicUsize,
    /// Number of disk evictions
    pub disk_evictions: AtomicUsize,

    // Loader stats
    /// Total images downloaded this session
    pub total_downloaded: AtomicU64,
    /// Total bytes downloaded this session
    pub total_bytes_downloaded: AtomicU64,
    /// Cache hits (memory + disk)
    pub cache_hits: AtomicU64,
    /// Cache misses (required network fetch)
    pub cache_misses: AtomicU64,
    /// Currently active downloads
    pub active_downloads: AtomicUsize,

    // Performance stats
    /// Disk hydrations (decompressed from disk)
    pub disk_hydrations: AtomicU64,
    /// Cumulative disk hydration time (microseconds)
    pub disk_hydration_time_us: AtomicU64,
    /// Number of evictions performed
    pub evictions: AtomicU64,
    /// Bytes evicted
    pub bytes_evicted: AtomicU64,

    // Texture deduplication stats
    /// Number of times an existing texture was reused (dedup hit)
    pub texture_reuses: AtomicU64,
    /// Bytes saved by texture deduplication
    pub texture_bytes_saved: AtomicU64,

    // Visible-set protection stats
    /// Number of decoded-media allocations allowed to exceed the static budget
    /// because every evictable entry was currently visible (freshly painted
    /// within the visible-set freshness window). A rising count means the
    /// on-screen working set (e.g. a full emote picker) exceeds the static
    /// budget and the cache is deliberately holding visible content over the RAM
    /// cap instead of evicting-and-reloading it (which would flicker).
    pub freshness_blocked_evictions: AtomicU64,
}

impl CacheStats {
    /// Create new empty stats
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cache hit
    #[inline]
    pub fn record_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss
    #[inline]
    pub fn record_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a download completion
    #[inline]
    pub fn record_download(&self, bytes: u64) {
        self.total_downloaded.fetch_add(1, Ordering::Relaxed);
        self.total_bytes_downloaded
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record disk hydration
    #[inline]
    pub fn record_hydration(&self, duration_us: u64) {
        self.disk_hydrations.fetch_add(1, Ordering::Relaxed);
        self.disk_hydration_time_us
            .fetch_add(duration_us, Ordering::Relaxed);
    }

    /// Record eviction
    #[inline]
    pub fn record_eviction(&self, bytes: usize) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        self.bytes_evicted
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Record texture reuse from deduplication
    #[inline]
    pub fn record_texture_reuse(&self, bytes_saved: usize) {
        self.texture_reuses.fetch_add(1, Ordering::Relaxed);
        self.texture_bytes_saved
            .fetch_add(bytes_saved as u64, Ordering::Relaxed);
    }

    /// Record an allocation that was allowed to exceed the static budget because
    /// eviction was blocked by the visible-set freshness guard (every evictable
    /// entry was currently on screen). Observability hook for the no-flicker
    /// visible-set protection in `MemoryLayer::insert_canonical`.
    #[inline]
    pub fn record_freshness_blocked_eviction(&self) {
        self.freshness_blocked_evictions
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Get cache hit rate as percentage (0-100)
    pub fn hit_rate(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed) as f64;
        let misses = self.cache_misses.load(Ordering::Relaxed) as f64;
        let total = hits + misses;
        if total > 0.0 {
            (hits / total) * 100.0
        } else {
            0.0
        }
    }

    /// Get snapshot for logging/display
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            static_images: self.static_images.load(Ordering::Relaxed),
            animated_images: self.animated_images.load(Ordering::Relaxed),
            memory_used: self.memory_used.load(Ordering::Relaxed),
            frame_memory: self.frame_memory.load(Ordering::Relaxed),
            active_downloads: self.active_downloads.load(Ordering::Relaxed),
            hit_rate: self.hit_rate(),
            freshness_blocked_evictions: self.freshness_blocked_evictions.load(Ordering::Relaxed),
        }
    }

    /// Complete point-in-time cache/resource counters for diagnostics export.
    /// Unlike `snapshot`, this intentionally includes cumulative I/O and
    /// eviction counters which are useful for long-session residue analysis.
    pub fn diagnostic_snapshot(&self) -> DiagnosticStatsSnapshot {
        DiagnosticStatsSnapshot {
            static_images: self.static_images.load(Ordering::Relaxed),
            animated_images: self.animated_images.load(Ordering::Relaxed),
            memory_used: self.memory_used.load(Ordering::Relaxed),
            hot_frames: self.hot_frames.load(Ordering::Relaxed),
            animation_metadata: self.animation_metadata.load(Ordering::Relaxed),
            frame_memory: self.frame_memory.load(Ordering::Relaxed),
            disk_entries: self.disk_entries.load(Ordering::Relaxed),
            disk_bytes: self.disk_bytes.load(Ordering::Relaxed),
            disk_evictions: self.disk_evictions.load(Ordering::Relaxed),
            total_downloaded: self.total_downloaded.load(Ordering::Relaxed),
            total_bytes_downloaded: self.total_bytes_downloaded.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            active_downloads: self.active_downloads.load(Ordering::Relaxed),
            disk_hydrations: self.disk_hydrations.load(Ordering::Relaxed),
            disk_hydration_time_us: self.disk_hydration_time_us.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            bytes_evicted: self.bytes_evicted.load(Ordering::Relaxed),
            texture_reuses: self.texture_reuses.load(Ordering::Relaxed),
            texture_bytes_saved: self.texture_bytes_saved.load(Ordering::Relaxed),
            freshness_blocked_evictions: self.freshness_blocked_evictions.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiagnosticStatsSnapshot {
    pub static_images: usize,
    pub animated_images: usize,
    pub memory_used: usize,
    pub hot_frames: usize,
    pub animation_metadata: usize,
    pub frame_memory: usize,
    pub disk_entries: usize,
    pub disk_bytes: usize,
    pub disk_evictions: usize,
    pub total_downloaded: u64,
    pub total_bytes_downloaded: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub active_downloads: usize,
    pub disk_hydrations: u64,
    pub disk_hydration_time_us: u64,
    pub evictions: u64,
    pub bytes_evicted: u64,
    pub texture_reuses: u64,
    pub texture_bytes_saved: u64,
    pub freshness_blocked_evictions: u64,
}

/// Point-in-time snapshot of cache statistics
#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub static_images: usize,
    pub animated_images: usize,
    pub memory_used: usize,
    pub frame_memory: usize,
    pub active_downloads: usize,
    pub hit_rate: f64,
    /// Cumulative count of allocations kept over-budget to protect visible media.
    pub freshness_blocked_evictions: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_default() {
        let stats = CacheStats::new();
        assert_eq!(stats.cache_hits.load(Ordering::Relaxed), 0);
        assert_eq!(stats.hit_rate(), 0.0);
    }

    #[test]
    fn test_hit_rate_calculation() {
        let stats = CacheStats::new();
        stats.cache_hits.store(75, Ordering::Relaxed);
        stats.cache_misses.store(25, Ordering::Relaxed);
        assert!((stats.hit_rate() - 75.0).abs() < 0.01);
    }
}
