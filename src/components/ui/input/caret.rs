//! Platform caret blink timing.

use std::time::Duration;

const DEFAULT_CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
#[cfg(target_os = "windows")]
const MIN_CARET_BLINK_INTERVAL_MS: u64 = 100;

/// Return the platform's caret visibility-toggle interval.
///
/// Windows exposes the user's configured half-period directly. `u32::MAX`
/// represents disabled blinking, in which case custom carets stay visible.
/// Other platforms use a 530 ms half-period until GPUI exposes
/// a native cursor-flash preference there.
#[cfg(target_os = "windows")]
pub fn platform_caret_blink_interval() -> Option<Duration> {
    // SAFETY: GetCaretBlinkTime takes no arguments and has no preconditions.
    let millis = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
    match millis {
        u32::MAX => None,
        0 => Some(DEFAULT_CARET_BLINK_INTERVAL),
        millis => Some(Duration::from_millis(
            u64::from(millis).max(MIN_CARET_BLINK_INTERVAL_MS),
        )),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn platform_caret_blink_interval() -> Option<Duration> {
    Some(DEFAULT_CARET_BLINK_INTERVAL)
}
