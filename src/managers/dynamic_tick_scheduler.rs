//! Dynamic Tick Scheduler - Per-emote native FPS animation playback
//!
//!
//! Instead of forcing all animated emotes to a fixed 30fps, this scheduler
//! calculates the optimal tick interval based on the visible emotes' native
//! frame durations. This allows:
//! - 60fps emotes to play at 60fps
//! - 10fps emotes to tick only 10 times/second (CPU savings)
//! - Mixed FPS emotes to all play correctly
//!
//! # Algorithm
//!
//! The scheduler collects frame durations from all visible animated emotes
//! and calculates the minimum tick interval needed to accurately display
//! all animations. This is done using a "minimum frame duration" approach
//! with sensible bounds.
//!
//! # Example
//!
//! ```text
//! Visible emotes: [60fps (16ms), 30fps (33ms), 10fps (100ms)]
//! Minimum duration: 16ms
//! Result: Tick every 16ms (62.5fps) - all emotes play correctly
//! ```

use gpui::Window;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cache::frame_layer::AnimationMetadata;
#[cfg(test)]
use echo_render_core::animation_timing::MAX_TICK_INTERVAL_MS;
use echo_render_core::animation_timing::{
    DEFAULT_TICK_INTERVAL_MS, STABLE_MIN_TICK_INTERVAL_MS, interval_from_min_durations,
};
use std::sync::Arc;

/// Default max animation FPS used to derive the minimum tick interval.
///
/// Animated emotes advance on a 20ms (50 FPS) cadence by default unless the
/// user or platform provides a stronger cap.
/// Override with `ECHO_ANIM_MAX_FPS` (e.g. 144 or 165).
const DEFAULT_MAX_ANIM_FPS: u64 = 50;

/// Background frame gate used for ordinary display-derived animation caps.
///
/// This is deliberately shorter than Echo's default 20ms animation cadence.
/// Using the same interval for the producer and GPUI's inactive-window gate can
/// reject an animation request that arrives slightly early, delaying it until a
/// later compositor wake and making unfocused chat visibly stutter.
const DEFAULT_INACTIVE_FRAME_GATE_INTERVAL_MS: u64 = 16;

/// Current calculated tick interval in milliseconds
static CURRENT_INTERVAL_MS: AtomicU64 = AtomicU64::new(DEFAULT_TICK_INTERVAL_MS);

/// Cached minimum frame duration from last update (for diagnostics)
static CACHED_MIN_DURATION_MS: AtomicU64 = AtomicU64::new(DEFAULT_TICK_INTERVAL_MS);

/// Number of visible animated emotes (for diagnostics)
static VISIBLE_ANIMATION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Detected animation cap and source.
static ANIM_CAP_INFO: LazyLock<(u64, &'static str)> = LazyLock::new(detect_max_anim_fps_hz);

/// Minimum tick interval derived from max animation FPS.
static MIN_TICK_INTERVAL_MS: LazyLock<u64> = LazyLock::new(|| {
    let max_fps = ANIM_CAP_INFO.0;
    min_tick_interval_ms_for_cap_fps(max_fps)
});

/// Runtime window-monitor override (set when we can detect the active window's monitor).
/// `0` means "use startup/default detection".
static RUNTIME_ANIM_CAP_FPS_HZ: AtomicU64 = AtomicU64::new(0);
static RUNTIME_MIN_TICK_INTERVAL_MS: AtomicU64 = AtomicU64::new(0);

static TICK_PHASE_START: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);

/// Per-window monitor samples; the runtime cap is the fastest monitor among
/// windows that rendered recently, so two windows on different displays do
/// not flip the cap back and forth.
#[cfg(target_os = "windows")]
static WINDOW_MONITORS: std::sync::Mutex<Vec<WindowMonitorSample>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(target_os = "windows")]
const WINDOW_MONITOR_REFRESH_QUERY_INTERVAL_MS: u64 = 500;
/// `EnumDisplaySettingsW` is re-run for an unchanged monitor only this often,
/// which also picks up refresh-rate changes made in display settings.
#[cfg(target_os = "windows")]
const MONITOR_REFRESH_HZ_TTL_MS: u64 = 30_000;
/// A window that has not rendered for this long no longer votes on the cap.
#[cfg(target_os = "windows")]
const WINDOW_MONITOR_STALE_MS: u64 = 5_000;

#[cfg(target_os = "windows")]
struct WindowMonitorSample {
    hwnd: isize,
    monitor: isize,
    hz: Option<u64>,
    queried_ms: u64,
    hz_queried_ms: u64,
}

/// Detect max animation FPS cap.
///
/// Priority:
/// 1) `ECHO_ANIM_MAX_FPS` override (debug/testing)
/// 2) Platform display refresh (Windows)
/// 3) Default fallback
fn detect_max_anim_fps_hz() -> (u64, &'static str) {
    if let Some(fps) =
        echo_core::env::parsed::<u64>("ECHO_ANIM_MAX_FPS").map(|fps| fps.clamp(30, 360))
    {
        return (fps, "env");
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Graphics::Gdi::{
            DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW,
        };
        use windows::core::PCWSTR;

        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };

        // SAFETY: `mode` is a DEVMODEW with dmSize set, and a null device name means the primary display.
        let ok = unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut mode) }
            .as_bool();
        if ok {
            let hz = mode.dmDisplayFrequency as u64;
            if (30..=360).contains(&hz) {
                return (hz.min(DEFAULT_MAX_ANIM_FPS), "display");
            }
        }
    }

    (DEFAULT_MAX_ANIM_FPS, "default")
}

/// Current animation cap in Hz.
pub fn animation_cap_fps_hz() -> u64 {
    let runtime = RUNTIME_ANIM_CAP_FPS_HZ.load(Ordering::Relaxed);
    if runtime > 0 {
        runtime
    } else {
        ANIM_CAP_INFO.0
    }
}

/// Current animation cap source (`env`, `window`, `display`, or `default`).
pub fn animation_cap_source() -> &'static str {
    if RUNTIME_ANIM_CAP_FPS_HZ.load(Ordering::Relaxed) > 0 {
        return "window";
    }
    ANIM_CAP_INFO.1
}

/// Current minimum tick interval in ms derived from cap.
pub fn animation_min_tick_interval_ms() -> u64 {
    let runtime = RUNTIME_MIN_TICK_INTERVAL_MS.load(Ordering::Relaxed);
    if runtime > 0 {
        runtime
    } else {
        *MIN_TICK_INTERVAL_MS
    }
}

/// Minimum GPUI frame interval while Echo is visible but unfocused.
///
/// Ordinary operation stays bounded to roughly 60fps while leaving enough
/// headroom for the 50fps animation producer and for moving the window between
/// displays with different refresh rates. An explicit animation-FPS environment
/// override keeps control of its requested cadence for diagnostics.
pub fn inactive_frame_gate_interval_ms() -> u64 {
    if ANIM_CAP_INFO.1 == "env" {
        animation_min_tick_interval_ms()
            .saturating_sub(1)
            .max(STABLE_MIN_TICK_INTERVAL_MS)
    } else {
        DEFAULT_INACTIVE_FRAME_GATE_INTERVAL_MS
    }
}

/// Timer delay that keeps split-local animation wakers on a shared cadence grid.
///
/// We cannot observe GPUI's private swap-chain present phase directly, but aligning
/// all animation timers to one display-period-derived grid avoids each split waking
/// at a different offset inside the frame interval.
pub fn aligned_tick_delay(interval_ms: u64) -> std::time::Duration {
    let interval_ms = interval_ms.max(STABLE_MIN_TICK_INTERVAL_MS);
    let elapsed_ms = TICK_PHASE_START.elapsed().as_millis() as u64;
    let remainder = elapsed_ms % interval_ms;
    let delay_ms = if remainder == 0 {
        interval_ms
    } else {
        interval_ms - remainder
    };
    std::time::Duration::from_millis(delay_ms.max(1))
}

#[inline]
fn min_tick_interval_ms_for_cap_fps(max_fps: u64) -> u64 {
    (1000_u64 / max_fps.max(1)).max(STABLE_MIN_TICK_INTERVAL_MS)
}

#[cfg(target_os = "windows")]
#[inline]
fn set_runtime_animation_cap_fps_hz(max_fps: u64) {
    let clamped = max_fps.clamp(30, 360).min(DEFAULT_MAX_ANIM_FPS);
    RUNTIME_ANIM_CAP_FPS_HZ.store(clamped, Ordering::Relaxed);
    RUNTIME_MIN_TICK_INTERVAL_MS
        .store(min_tick_interval_ms_for_cap_fps(clamped), Ordering::Relaxed);
}

/// Update the animation cap from the monitor that currently contains this window.
///
/// This keeps animation pacing aligned with the display Echo is actually on when
/// users move the window between monitors with different refresh rates.
pub fn sync_animation_cap_from_window(window: &Window) {
    #[cfg(target_os = "windows")]
    {
        if ANIM_CAP_INFO.1 == "env" {
            return;
        }

        if let Some(hwnd) = window_hwnd(window) {
            sync_animation_cap_from_hwnd(hwnd);
        }
    }

    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

#[cfg(target_os = "windows")]
fn sync_animation_cap_from_hwnd(hwnd: windows::Win32::Foundation::HWND) {
    use crate::backend::lock_utils::MutexExt;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};

    let now_ms = TICK_PHASE_START.elapsed().as_millis() as u64;
    let hwnd_key = hwnd.0 as isize;
    let mut samples = WINDOW_MONITORS.lock_or_recover();
    if samples.iter().any(|sample| {
        sample.hwnd == hwnd_key
            && now_ms.saturating_sub(sample.queried_ms) < WINDOW_MONITOR_REFRESH_QUERY_INTERVAL_MS
    }) {
        return;
    }

    // SAFETY: MonitorFromWindow accepts any HWND, valid or not; with
    // MONITOR_DEFAULTTONEAREST it only returns null when `hwnd` itself is invalid,
    // which the null check below still guards against.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return;
    }
    let monitor_key = monitor.0 as isize;

    let cached = samples
        .iter()
        .filter(|sample| {
            sample.monitor == monitor_key
                && now_ms.saturating_sub(sample.hz_queried_ms) < MONITOR_REFRESH_HZ_TTL_MS
        })
        .max_by_key(|sample| sample.hz_queried_ms)
        .map(|sample| (sample.hz, sample.hz_queried_ms));
    let (hz, hz_queried_ms) =
        cached.unwrap_or_else(|| (detect_monitor_refresh_hz(monitor), now_ms));

    samples.retain(|sample| {
        sample.hwnd != hwnd_key
            && now_ms.saturating_sub(sample.queried_ms) < WINDOW_MONITOR_STALE_MS
    });
    samples.push(WindowMonitorSample {
        hwnd: hwnd_key,
        monitor: monitor_key,
        hz,
        queried_ms: now_ms,
        hz_queried_ms,
    });

    if let Some(hz) = samples.iter().filter_map(|sample| sample.hz).max() {
        set_runtime_animation_cap_fps_hz(hz);
    }
}

#[cfg(target_os = "windows")]
fn detect_monitor_refresh_hz(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<u64> {
    use windows::Win32::Graphics::Gdi::{
        DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, MONITORINFOEXW,
    };
    use windows::core::PCWSTR;

    let mut monitor_info = MONITORINFOEXW::default();
    monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `monitor_info` is a MONITORINFOEXW with cbSize set, which GetMonitorInfoW accepts through its base struct.
    let ok = unsafe { GetMonitorInfoW(monitor, &mut monitor_info.monitorInfo as *mut _) }.as_bool();
    if !ok {
        return None;
    }

    let mut mode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `szDevice` was NUL-terminated by GetMonitorInfoW, and `mode` has dmSize set.
    let ok = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(monitor_info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
        )
    }
    .as_bool();
    if !ok {
        return None;
    }

    let hz = mode.dmDisplayFrequency as u64;
    if (30..=360).contains(&hz) {
        Some(hz)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn window_hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;

    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// Update the tick interval based on visible emotes' metadata
///
/// This calculates the minimum frame duration across all visible emotes
/// and sets the tick interval to match. The interval is clamped to
/// [MIN_TICK_INTERVAL_MS, MAX_TICK_INTERVAL_MS].
///
/// # Arguments
///
/// * `metadata_list` - Slice of AnimationMetadata from visible emotes
///
/// # Returns
///
/// The new tick interval in milliseconds
pub fn update_from_visible_emotes<'a, I>(metadata_list: I) -> u64
where
    I: IntoIterator<Item = &'a Arc<AnimationMetadata>>,
    I::IntoIter: Clone,
{
    let metadata_list = metadata_list.into_iter();
    let visible_count = metadata_list.clone().count();
    VISIBLE_ANIMATION_COUNT.store(visible_count as u64, Ordering::Relaxed);

    if visible_count == 0 {
        CURRENT_INTERVAL_MS.store(DEFAULT_TICK_INTERVAL_MS, Ordering::Relaxed);
        CACHED_MIN_DURATION_MS.store(DEFAULT_TICK_INTERVAL_MS, Ordering::Relaxed);
        return DEFAULT_TICK_INTERVAL_MS;
    }

    let min_interval = animation_min_tick_interval_ms().max(STABLE_MIN_TICK_INTERVAL_MS);
    let decision = interval_from_min_durations(
        metadata_list.map(|metadata| metadata.min_frame_duration_ms as u64),
        min_interval,
    );
    CACHED_MIN_DURATION_MS.store(decision.min_frame_duration_ms, Ordering::Relaxed);
    CURRENT_INTERVAL_MS.store(decision.interval_ms, Ordering::Relaxed);
    decision.interval_ms
}

/// Reset to default interval (called when no animations are active)
pub fn reset_to_default() {
    CURRENT_INTERVAL_MS.store(DEFAULT_TICK_INTERVAL_MS, Ordering::Relaxed);
    CACHED_MIN_DURATION_MS.store(DEFAULT_TICK_INTERVAL_MS, Ordering::Relaxed);
    VISIBLE_ANIMATION_COUNT.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::sync::Mutex;

    static TEST_STATE_LOCK: Mutex<()> = Mutex::new(());

    fn make_metadata(frame_durations: Vec<u32>) -> Arc<AnimationMetadata> {
        Arc::new(AnimationMetadata::new(frame_durations))
    }

    #[test]
    fn test_stats() {
        let _guard = TEST_STATE_LOCK.lock().expect("scheduler test state lock");
        let runtime_minimum = animation_min_tick_interval_ms().max(STABLE_MIN_TICK_INTERVAL_MS);
        let expected_interval = 20_u64.clamp(runtime_minimum, MAX_TICK_INTERVAL_MS);
        let metadata = make_metadata(vec![20, 20, 20]);
        let returned = update_from_visible_emotes(&[metadata]);

        assert_eq!(returned, expected_interval);
        assert_eq!(
            CURRENT_INTERVAL_MS.load(Ordering::Relaxed),
            expected_interval
        );
        assert_eq!(CACHED_MIN_DURATION_MS.load(Ordering::Relaxed), 20);
        assert_eq!(VISIBLE_ANIMATION_COUNT.load(Ordering::Relaxed), 1);
        let effective_fps = 1000.0 / CURRENT_INTERVAL_MS.load(Ordering::Relaxed) as f64;
        assert!((effective_fps - 1000.0 / expected_interval as f64).abs() < 0.1);

        let returned = update_from_visible_emotes(&[make_metadata(vec![5, 5, 5])]);
        assert_eq!(returned, 5_u64.clamp(runtime_minimum, MAX_TICK_INTERVAL_MS));
        assert_eq!(CURRENT_INTERVAL_MS.load(Ordering::Relaxed), returned);
    }

    #[test]
    fn test_reset() {
        let _guard = TEST_STATE_LOCK.lock().expect("scheduler test state lock");
        let metadata = make_metadata(vec![100, 100, 100]);
        let returned = update_from_visible_emotes(&[metadata]);
        assert_eq!(returned, 100);
        assert_eq!(CURRENT_INTERVAL_MS.load(Ordering::Relaxed), returned);

        reset_to_default();
        assert_eq!(
            CURRENT_INTERVAL_MS.load(Ordering::Relaxed),
            DEFAULT_TICK_INTERVAL_MS
        );
        assert_eq!(VISIBLE_ANIMATION_COUNT.load(Ordering::Relaxed), 0);
    }
}
