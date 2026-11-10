//! Memory budget allocation and enforcement
//!
//! Provides unified memory budget management across all cache layers.
//! Uses atomic operations for lock-free budget checking on hot paths without UI dependencies.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Wall-clock milliseconds since the epoch (coarse; gates the elastic expansion).
#[inline]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Cache layer identifier for budget allocation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheLayer {
    /// Static images (non-animated emotes, badges)
    Static,
    /// Animation frames (hot frame cache)
    Frames,
    /// Metadata (animation metadata, frequency tracking, index)
    Metadata,
}

/// Memory budget manager
///
/// Allocates a total memory budget across cache layers:
/// - Static: 50% for decoded RenderImages
/// - Frames: 40% for hot animation frames
/// - Metadata: 10% for animation metadata and tracking
#[derive(Debug)]
pub struct BudgetManager {
    /// Static images budget (bytes)
    static_budget: usize,
    /// Animation frames budget (bytes)
    frame_budget: usize,
    /// Metadata budget (bytes)
    metadata_budget: usize,
    /// Expanded static budget while an emote picker is open.
    picker_static_budget: usize,
    /// Expanded animation-frame budget while an emote picker is open.
    picker_frame_budget: usize,
    /// Expanded metadata budget while an emote picker is open.
    picker_metadata_budget: usize,
    /// Number of open picker scopes.
    picker_scope_count: AtomicUsize,
    /// Epoch-ms until which the static layer may use its expanded (1.5x) ceiling.
    /// Bumped whenever a static allocation would exceed the base budget.
    pressure_until: AtomicU64,
    /// Current allocations
    allocations: BudgetAllocations,
}

/// Atomic allocation tracking
#[derive(Debug, Default)]
struct BudgetAllocations {
    static_used: AtomicUsize,
    frames_used: AtomicUsize,
    metadata_used: AtomicUsize,
}

impl BudgetManager {
    /// Budget allocation percentages
    const STATIC_PERCENT: f64 = 0.50;
    const FRAMES_PERCENT: f64 = 0.40;
    const METADATA_PERCENT: f64 = 0.10;

    /// Multiplier the static-image layer may grow to under display pressure, so
    /// all currently-visible emotes fit without evicting each other. The idle TTL
    /// sweep (`MemoryLayer::evict_idle`) returns this headroom when emotes stop
    /// displaying, so the larger ceiling is only used while genuinely needed.
    ///
    /// The expansion window RE-ARMS on every over-base static allocation (see
    /// `try_allocate`), so on emote-heavy channels a steady stream of
    /// allocations holds the expanded ceiling open effectively permanently —
    /// the static layer sits near its expanded ceiling continuously, not just
    /// in transient bursts. 1.5x keeps enough headroom for a busy screen while
    /// capping worst-case decoded-media RSS.
    const STATIC_EXPANSION_FACTOR: f64 = 1.5;
    /// How long (ms) the expanded static ceiling stays open after the last
    /// over-base allocation before reverting to the base budget. Because the
    /// window re-arms on every over-base allocation, continuous emote traffic
    /// holds it open indefinitely; a shorter window collapses the ceiling back to
    /// base sooner once allocations quiet down.
    const PRESSURE_WINDOW_MS: u64 = 15_000;

    /// Create with default budget.
    ///
    /// This budget is for decoded media only. The process also carries GPUI,
    /// chat state, layout rows, network/runtime state, and allocator overhead,
    /// so decoded media must stay well below the desired process-resident
    /// target.
    pub fn new() -> Self {
        let total = default_total_budget_bytes();
        let picker_total = default_picker_total_budget_bytes(total);
        Self::with_budgets(total, picker_total)
    }

    /// Create with custom total budget
    ///
    /// Test fixture only. The memory-layer tests that call this live in
    /// another crate, which a `cfg(test)` item cannot reach; `test-support`
    /// exposes it to those dev-dependencies without shipping it in a normal
    /// build.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_budget(total: usize) -> Self {
        Self::with_budgets(total, total)
    }

    fn with_budgets(total: usize, picker_total: usize) -> Self {
        Self {
            static_budget: (total as f64 * Self::STATIC_PERCENT) as usize,
            frame_budget: (total as f64 * Self::FRAMES_PERCENT) as usize,
            metadata_budget: (total as f64 * Self::METADATA_PERCENT) as usize,
            picker_static_budget: (picker_total as f64 * Self::STATIC_PERCENT) as usize,
            picker_frame_budget: (picker_total as f64 * Self::FRAMES_PERCENT) as usize,
            picker_metadata_budget: (picker_total as f64 * Self::METADATA_PERCENT) as usize,
            picker_scope_count: AtomicUsize::new(0),
            pressure_until: AtomicU64::new(0),
            allocations: BudgetAllocations::default(),
        }
    }

    /// Get budget for a layer
    pub fn budget_for(&self, layer: CacheLayer) -> usize {
        let base = match layer {
            CacheLayer::Static => self.static_budget,
            CacheLayer::Frames => self.frame_budget,
            CacheLayer::Metadata => self.metadata_budget,
        };

        if self.picker_scope_count.load(Ordering::Relaxed) == 0 {
            return base;
        }

        let picker_budget = match layer {
            CacheLayer::Static => self.picker_static_budget,
            CacheLayer::Frames => self.picker_frame_budget,
            CacheLayer::Metadata => self.picker_metadata_budget,
        };
        base.max(picker_budget)
    }

    /// Try to allocate memory from a layer's budget
    ///
    /// Returns true if allocation succeeded, false if it would exceed budget.
    /// Does NOT block - caller should trigger eviction if needed.
    #[inline]
    pub fn try_allocate(&self, layer: CacheLayer, bytes: usize) -> bool {
        let counter = self.counter_for(layer);
        let base = self.budget_for(layer);
        let current = counter.load(Ordering::Relaxed);

        // Elastic headroom: when decoded static emote textures exceed their base
        // allowance (many distinct emotes on screen at once), open a time-bounded
        // expansion window so currently-visible emotes are not evicted to fit each
        // other. Marked BEFORE computing the ceiling below, so an over-base
        // allocation immediately sees the expanded ceiling (no transient thrash).
        if matches!(layer, CacheLayer::Static) && current + bytes > base {
            self.pressure_until
                .store(now_ms() + Self::PRESSURE_WINDOW_MS, Ordering::Relaxed);
        }

        let budget = self.effective_budget(layer, base);

        // Optimistic check first (most common path)
        if current + bytes > budget {
            return false;
        }

        // Atomic add with overflow check
        let prev = counter.fetch_add(bytes, Ordering::Relaxed);
        if prev + bytes > budget {
            // Exceeded - roll back
            counter.fetch_sub(bytes, Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    /// Effective allocation ceiling. The static layer may grow to
    /// `STATIC_EXPANSION_FACTOR x` its base budget while an expansion window is
    /// open; all other layers always use their base budget.
    #[inline]
    fn effective_budget(&self, layer: CacheLayer, base: usize) -> usize {
        if matches!(layer, CacheLayer::Static)
            && now_ms() < self.pressure_until.load(Ordering::Relaxed)
        {
            base.max((self.static_budget as f64 * Self::STATIC_EXPANSION_FACTOR) as usize)
        } else {
            base
        }
    }

    /// Force allocate (for cases where we've already evicted)
    #[inline]
    pub fn allocate(&self, layer: CacheLayer, bytes: usize) {
        self.counter_for(layer).fetch_add(bytes, Ordering::Relaxed);
    }

    /// Bytes currently charged to a layer.
    ///
    /// Diagnostics only: this is what the layer believes it is holding, which is
    /// what a memory breakdown needs in order to separate decoded media from the
    /// rest of the process.
    #[inline]
    pub fn used_bytes(&self, layer: CacheLayer) -> usize {
        self.counter_for(layer).load(Ordering::Relaxed)
    }

    /// Get current usage for a layer.
    pub fn usage_for(&self, layer: CacheLayer) -> usize {
        self.used_bytes(layer)
    }

    /// Get total current usage across all layers.
    pub fn total_usage(&self) -> usize {
        self.used_bytes(CacheLayer::Static)
            + self.used_bytes(CacheLayer::Frames)
            + self.used_bytes(CacheLayer::Metadata)
    }

    /// Get usage as a percentage of the current total layer budget.
    pub fn usage_percent(&self) -> f64 {
        let total_budget = self.budget_for(CacheLayer::Static)
            + self.budget_for(CacheLayer::Frames)
            + self.budget_for(CacheLayer::Metadata);
        if total_budget == 0 {
            0.0
        } else {
            (self.total_usage() as f64 / total_budget as f64) * 100.0
        }
    }

    /// Release allocated memory
    #[inline]
    pub fn release(&self, layer: CacheLayer, bytes: usize) {
        // Saturating: an over-release (concurrent double-free) must clamp at
        // zero, never wrap to ~usize::MAX, which would permanently stall the
        // layer's budget by making it appear fully allocated forever.
        let _ =
            self.counter_for(layer)
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_sub(bytes))
                });
    }

    /// Enter an expanded cache budget while picker-owned media is visible.
    pub fn begin_picker_scope(&self) -> usize {
        self.picker_scope_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether at least one picker currently owns the expanded media budget.
    #[inline]
    pub fn picker_scope_active(&self) -> bool {
        self.picker_scope_count.load(Ordering::Relaxed) > 0
    }

    /// Number of picker owners currently holding the expanded cache budget.
    /// Used only by the opt-in process-memory diagnostic.
    #[inline]
    pub fn picker_scope_count(&self) -> usize {
        self.picker_scope_count.load(Ordering::Relaxed)
    }

    /// Leave one expanded picker cache scope. Returns remaining scopes.
    pub fn end_picker_scope(&self) -> usize {
        let mut current = self.picker_scope_count.load(Ordering::SeqCst);
        loop {
            if current == 0 {
                return 0;
            }
            match self.picker_scope_count.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return current - 1,
                Err(actual) => current = actual,
            }
        }
    }

    /// Reset all allocations (for testing)
    #[cfg(test)]
    pub fn reset(&self) {
        self.allocations.static_used.store(0, Ordering::Relaxed);
        self.allocations.frames_used.store(0, Ordering::Relaxed);
        self.allocations.metadata_used.store(0, Ordering::Relaxed);
    }

    /// Get atomic counter for a layer
    fn counter_for(&self, layer: CacheLayer) -> &AtomicUsize {
        match layer {
            CacheLayer::Static => &self.allocations.static_used,
            CacheLayer::Frames => &self.allocations.frames_used,
            CacheLayer::Metadata => &self.allocations.metadata_used,
        }
    }
}

impl Default for BudgetManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Pre-warmed total system memory in bytes.
///
/// Populated at startup (before the first frame) by the prewarm thread in
/// `main.rs`. Any subsequent caller that reaches here before the prewarm
/// thread finishes will perform the sysinfo probe itself — `OnceLock`
/// guarantees exactly-once semantics, so the cost is paid at most once.
pub fn prewarmed_total_memory_bytes() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let Ok(mut sys) = sysinfo::System::new() else {
            return 0;
        };
        sys.refresh_memory();
        sys.total_memory()
    })
}

/// The resolved decoded-media budget in bytes, for the memory breakdown
/// diagnostic.
///
/// Priority:
/// 1) `ECHO_MEDIA_CACHE_MB` env var (preferred name) — set to a positive
///    integer to override the tier ceilings at runtime, e.g.
///    `ECHO_MEDIA_CACHE_MB=64` for a 64 MB budget.
/// 2) `ECHO_CACHE_BUDGET_MB` env var (legacy name, still honoured).
/// 3) Adaptive tiers based on total system memory (conservative defaults that
///    keep the decoded-media RSS footprint low; validate RSS and cache
///    hit-rate on a busy channel before reducing them further):
///    - ≤ 8 GB:  72 MB
///    - ≤16 GB: 112 MB
///    - >16 GB: 192 MB
pub fn default_total_budget_bytes() -> usize {
    static CACHED: OnceLock<usize> = OnceLock::new();
    *CACHED.get_or_init(|| {
        // Check the preferred name first, then the legacy name.
        let env_override = echo_core::env::value("ECHO_MEDIA_CACHE_MB")
            .or_else(|| echo_core::env::value("ECHO_CACHE_BUDGET_MB"))
            .and_then(|v| v.parse::<usize>().ok())
            .map(|mb| mb.clamp(32, 4096));

        if let Some(mb) = env_override {
            return mb * 1024 * 1024;
        }

        // Reuse the prewarm probe; no second sysinfo::System allocation.
        let total_gb = (prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);

        // Animation-heavy chat needs enough hot-frame residency to avoid
        // repeatedly evicting and refilling the same visible animated emotes;
        // a too-small frame layer turns animation ticks into cache churn.
        // These ceilings are conservative; if cache hit-rate drops noticeably
        // on a busy channel, raise via ECHO_MEDIA_CACHE_MB before changing
        // the constants here.
        let mb = if total_gb <= 8.5 {
            72
        } else if total_gb <= 16.5 {
            112
        } else {
            192
        };
        mb * 1024 * 1024
    })
}

fn default_picker_total_budget_bytes(base_total: usize) -> usize {
    static CACHED: OnceLock<usize> = OnceLock::new();
    *CACHED.get_or_init(|| {
        if let Some(mb) = echo_core::env::parsed::<usize>("ECHO_PICKER_CACHE_BUDGET_MB")
            .map(|mb| mb.clamp(64, 4096))
        {
            return (mb * 1024 * 1024).max(base_total);
        }

        // Reuse the prewarm probe; no second sysinfo::System allocation.
        let total_gb = (prewarmed_total_memory_bytes() as f64) / (1024.0 * 1024.0 * 1024.0);
        let mb = if total_gb <= 8.5 {
            128
        } else if total_gb <= 16.5 {
            192
        } else {
            256
        };
        (mb * 1024 * 1024).max(base_total)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_allocation() {
        let budget = BudgetManager::with_budget(100_000);

        // Check initial allocations
        assert_eq!(budget.static_budget, 50_000);
        assert_eq!(budget.frame_budget, 40_000);
        assert_eq!(budget.metadata_budget, 10_000);
    }

    #[test]
    fn test_try_allocate() {
        let budget = BudgetManager::with_budget(100);
        budget.reset();

        // Should succeed
        assert!(budget.try_allocate(CacheLayer::Static, 50));
        assert_eq!(budget.usage_for(CacheLayer::Static), 50);

        // Should fail (would exceed the expanded 75-byte static ceiling)
        assert!(!budget.try_allocate(CacheLayer::Static, 30));
        assert_eq!(budget.usage_for(CacheLayer::Static), 50);
    }

    #[test]
    fn test_release() {
        let budget = BudgetManager::with_budget(100);
        budget.reset();

        budget.allocate(CacheLayer::Frames, 25);
        assert_eq!(budget.usage_for(CacheLayer::Frames), 25);

        budget.release(CacheLayer::Frames, 10);
        assert_eq!(budget.usage_for(CacheLayer::Frames), 15);
    }

    #[test]
    fn test_usage_percent() {
        let budget = BudgetManager::with_budget(100);
        budget.reset();

        budget.allocate(CacheLayer::Static, 30);
        budget.allocate(CacheLayer::Frames, 15);
        budget.allocate(CacheLayer::Metadata, 5);

        assert_eq!(budget.total_usage(), 50);
        assert!((budget.usage_percent() - 50.0).abs() < 0.01);
    }
}
