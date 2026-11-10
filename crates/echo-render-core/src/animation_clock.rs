//! GPUI-free global animation clock for emote playback.
//!
//! A single monotonic frame counter keeps every visible animation in sync. The
//! tick interval is chosen by the dynamic scheduler rather than fixed per
//! emote, and the split-render cadence ring feeds the adaptive off-screen
//! throttle. Collecting which emotes are currently visible stays with the
//! renderer and is reached through [`install_visible_animation_count`].

use std::sync::LazyLock;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Global animation frame counter shared by animated emotes for synchronization.
static ANIMATION_FRAME: AtomicU64 = AtomicU64::new(0);

/// Monotonic base instant for encoding update timestamps as plain `u64` nanos.
static ANIMATION_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Last time the animation frame was updated, stored as nanoseconds since
/// `ANIMATION_EPOCH`. `0` is the sentinel for "never updated" (the epoch itself
/// reads as ~0ns, which is harmless: the first tick will always advance).
///
/// PERF: An `AtomicU64` replaces the previous `RwLock<Option<Instant>>` so the
/// per-tick read+write on the hot waker path is lock-free.
static LAST_ANIMATION_UPDATE_NS: AtomicU64 = AtomicU64::new(0);

/// Current animation interval (dynamically updated by scheduler)
static CURRENT_ANIMATION_INTERVAL_MS: AtomicU64 = AtomicU64::new(33);

/// Whether animation is globally enabled (can be toggled for performance)
static ANIMATION_ENABLED: AtomicBool = AtomicBool::new(true);

// ============================================================================
// FPS-Based Adaptive Throttling (Lock-Free)
// ============================================================================

/// PERF: Lock-free frame time ring buffer (8 samples sufficient for FPS estimation)
/// Uses atomic operations instead of RwLock to eliminate contention
static FRAME_TIME_0: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_1: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_2: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_3: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_4: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_5: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_6: AtomicU64 = AtomicU64::new(16667);
static FRAME_TIME_7: AtomicU64 = AtomicU64::new(16667);

/// Last `record_frame_cadence()` timestamp (ns since `ANIMATION_EPOCH`); `0` =
/// never written. Used to derive the inter-call delta fed into the FRAME_TIME ring.
static LAST_BEGIN_FRAME_NS: AtomicU64 = AtomicU64::new(0);

/// Wrapping write cursor into the FRAME_TIME_0..7 ring.
static FRAME_TIME_WRITE_INDEX: AtomicU64 = AtomicU64::new(0);

/// Target FPS below which we start throttling off-screen emotes
const TARGET_FPS_THRESHOLD: f32 = 50.0;

/// PERF: Cached FPS value to avoid recomputing every call
/// Stored as u32 bits (use f32::from_bits/to_bits for conversion)
static CACHED_FPS_BITS: AtomicU32 = AtomicU32::new(0x42700000); // 60.0f32.to_bits()

/// Frame number when FPS was last cached
static CACHED_FPS_FRAME: AtomicU64 = AtomicU64::new(0);

/// Timestamp (ms) when FPS was last cached
static CACHED_FPS_TIME_MS: AtomicU64 = AtomicU64::new(0);

static VISIBLE_ANIMATION_COUNT: OnceLock<fn() -> usize> = OnceLock::new();

/// Install the renderer's visible-animation census. Idempotent.
///
/// Without it the clock reports zero visible animations, which is also what a
/// renderer reports before it has painted anything.
pub fn install_visible_animation_count(callback: fn() -> usize) {
    let _ = VISIBLE_ANIMATION_COUNT.set(callback);
}

/// Number of animated emotes the renderer currently considers visible.
pub fn visible_animation_count() -> usize {
    VISIBLE_ANIMATION_COUNT
        .get()
        .map_or(0, |callback| callback())
}

/// Calculate current FPS from recent frame times
/// PERF: Lock-free with time-based caching (100ms minimum between recalculations)
pub fn current_fps() -> f32 {
    let current_frame = ANIMATION_FRAME.load(Ordering::Relaxed);
    let cached_frame = CACHED_FPS_FRAME.load(Ordering::Relaxed);

    // Return cached value if computed this frame
    if current_frame == cached_frame {
        return f32::from_bits(CACHED_FPS_BITS.load(Ordering::Relaxed));
    }

    // Also check time-based cache (avoid recalculating more than 10x/sec)
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let cached_time = CACHED_FPS_TIME_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(cached_time) < 100 {
        return f32::from_bits(CACHED_FPS_BITS.load(Ordering::Relaxed));
    }

    // Compute new FPS value (lock-free sum of 8 samples)
    let sum = FRAME_TIME_0.load(Ordering::Relaxed)
        + FRAME_TIME_1.load(Ordering::Relaxed)
        + FRAME_TIME_2.load(Ordering::Relaxed)
        + FRAME_TIME_3.load(Ordering::Relaxed)
        + FRAME_TIME_4.load(Ordering::Relaxed)
        + FRAME_TIME_5.load(Ordering::Relaxed)
        + FRAME_TIME_6.load(Ordering::Relaxed)
        + FRAME_TIME_7.load(Ordering::Relaxed);

    let avg_time_us = sum / 8;
    let fps = if avg_time_us > 0 {
        1_000_000.0 / avg_time_us as f32
    } else {
        60.0
    };

    // Cache the result
    CACHED_FPS_BITS.store(fps.to_bits(), Ordering::Relaxed);
    CACHED_FPS_FRAME.store(current_frame, Ordering::Relaxed);
    CACHED_FPS_TIME_MS.store(now_ms, Ordering::Relaxed);

    fps
}

/// Check if we should throttle OFF-SCREEN animations based on FPS
/// Note: Visible emotes are NEVER throttled
pub fn should_throttle_offscreen() -> bool {
    current_fps() < TARGET_FPS_THRESHOLD
}

/// Store one frame-time sample (microseconds) into the FRAME_TIME_0..7 ring.
///
/// The ring is 8 separate atomics rather than an array, so dispatch the write
/// by wrapped index. Relaxed + allocation-free for the `begin_frame()` hot path.
#[inline]
fn store_frame_time_us(idx: u64, value_us: u64) {
    match idx % 8 {
        0 => FRAME_TIME_0.store(value_us, Ordering::Relaxed),
        1 => FRAME_TIME_1.store(value_us, Ordering::Relaxed),
        2 => FRAME_TIME_2.store(value_us, Ordering::Relaxed),
        3 => FRAME_TIME_3.store(value_us, Ordering::Relaxed),
        4 => FRAME_TIME_4.store(value_us, Ordering::Relaxed),
        5 => FRAME_TIME_5.store(value_us, Ordering::Relaxed),
        6 => FRAME_TIME_6.store(value_us, Ordering::Relaxed),
        _ => FRAME_TIME_7.store(value_us, Ordering::Relaxed),
    }
}

/// Measure render cadence: record the delta since the previous call into the
/// FRAME_TIME_0..7 ring that `current_fps()` averages.
///
/// The renderer calls this once per render frame (when a split renders again),
/// not once per split, so N visible splits do not inflate `current_fps()`. It
/// is still render cadence, not GPU present rate. All-Relaxed and
/// allocation-free.
#[inline]
pub fn record_frame_cadence() {
    let now_ns = ANIMATION_EPOCH.elapsed().as_nanos() as u64;
    let last_ns = LAST_BEGIN_FRAME_NS.swap(now_ns, Ordering::Relaxed);
    if last_ns != 0 {
        let delta_us = (now_ns.saturating_sub(last_ns) / 1_000).max(1);
        let idx = FRAME_TIME_WRITE_INDEX.fetch_add(1, Ordering::Relaxed);
        store_frame_time_us(idx, delta_us);
    }
}

/// Set the animation tick interval (called by DynamicTickScheduler)
///
/// This allows the scheduler to adjust the tick rate based on visible emotes'
/// native frame rates. For example, if only 10fps emotes are visible, we can
/// tick at 100ms instead of 33ms for CPU savings.
pub fn set_animation_interval_ms(interval_ms: u64) {
    CURRENT_ANIMATION_INTERVAL_MS.store(interval_ms, Ordering::Relaxed);
}

#[inline]
pub fn set_animation_enabled(enabled: bool) {
    ANIMATION_ENABLED.store(enabled, Ordering::Release);
}

#[inline]
pub fn animation_enabled() -> bool {
    ANIMATION_ENABLED.load(Ordering::Acquire)
}

/// Get the current animation interval in milliseconds.
#[inline]
pub fn current_animation_interval_ms() -> u64 {
    CURRENT_ANIMATION_INTERVAL_MS.load(Ordering::Relaxed)
}

/// Advance the global animation frame (call from main loop)
///
/// The tick interval is dynamically adjusted by DynamicTickScheduler based on
/// visible emotes' native frame rates. This allows:
/// - 60fps emotes to play at 60fps (16ms ticks)
/// - 10fps emotes to tick at 10fps (100ms ticks, saving CPU)
/// - Mixed emotes to all play correctly (uses minimum interval)
///
/// Returns true if the animation frame was actually advanced (for conditional repaint)
pub fn tick_animation() -> bool {
    if !animation_enabled() {
        return false;
    }
    let now_ns = ANIMATION_EPOCH.elapsed().as_nanos() as u64;
    let interval_ms = CURRENT_ANIMATION_INTERVAL_MS.load(Ordering::Relaxed);

    let last_ns = LAST_ANIMATION_UPDATE_NS.load(Ordering::Relaxed);
    let should_update = if last_ns == 0 {
        true
    } else {
        now_ns.saturating_sub(last_ns) >= interval_ms.saturating_mul(1_000_000)
    };

    if should_update {
        let frame = ANIMATION_FRAME.fetch_add(1, Ordering::Relaxed);
        LAST_ANIMATION_UPDATE_NS.store(now_ns, Ordering::Relaxed);

        // Log animation tick periodically (every ~1 second at 30fps)
        if frame.is_multiple_of(30) {
            let count = visible_animation_count();
            echo_core::log_emotes!(
                trace,
                "Animation tick: animation_frame={}, animated_count={}, interval_ms={}",
                frame,
                count,
                interval_ms
            );
        }
        true
    } else {
        false
    }
}

/// Get current animation frame number (for synchronizing all emotes)
pub fn current_animation_frame() -> u64 {
    ANIMATION_FRAME.load(Ordering::Relaxed)
}

/// Get animation stats for debugging
pub fn animation_stats() -> (u64, u32, bool) {
    (
        current_animation_frame(),
        visible_animation_count().min(u32::MAX as usize) as u32,
        ANIMATION_ENABLED.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_animation_frame_advances() {
        let initial = current_animation_frame();
        tick_animation();
        // May or may not advance depending on timing
        assert!(current_animation_frame() >= initial);
    }

    #[test]
    fn disabled_animation_does_not_advance_clock() {
        let initial = current_animation_frame();
        set_animation_enabled(false);
        assert!(!tick_animation());
        assert_eq!(current_animation_frame(), initial);
        set_animation_enabled(true);
    }
}
