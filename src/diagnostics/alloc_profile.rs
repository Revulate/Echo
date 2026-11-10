//! Opt-in allocation counters for render-path profiling.
//!
//! Build with `--features alloc-profile`, then enable with
//! `ECHO_ALLOC_PROFILE=1`. Standard distribution builds use their allocator
//! directly and compile the counter calls down to no-ops.

#[cfg(all(
    feature = "alloc-profile",
    not(any(target_os = "linux", all(windows, feature = "windows-mimalloc")))
))]
use std::alloc::System;
#[cfg(feature = "alloc-profile")]
use std::alloc::{GlobalAlloc, Layout};
#[cfg(feature = "alloc-profile")]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The allocator a build without `alloc-profile` uses (see `main.rs`).
#[cfg(all(
    feature = "alloc-profile",
    any(target_os = "linux", all(windows, feature = "windows-mimalloc"))
))]
type InnerAllocator = mimalloc::MiMalloc;
#[cfg(all(
    feature = "alloc-profile",
    not(any(target_os = "linux", all(windows, feature = "windows-mimalloc")))
))]
type InnerAllocator = System;

/// Diagnostic global allocator wrapper with runtime-controlled counters.
#[cfg(feature = "alloc-profile")]
pub struct CountingAllocator {
    inner: InnerAllocator,
}

#[cfg(feature = "alloc-profile")]
impl CountingAllocator {
    pub const fn new() -> Self {
        Self {
            #[cfg(any(target_os = "linux", all(windows, feature = "windows-mimalloc")))]
            inner: mimalloc::MiMalloc,
            #[cfg(not(any(target_os = "linux", all(windows, feature = "windows-mimalloc"))))]
            inner: System,
        }
    }
}

#[cfg(feature = "alloc-profile")]
impl Default for CountingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllocationFrameStats {
    pub alloc_count: u64,
    pub alloc_bytes: u64,
    pub dealloc_count: u64,
    pub dealloc_bytes: u64,
    pub realloc_count: u64,
}

#[cfg(feature = "alloc-profile")]
static ENABLED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "alloc-profile")]
static FRAME_ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static FRAME_ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static FRAME_DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static FRAME_DEALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static FRAME_REALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static LAST_ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static LAST_ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static LAST_DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static LAST_DEALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "alloc-profile")]
static LAST_REALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Initialize allocation profiling from `ECHO_ALLOC_PROFILE`.
pub fn init() {
    #[cfg(feature = "alloc-profile")]
    {
        let enabled = echo_core::env::flag("ECHO_ALLOC_PROFILE");
        ENABLED.store(enabled, Ordering::Relaxed);
        if enabled {
            tracing::info!("Allocation profiling enabled");
        }
    }
    #[cfg(not(feature = "alloc-profile"))]
    if echo_core::env::flag("ECHO_ALLOC_PROFILE") {
        tracing::warn!("ECHO_ALLOC_PROFILE ignored: rebuild Echo with --features alloc-profile");
    }
}

#[inline]
pub fn is_enabled() -> bool {
    #[cfg(feature = "alloc-profile")]
    {
        ENABLED.load(Ordering::Relaxed)
    }
    #[cfg(not(feature = "alloc-profile"))]
    {
        false
    }
}

#[inline]
pub fn reset_frame_counters() {
    #[cfg(feature = "alloc-profile")]
    {
        if !is_enabled() {
            return;
        }
        FRAME_ALLOC_COUNT.store(0, Ordering::Relaxed);
        FRAME_ALLOC_BYTES.store(0, Ordering::Relaxed);
        FRAME_DEALLOC_COUNT.store(0, Ordering::Relaxed);
        FRAME_DEALLOC_BYTES.store(0, Ordering::Relaxed);
        FRAME_REALLOC_COUNT.store(0, Ordering::Relaxed);
    }
}

#[inline]
pub fn record_frame_snapshot() {
    #[cfg(feature = "alloc-profile")]
    {
        if !is_enabled() {
            return;
        }
        LAST_ALLOC_COUNT.store(FRAME_ALLOC_COUNT.load(Ordering::Relaxed), Ordering::Relaxed);
        LAST_ALLOC_BYTES.store(FRAME_ALLOC_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
        LAST_DEALLOC_COUNT.store(
            FRAME_DEALLOC_COUNT.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        LAST_DEALLOC_BYTES.store(
            FRAME_DEALLOC_BYTES.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        LAST_REALLOC_COUNT.store(
            FRAME_REALLOC_COUNT.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }
}

#[inline]
pub fn last_frame_stats() -> AllocationFrameStats {
    #[cfg(feature = "alloc-profile")]
    {
        if !is_enabled() {
            return AllocationFrameStats::default();
        }

        AllocationFrameStats {
            alloc_count: LAST_ALLOC_COUNT.load(Ordering::Relaxed),
            alloc_bytes: LAST_ALLOC_BYTES.load(Ordering::Relaxed),
            dealloc_count: LAST_DEALLOC_COUNT.load(Ordering::Relaxed),
            dealloc_bytes: LAST_DEALLOC_BYTES.load(Ordering::Relaxed),
            realloc_count: LAST_REALLOC_COUNT.load(Ordering::Relaxed),
        }
    }
    #[cfg(not(feature = "alloc-profile"))]
    {
        AllocationFrameStats::default()
    }
}

#[cfg(feature = "alloc-profile")]
#[inline]
fn record_alloc(bytes: usize) {
    if !is_enabled() {
        return;
    }
    FRAME_ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    FRAME_ALLOC_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

#[cfg(feature = "alloc-profile")]
#[inline]
fn record_dealloc(bytes: usize) {
    if !is_enabled() {
        return;
    }
    FRAME_DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    FRAME_DEALLOC_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

#[cfg(feature = "alloc-profile")]
#[inline]
fn record_realloc(old_bytes: usize, new_bytes: usize) {
    if !is_enabled() {
        return;
    }
    FRAME_REALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    FRAME_ALLOC_BYTES.fetch_add(new_bytes as u64, Ordering::Relaxed);
    FRAME_DEALLOC_BYTES.fetch_add(old_bytes as u64, Ordering::Relaxed);
}

#[cfg(feature = "alloc-profile")]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the allocator contract unchanged to the wrapped allocator.
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_dealloc(layout.size());
        // SAFETY: `ptr` and `layout` retain the caller-provided allocator contract.
        unsafe { self.inner.dealloc(ptr, layout) };
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwards the allocator contract unchanged to the wrapped allocator.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwards the caller-provided reallocation contract unchanged.
        let new_ptr = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            record_realloc(layout.size(), new_size);
        }
        new_ptr
    }
}
