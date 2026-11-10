//! Window utility functions

use crate::backend::lock_utils::MutexExt;
use gpui::{App, Bounds, DisplayId, Pixels, Point, Size, Window, WindowId, point, px};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Track last click time for double-click detection (milliseconds since epoch)
static LAST_CLICK_TIME: AtomicU64 = AtomicU64::new(0);

/// Track whether a drag operation is pending (waiting for mouse move)
static DRAG_PENDING: AtomicBool = AtomicBool::new(false);

/// The display currently hosting Echo's main workspace. GPUI updates a
/// window's display association as it crosses monitors; the main render path
/// refreshes this cache so secondary-window requests can resolve placement
/// even when they are dispatched from inside that window's update stack.
static MAIN_DISPLAY_ID: AtomicU64 = AtomicU64::new(0);
static MAIN_DISPLAY_KNOWN: AtomicBool = AtomicBool::new(false);

/// A preferred overlay size fitted inside one application viewport.
#[derive(Clone, Copy, Debug)]
pub struct FittedOverlaySize {
    pub width: f32,
    pub height: f32,
    pub margin_x: f32,
    pub margin_y: f32,
}

/// Fit a tooltip, menu, alert, or modal inside the owning application window.
/// Margins shrink only when the viewport itself is smaller than the normal
/// inset, preserving some usable content even during extreme window resizing.
pub fn fit_overlay_to_viewport(
    viewport: Size<Pixels>,
    preferred_width: f32,
    preferred_height: f32,
    margin: f32,
) -> FittedOverlaySize {
    let viewport_width = f32::from(viewport.width).max(0.0);
    let viewport_height = f32::from(viewport.height).max(0.0);
    let margin_x = margin.max(0.0).min(viewport_width * 0.1);
    let margin_y = margin.max(0.0).min(viewport_height * 0.1);

    FittedOverlaySize {
        width: preferred_width
            .max(0.0)
            .min((viewport_width - margin_x * 2.0).max(0.0)),
        height: preferred_height
            .max(0.0)
            .min((viewport_height - margin_y * 2.0).max(0.0)),
        margin_x,
        margin_y,
    }
}

/// Clamp one overlay axis so both its leading and trailing edge remain inside
/// the viewport. Callers should first fit the size with
/// [`fit_overlay_to_viewport`] when the preferred surface can exceed the window.
pub fn clamp_overlay_axis(
    requested_position: f32,
    fitted_size: f32,
    viewport_size: f32,
    margin: f32,
) -> f32 {
    let viewport_size = viewport_size.max(0.0);
    let margin = margin.max(0.0).min(viewport_size / 2.0);
    let minimum = margin;
    let maximum = (viewport_size - fitted_size.max(0.0) - margin).max(minimum);
    requested_position.clamp(minimum, maximum)
}

thread_local! {
    /// Main-window bounds and scale factor at the last successful display
    /// lookup. GPUI re-resolves a window's display only on a bounds change, so
    /// an unchanged key means the display is unchanged too.
    static MAIN_DISPLAY_KEY: Cell<Option<(Bounds<Pixels>, f32)>> = const { Cell::new(None) };
}

/// Snapshot the main Echo window's current display for subsequent native
/// popup creation. This must be called only from the main workspace render.
///
/// `Window::display` enumerates every monitor on Windows, so the lookup runs
/// only when the window's bounds or scale factor change.
pub fn remember_main_window_display(window: &Window, cx: &App) {
    remember_main_window_handle(window);
    let key = (window.bounds(), window.scale_factor());
    if MAIN_DISPLAY_KEY.with(Cell::get) == Some(key) {
        return;
    }
    let Some(display) = window.display(cx) else {
        return;
    };
    MAIN_DISPLAY_ID.store(u64::from(display.id()), Ordering::Release);
    MAIN_DISPLAY_KNOWN.store(true, Ordering::Release);
    MAIN_DISPLAY_KEY.with(|cached| cached.set(Some(key)));
}

/// Resolve a native popup to Echo's current monitor.
///
/// A saved position is retained only while the window center is still on the
/// same display as the main workspace. This both honors useful geometry and
/// repairs old primary-monitor fallbacks or stale coordinates after a monitor
/// layout change. New/off-monitor windows are centered in the display's usable
/// area rather than the system-wide primary display.
pub fn popup_window_placement(
    saved_position: Option<(f32, f32)>,
    window_size: Size<Pixels>,
    cx: &App,
) -> (Bounds<Pixels>, Option<DisplayId>) {
    let remembered_display = MAIN_DISPLAY_KNOWN
        .load(Ordering::Acquire)
        .then(|| DisplayId::new(MAIN_DISPLAY_ID.load(Ordering::Acquire)))
        .and_then(|display_id| cx.find_display(display_id));
    let display = remembered_display.or_else(|| cx.primary_display());
    let display_id = display.as_ref().map(|display| display.id());
    let saved_bounds = saved_position.map(|(x, y)| Bounds::new(point(px(x), px(y)), window_size));

    let bounds = match (saved_bounds, display) {
        (saved, Some(display)) => {
            let visible = display.visible_bounds();
            let origin = saved
                .filter(|saved| visible.contains(&saved.center()))
                .map(|saved| saved.origin);
            fit_into_visible(origin, window_size, visible)
        }
        (Some(saved), None) => saved,
        (None, None) => Bounds::new(point(px(0.0), px(0.0)), window_size),
    };

    (bounds, display_id)
}

/// Resolve the initial main-window restore bounds against the displays that
/// exist now. A saved rectangle remains on the display containing its center;
/// a stale or disconnected-monitor origin falls back to the current primary
/// display. In either case, fit the requested rectangle into the display's
/// usable area so a taskbar or changed monitor topology cannot hide it.
///
/// This is intentionally startup-only. During normal use GPUI owns placement,
/// and the windowed rectangle is persisted from the live `window_bounds()`.
pub fn main_window_restore_bounds(
    saved_position: Option<(f32, f32)>,
    window_size: Size<Pixels>,
    cx: &App,
) -> Bounds<Pixels> {
    let saved_bounds = saved_position.map(|(x, y)| Bounds::new(point(px(x), px(y)), window_size));
    let saved_display = saved_bounds.as_ref().and_then(|saved| {
        cx.displays()
            .into_iter()
            .find(|display| display.bounds().contains(&saved.center()))
    });
    let display = saved_display.or_else(|| cx.primary_display());

    let Some(display) = display else {
        return saved_bounds.unwrap_or_else(|| Bounds::centered(None, window_size, cx));
    };

    let visible = display.visible_bounds();
    let origin = saved_bounds
        .filter(|saved| display.bounds().contains(&saved.center()))
        .map(|saved| saved.origin);
    fit_into_visible(origin, window_size, visible)
}

/// Shrink `window_size` to fit `visible`, place it at `origin` (or centered),
/// and clamp it inside `visible`.
fn fit_into_visible(
    origin: Option<Point<Pixels>>,
    window_size: Size<Pixels>,
    visible: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let fitted_size = Size {
        width: px(f32::from(window_size.width).min(f32::from(visible.size.width))),
        height: px(f32::from(window_size.height).min(f32::from(visible.size.height))),
    };
    let requested = origin
        .map(|origin| Bounds::new(origin, fitted_size))
        .unwrap_or_else(|| Bounds::centered_at(visible.center(), fitted_size));
    let min_x = f32::from(visible.origin.x);
    let min_y = f32::from(visible.origin.y);
    let max_x = min_x + f32::from(visible.size.width) - f32::from(fitted_size.width);
    let max_y = min_y + f32::from(visible.size.height) - f32::from(fitted_size.height);

    Bounds::new(
        point(
            px(f32::from(requested.origin.x).clamp(min_x, max_x)),
            px(f32::from(requested.origin.y).clamp(min_y, max_y)),
        ),
        fitted_size,
    )
}

/// Handle a titlebar click. Returns true if this was a double-click (maximize/restore should happen).
/// If it returns false, caller should set up to start a drag on mouse move.
pub fn handle_titlebar_click() -> bool {
    let now = crate::utils::unix_now_ms();
    let last = LAST_CLICK_TIME.swap(now, Ordering::SeqCst);

    if last > 0 && now - last < 400 {
        LAST_CLICK_TIME.store(0, Ordering::SeqCst);
        DRAG_PENDING.store(false, Ordering::SeqCst);
        true
    } else {
        DRAG_PENDING.store(true, Ordering::SeqCst);
        false
    }
}

/// Arm a custom titlebar drag without applying main-window double-click
/// maximize behavior. This is used by compact borderless utility windows.
pub fn arm_window_drag() {
    DRAG_PENDING.store(true, Ordering::SeqCst);
}

/// Check if a drag is pending and should be started (call this on mouse move)
pub fn should_start_drag() -> bool {
    DRAG_PENDING.swap(false, Ordering::SeqCst)
}

/// Cancel any pending drag (call on mouse up without movement)
pub fn cancel_pending_drag() {
    DRAG_PENDING.store(false, Ordering::SeqCst);
}

#[cfg(target_os = "windows")]
static MAIN_WINDOW_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "windows")]
fn remember_main_window_handle(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Win32(win32) = handle.as_raw()
    {
        MAIN_WINDOW_HWND.store(win32.hwnd.get() as usize, Ordering::Relaxed);
    }
}

#[cfg(not(target_os = "windows"))]
fn remember_main_window_handle(_window: &Window) {}

/// GPUI ids of the Echo windows that are active right now. Every native
/// window reports its activation changes here (see `NativeWindowRoot`).
static ACTIVE_WINDOWS: Mutex<Vec<WindowId>> = Mutex::new(Vec::new());

/// Record whether one Echo window is active; a closed window reports `false`.
pub fn record_window_activation(window_id: WindowId, active: bool) {
    let mut active_windows = ACTIVE_WINDOWS.lock_or_recover();
    active_windows.retain(|id| *id != window_id);
    if active {
        active_windows.push(window_id);
    }
}

/// Whether any Echo window is the foreground window, or `None` when the
/// platform cannot tell.
#[cfg(target_os = "windows")]
pub fn echo_window_is_foreground() -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    let mut process_id = 0u32;
    // SAFETY: GetForegroundWindow has no preconditions; the handle is null-checked before use.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Some(false);
        }
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    Some(process_id == std::process::id())
}

/// Wayland has no foreground-window query, so this answers from GPUI's own
/// activation reports for Echo's windows.
#[cfg(not(target_os = "windows"))]
pub fn echo_window_is_foreground() -> Option<bool> {
    Some(!ACTIVE_WINDOWS.lock_or_recover().is_empty())
}

/// Flash the main window's taskbar button until Echo is focused again.
#[cfg(target_os = "windows")]
pub fn flash_main_window_taskbar() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx,
    };

    let raw = MAIN_WINDOW_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: HWND(raw as *mut core::ffi::c_void),
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: `info` is a fully initialized FLASHWINFO with cbSize set.
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

/// KWin flags the main window in the task manager until it is activated.
#[cfg(target_os = "linux")]
pub fn flash_main_window_taskbar() {
    crate::utils::kwin::demand_main_window_attention();
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn flash_main_window_taskbar() {}

/// Start moving the window (platform-specific implementation)
/// On Windows, this posts WM_NCLBUTTONDOWN with HTCAPTION to simulate titlebar drag
/// On other platforms, this is currently a no-op (GPUI's start_window_move handles Linux)
#[cfg(target_os = "windows")]
pub fn start_window_drag() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, HTCAPTION, PostMessageW, WM_NCLBUTTONDOWN,
    };

    // SAFETY: these calls take no pointers; posting to a stale foreground HWND fails harmlessly.
    unsafe {
        let _ = ReleaseCapture();

        let hwnd = GetForegroundWindow();

        let wparam = WPARAM(HTCAPTION as usize);
        let _ = PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, wparam, LPARAM(0));
    }
}

#[cfg(not(target_os = "windows"))]
pub fn start_window_drag() {}

/// Restore a maximized window to its normal state
/// On Windows, this uses ShowWindowAsync (same as GPUI's zoom_window) with SW_RESTORE
#[cfg(target_os = "windows")]
pub fn restore_window(_window: &Window) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, SW_RESTORE, ShowWindowAsync,
    };

    // SAFETY: ShowWindowAsync accepts any HWND, including a stale or null one.
    unsafe {
        let hwnd = GetForegroundWindow();
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
    }
}

/// GPUI's Linux `zoom_window` toggles the compositor's maximized state.
#[cfg(not(target_os = "windows"))]
pub fn restore_window(window: &Window) {
    if window.is_maximized() {
        window.zoom_window();
    }
}

/// Toggle window maximize state
#[cfg(target_os = "windows")]
pub fn toggle_maximize(_window: &Window) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetForegroundWindow, GetWindowLongW, SW_MAXIMIZE, SW_RESTORE, ShowWindowAsync,
        WS_MAXIMIZE,
    };

    // SAFETY: these calls take no pointers and accept any HWND, including a stale or null one.
    unsafe {
        let hwnd = GetForegroundWindow();
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;

        if (style & WS_MAXIMIZE.0) != 0 {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindowAsync(hwnd, SW_MAXIMIZE);
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn toggle_maximize(window: &Window) {
    window.zoom_window();
}

/// Persist window position and size to settings when they change.
///
/// Compares the current window bounds against the last persisted values and calls
/// `on_position_changed(x, y)` / `on_size_changed(width, height)` only when the
/// values have drifted by at least 1 pixel. Updates the cached values in place.
///
/// # Usage
///
/// ```ignore
/// persist_window_bounds(
///     window,
///     &mut self.last_persisted_position,
///     &mut self.last_persisted_size,
///     |x, y| settings::set_my_window_position(x, y),
///     |w, h| settings::set_my_window_size(w, h),
/// );
/// ```
pub fn persist_window_bounds(
    window: &gpui::Window,
    last_position: &mut Option<(f32, f32)>,
    last_size: &mut (f32, f32),
    on_position_changed: impl FnOnce(f32, f32),
    on_size_changed: impl FnOnce(f32, f32),
) {
    let bounds = window.window_bounds().get_bounds();
    let window_x: f32 = bounds.origin.x.into();
    let window_y: f32 = bounds.origin.y.into();
    let window_width: f32 = bounds.size.width.into();
    let window_height: f32 = bounds.size.height.into();

    let position_changed = last_position
        .map(|(x, y)| (x - window_x).abs() >= 1.0 || (y - window_y).abs() >= 1.0)
        .unwrap_or(true);
    if position_changed {
        on_position_changed(window_x, window_y);
        *last_position = Some((window_x, window_y));
    }

    let size_changed =
        (last_size.0 - window_width).abs() >= 1.0 || (last_size.1 - window_height).abs() >= 1.0;
    if size_changed {
        on_size_changed(window_width, window_height);
        *last_size = (window_width, window_height);
    }
}
