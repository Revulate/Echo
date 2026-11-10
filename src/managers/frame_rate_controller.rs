//! Frame Rate Controller - Automatic FPS management based on activity
//!
//! This module provides invisible, automatic frame rate optimization:
//! - 16ms (60fps) after recent user interaction (scrolling, clicking, typing)
//! - 33ms (30fps) when visible animations are active
//! - 100ms (10fps) when recently active but idle
//! - 500ms (2fps) when in deep idle (no activity for 2+ seconds)
//! - 1000ms (1fps) when in true idle (no visible animations, no activity for 5+ seconds)
//!
//! Window focus is tracked for input/UI state, but it must not throttle chat
//! delivery, emote loading, layout work, or animation wakeups. Echo is a live
//! chat client; losing OS focus should not make the app stale.
//!
//! The controller uses atomic state for thread-safe access from both
//! the UI thread and background polling tasks.

use crate::cache::loader as image_cache;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Atomic window focus state
static IS_FOCUSED: AtomicBool = AtomicBool::new(true);

/// Atomic timestamp of last user interaction (ms since UNIX epoch)
/// Zero means no interaction yet: only this drives the Interactive
/// keep-alive, and `now - MAX` would read as input happening forever until
/// the first real interaction.
static LAST_INTERACTION_MS: AtomicU64 = AtomicU64::new(0);

/// Atomic timestamp of last activity (messages, events, etc.) (ms since UNIX epoch)
/// Initialized to MAX so `now - MAX` wraps to ~0 via saturating_sub,
/// keeping us out of TrueIdle/DeepIdle until the first real activity.
/// Without this, the polling coordinator enters TrueIdle on its very first
/// iteration (because `now - 0` >> threshold), skipping channel initialization
/// and causing chat to never load until a tab switch.
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Atomic timestamp of the last IRC message observed by the backend.
static LAST_IRC_MESSAGE_MS: AtomicU64 = AtomicU64::new(0);

/// Atomic timestamp of the last observer drain applied on the UI thread.
static LAST_OBSERVER_DRAIN_MS: AtomicU64 = AtomicU64::new(0);

/// Atomic timestamp of the last image completion/hydration notification.
static LAST_IMAGE_READY_MS: AtomicU64 = AtomicU64::new(0);

/// Consecutive low-cost canvas frames. Used to drop idle cadence sooner only
/// after several cheap render passes, not from one lucky sample.
static LOW_CANVAS_CPU_IDLE_STREAK: AtomicU64 = AtomicU64::new(0);

/// Wakeup signal for event-driven idle modes
/// Used to wake up from TrueIdle when messages arrive or user interacts
static WAKEUP_NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Cached render mode (stored as u8 discriminant)
static CACHED_MODE: AtomicU64 = AtomicU64::new(0);

/// Timestamp when mode was last calculated (ms since UNIX epoch)
static MODE_CACHE_TIME_MS: AtomicU64 = AtomicU64::new(0);

/// How long to cache the mode calculation (ms)
/// PERF: Avoid recalculating mode every poll cycle.
/// Kept at one 60fps frame so Animating-mode recognition does not lag by up to a
/// full 50ms after animations become visible (which would otherwise stall the
/// first animation frame). The cache is invalidated immediately on activity, so
/// this only bounds the worst-case staleness when no activity event fires.
const MODE_CACHE_DURATION_MS: u64 = 16;

/// Keep-alive duration in milliseconds (full 60fps responsiveness after user input)
const KEEP_ALIVE_DURATION_MS: u64 = 500;

/// Deep idle threshold in milliseconds (2 seconds of no activity triggers deep idle)
const DEEP_IDLE_THRESHOLD_MS: u64 = 2000;

/// True idle threshold in milliseconds (5 seconds of no activity + no visible animations)
const TRUE_IDLE_THRESHOLD_MS: u64 = 5000;

/// Canvas CPU threshold below which the renderer is considered settled.
const LOW_CANVAS_CPU_IDLE_THRESHOLD_MS: f64 = 2.0;
/// Number of consecutive low-cost samples needed before fast idle is allowed.
const LOW_CANVAS_CPU_IDLE_STREAK_TARGET: u64 = 4;
/// Faster deep-idle transition once recent frames prove the canvas is cheap.
const LOW_CANVAS_CPU_FAST_IDLE_THRESHOLD_MS: u64 = 1000;

/// Render mode determines polling interval and frame behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Full speed rendering (16ms/60fps) - user actively interacting
    Interactive,
    /// Animation speed (33ms/30fps) - emotes animating but no user input
    Animating,
    /// Reduced polling (50ms/20fps) - focused but idle, animations may be present
    FocusedIdle,
    /// Deep idle (200ms/5fps) - no activity for 5+ seconds, near-zero CPU
    DeepIdle,
    /// True idle (1000ms/1fps) - no visible animations, 10+ seconds idle
    /// Uses event-driven wakeup for near-zero CPU
    TrueIdle,
}

/// Snapshot of frame-loop liveness state for debug surfaces.
#[derive(Debug, Clone)]
pub struct FrameRateDiagnostics {
    pub mode: RenderMode,
    pub focused: bool,
    pub visible_animations: bool,
    pub skipping_frames: bool,
    pub last_interaction_ms_ago: Option<u64>,
    pub last_activity_ms_ago: Option<u64>,
    pub last_irc_message_ms_ago: Option<u64>,
    pub last_observer_drain_ms_ago: Option<u64>,
    pub last_image_ready_ms_ago: Option<u64>,
}

impl RenderMode {
    /// Get the polling interval for this render mode
    pub fn poll_interval(&self) -> Duration {
        match self {
            RenderMode::Interactive => Duration::from_millis(16),
            RenderMode::Animating => Duration::from_millis(33),
            RenderMode::FocusedIdle => Duration::from_millis(100),
            RenderMode::DeepIdle => Duration::from_millis(500),
            RenderMode::TrueIdle => Duration::from_millis(1000),
        }
    }

    /// Check if this mode uses event-driven wakeup instead of continuous polling
    pub fn is_event_driven(&self) -> bool {
        matches!(self, RenderMode::TrueIdle)
    }

    /// Convert to u64 for atomic storage
    fn to_u64(self) -> u64 {
        match self {
            RenderMode::Interactive => 0,
            RenderMode::Animating => 1,
            RenderMode::FocusedIdle => 2,
            RenderMode::DeepIdle => 3,
            RenderMode::TrueIdle => 4,
        }
    }

    /// Convert from u64 (with fallback to Interactive)
    fn from_u64(val: u64) -> Self {
        match val {
            0 => RenderMode::Interactive,
            1 => RenderMode::Animating,
            2 => RenderMode::FocusedIdle,
            3 => RenderMode::DeepIdle,
            4 => RenderMode::TrueIdle,
            _ => RenderMode::Interactive,
        }
    }
}
fn age_ms(now: u64, timestamp: u64) -> Option<u64> {
    if timestamp == 0 || timestamp == u64::MAX {
        None
    } else {
        Some(now.saturating_sub(timestamp))
    }
}

fn record_activity_at(now: u64) {
    LAST_ACTIVITY_MS.store(now, Ordering::Relaxed);
    MODE_CACHE_TIME_MS.store(0, Ordering::Relaxed);
    WAKEUP_NOTIFY.notify_one();
}

/// Update window focus state (call from window activation observer)
pub fn set_focused(focused: bool) {
    let was_focused = IS_FOCUSED.swap(focused, Ordering::Relaxed);

    if focused {
        record_interaction();
    }

    if focused != was_focused {
        MODE_CACHE_TIME_MS.store(0, Ordering::Relaxed);
        wakeup();
    }
}

/// Record a user interaction (keyboard, mouse, scroll)
/// This resets the keep-alive timer and activity timer
pub fn record_interaction() {
    let now = crate::utils::unix_now_ms();
    LAST_INTERACTION_MS.store(now, Ordering::Relaxed);
    record_activity_at(now);
}

/// Record background activity (incoming messages, events)
/// This prevents deep idle when messages are arriving
pub fn record_activity() {
    record_activity_at(crate::utils::unix_now_ms());
}

/// Record an IRC message. This is separate from generic activity so liveness
/// diagnostics can distinguish network traffic from UI/image wakeups.
pub fn record_irc_message() {
    let now = crate::utils::unix_now_ms();
    LAST_IRC_MESSAGE_MS.store(now, Ordering::Relaxed);
    record_activity_at(now);
}

/// Record that observer events were drained into GPUI-owned state.
pub fn record_observer_drain() {
    let now = crate::utils::unix_now_ms();
    LAST_OBSERVER_DRAIN_MS.store(now, Ordering::Relaxed);
    record_activity_at(now);
}

/// Record that an image finished loading or hydrating.
pub fn record_image_ready() {
    let now = crate::utils::unix_now_ms();
    LAST_IMAGE_READY_MS.store(now, Ordering::Relaxed);
    record_activity_at(now);
}

/// Check if we're within the keep-alive window after user input, relative to a
/// caller-provided `now` timestamp (avoids a redundant `SystemTime` syscall when
/// several mode checks share one evaluation).
#[inline]
fn is_in_keep_alive_at(now: u64) -> bool {
    let last = LAST_INTERACTION_MS.load(Ordering::Relaxed);
    now.saturating_sub(last) < KEEP_ALIVE_DURATION_MS
}

/// Check if we're in deep idle relative to a caller-provided `now` timestamp.
#[inline]
fn is_deep_idle_at(now: u64) -> bool {
    let last_activity = LAST_ACTIVITY_MS.load(Ordering::Relaxed);
    now.saturating_sub(last_activity) >= DEEP_IDLE_THRESHOLD_MS
}

#[inline]
fn canvas_cpu_allows_fast_idle() -> bool {
    let canvas_cpu_ms = crate::fps_profiler::canvas_cpu_ms();
    if canvas_cpu_ms > 0.0 && canvas_cpu_ms <= LOW_CANVAS_CPU_IDLE_THRESHOLD_MS {
        let next = LOW_CANVAS_CPU_IDLE_STREAK
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        next >= LOW_CANVAS_CPU_IDLE_STREAK_TARGET
    } else {
        LOW_CANVAS_CPU_IDLE_STREAK.store(0, Ordering::Relaxed);
        false
    }
}

/// Check if we're in true idle relative to a caller-provided `now` timestamp.
#[inline]
fn is_true_idle_at(now: u64) -> bool {
    let last_activity = LAST_ACTIVITY_MS.load(Ordering::Relaxed);
    now.saturating_sub(last_activity) >= TRUE_IDLE_THRESHOLD_MS
        && !image_cache::has_visible_animations()
}

/// Wake up from TrueIdle mode
///
/// Call this when messages arrive or user interacts to immediately
/// wake up from event-driven idle modes.
pub fn wakeup() {
    WAKEUP_NOTIFY.notify_one();
}

/// Get reference to the wakeup notify for use in tokio::select!
pub fn get_wakeup_notify() -> &'static tokio::sync::Notify {
    &WAKEUP_NOTIFY
}

/// Determine the current render mode based on interaction and animation state.
///
/// Focus is intentionally ignored. Losing focus must not make Echo stale.
pub fn current_mode() -> RenderMode {
    let now = crate::utils::unix_now_ms();
    if is_in_keep_alive_at(now) {
        RenderMode::Interactive
    } else if image_cache::has_visible_animations() && !crate::fps_profiler::canvas_paint_stalled()
    {
        RenderMode::Animating
    } else if is_true_idle_at(now) {
        RenderMode::TrueIdle
    } else if is_deep_idle_at(now)
        || (canvas_cpu_allows_fast_idle()
            && now.saturating_sub(LAST_ACTIVITY_MS.load(Ordering::Relaxed))
                >= LOW_CANVAS_CPU_FAST_IDLE_THRESHOLD_MS)
    {
        RenderMode::DeepIdle
    } else {
        RenderMode::FocusedIdle
    }
}

/// Get current render mode with caching to avoid recalculation overhead
///
/// PERF: Mode calculation involves several atomic loads and time comparisons.
/// This cached version reuses the result for up to MODE_CACHE_DURATION_MS.
/// Safe to call frequently from polling loops.
pub fn current_mode_cached() -> RenderMode {
    let now = crate::utils::unix_now_ms();
    let cache_time = MODE_CACHE_TIME_MS.load(Ordering::Relaxed);

    if now.saturating_sub(cache_time) < MODE_CACHE_DURATION_MS {
        return RenderMode::from_u64(CACHED_MODE.load(Ordering::Relaxed));
    }

    let mode = current_mode();
    CACHED_MODE.store(mode.to_u64(), Ordering::Relaxed);
    MODE_CACHE_TIME_MS.store(now, Ordering::Relaxed);
    mode
}

/// Check if this frame should be skipped.
///
/// Echo never skips frames because of OS focus. Returning `false` keeps chat,
/// emote loading, and visible animation scheduling live while unfocused.
pub fn should_skip_frame() -> bool {
    false
}

/// Snapshot current frame-loop diagnostics for debug UI and live logs.
pub fn diagnostics() -> FrameRateDiagnostics {
    let now = crate::utils::unix_now_ms();
    FrameRateDiagnostics {
        mode: current_mode_cached(),
        focused: IS_FOCUSED.load(Ordering::Relaxed),
        visible_animations: image_cache::has_visible_animations(),
        skipping_frames: should_skip_frame(),
        last_interaction_ms_ago: age_ms(now, LAST_INTERACTION_MS.load(Ordering::Relaxed)),
        last_activity_ms_ago: age_ms(now, LAST_ACTIVITY_MS.load(Ordering::Relaxed)),
        last_irc_message_ms_ago: age_ms(now, LAST_IRC_MESSAGE_MS.load(Ordering::Relaxed)),
        last_observer_drain_ms_ago: age_ms(now, LAST_OBSERVER_DRAIN_MS.load(Ordering::Relaxed)),
        last_image_ready_ms_ago: age_ms(now, LAST_IMAGE_READY_MS.load(Ordering::Relaxed)),
    }
}
