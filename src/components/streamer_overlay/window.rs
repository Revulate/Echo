//! Transparent Streamer Overlay Window
//!
//! A borderless, transparent window for streamers to monitor chat while gaming.

use crate::backend::{self, ChatMessage};
use crate::components::ui::{Retarget, SingletonWindow};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use crate::utils::persist_window_bounds;
use gpui::prelude::FluentBuilder;
use gpui::*;
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowLongW, SetWindowPos, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// Payload: the overlay's channel. The window keeps it set after applying it,
/// so it also answers which channel the open overlay shows.
static OVERLAY: SingletonWindow<String> = SingletonWindow::new();

const IDLE_FADE_DURATION: Duration = Duration::from_secs(5);
const IDLE_FADE_TICK: Duration = Duration::from_millis(100);

/// Mark overlay as closed
pub fn mark_overlay_closed() {
    OVERLAY.mark_closed();
}

pub fn is_overlay_open() -> bool {
    OVERLAY.is_open()
}

pub fn toggle_overlay_window(channel: String, cx: &mut App) {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return;
    }

    let shows_channel = OVERLAY.with_pending(|current| current == Some(&channel));

    if is_overlay_open() && shows_channel {
        close_overlay_window(cx);
    } else {
        open_overlay_window(channel, cx);
    }
}

pub fn close_overlay_window(cx: &mut App) {
    OVERLAY.close(cx);
}

/// Overlay window configuration
#[derive(Clone, Debug)]
pub struct OverlayConfig {
    /// Channel to display
    pub channel: String,
    /// Maximum messages to show
    pub max_messages: usize,
    /// Idle fade timeout (seconds)
    pub idle_fade_secs: u64,
    /// Minimum opacity when faded
    pub min_opacity: f32,
    /// Whether the overlay should ignore mouse input
    pub click_through: bool,
    /// Whether to render a reduced "ghost" presentation
    pub ghost_mode: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            channel: String::new(),
            max_messages: 15,
            idle_fade_secs: 30,
            min_opacity: 0.1,
            click_through: false,
            ghost_mode: false,
        }
    }
}

/// Open the streamer overlay window
pub fn open_overlay_window(channel: String, cx: &mut App) {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return;
    }

    let channel = match OVERLAY.retarget(channel, cx) {
        Retarget::Retargeted => return,
        Retarget::NotOpen(channel) => channel,
        Retarget::Stale(channel) => {
            mark_overlay_closed();
            channel
        }
    };

    let (width, height) = crate::backend::settings::get_overlay_window_size();
    let (bounds, display_id) = crate::utils::popup_window_placement(
        crate::backend::settings::get_overlay_window_position(),
        size(px(width), px(height)),
        cx,
    );

    crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(format!("Chat - {}", channel).into()),
                appears_transparent: true,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(px(200.0), px(150.0))),
            ..Default::default()
        },
        move |window, cx| {
            OVERLAY.set_handle(gpui::Window::window_handle(window));
            OVERLAY.set_pending(channel.clone());
            OVERLAY.mark_open();
            cx.new(|cx| StreamerOverlay::new(channel.clone(), cx))
        },
    )
    .ok();
}

/// Display-ready copy of a message.
///
/// The source `Arc<str>` fields are retained so an unchanged tail can be
/// detected by pointer identity: deletions and late text resolution replace the
/// content `Arc` in place rather than the message id.
struct OverlayRow {
    id: Arc<str>,
    display_name: Arc<str>,
    content: Arc<str>,
    color: Option<Arc<str>>,
    name_label: SharedString,
    content_label: SharedString,
}

impl OverlayRow {
    fn from_message(msg: &ChatMessage) -> Self {
        Self {
            id: msg.id.clone(),
            display_name: msg.display_name.clone(),
            content: msg.content.clone(),
            color: msg.color.clone(),
            name_label: SharedString::from(format!("{}:", msg.display_name)),
            content_label: SharedString::from(msg.content.to_string()),
        }
    }

    fn matches(&self, msg: &ChatMessage) -> bool {
        Arc::ptr_eq(&self.id, &msg.id)
            && Arc::ptr_eq(&self.content, &msg.content)
            && Arc::ptr_eq(&self.display_name, &msg.display_name)
    }
}

/// Streamer overlay window component
pub struct StreamerOverlay {
    config: OverlayConfig,
    /// Cached display-ready messages
    rows: Vec<OverlayRow>,
    /// Header label for the active channel
    channel_label: SharedString,
    /// Whether the native window title still lags the active channel
    title_dirty: bool,
    /// Settings broadcast receiver, drained to detect config edits.
    /// `None` means the channel was unavailable, so settings are re-read every render.
    settings_events: Option<broadcast::Receiver<crate::backend::settings::SettingsEvent>>,
    /// Keeps the selected-channel publication listener alive for this window.
    _message_update_task: Option<Task<()>>,
    /// Last activity time (for idle fade)
    last_activity: Instant,
    /// Current opacity
    opacity: f32,
    /// Whether decorative overlay motion currently follows the static policy.
    reduced_motion: bool,
    /// Last persisted window position
    last_persisted_position: Option<(f32, f32)>,
    /// Last persisted window size
    last_persisted_size: (f32, f32),
    /// Last click-through state applied to the native window
    last_click_through: Option<bool>,
    /// Replaces the previous bounded idle-fade task when new activity arrives.
    idle_fade_generation: u64,
    /// One-shot wake sequence: sleep until fading starts, repaint through the
    /// five-second transition only when decorative motion is enabled, then stop
    /// at terminal opacity.
    idle_fade_task: Option<Task<()>>,
}

impl Drop for StreamerOverlay {
    fn drop(&mut self) {
        mark_overlay_closed();
    }
}

impl StreamerOverlay {
    pub fn new(channel: String, cx: &mut Context<Self>) -> Self {
        let settings_events = crate::backend::settings::subscribe_settings();
        let mut config = OverlayConfig {
            channel,
            ..Default::default()
        };
        refresh_config_from_settings(&mut config);
        let last_persisted_size = crate::backend::settings::get_overlay_window_size();
        let last_persisted_position = crate::backend::settings::get_overlay_window_position();
        let channel_label = SharedString::from(config.channel.to_string());
        let mut overlay = Self {
            config,
            rows: Vec::new(),
            channel_label,
            title_dirty: true,
            settings_events,
            _message_update_task: None,
            last_activity: Instant::now(),
            opacity: 1.0,
            reduced_motion: Self::reduced_motion_policy(cx),
            last_persisted_position,
            last_persisted_size,
            last_click_through: None,
            idle_fade_generation: 0,
            idle_fade_task: None,
        };
        overlay.listen_for_selected_channel_messages(cx);
        overlay.schedule_idle_fade_waker(cx);
        overlay
    }

    /// Whether overlay settings were edited since the last check.
    ///
    /// Overlay config only moves on explicit user edits, which always broadcast
    /// a settings event, so the global settings object is read on demand rather
    /// than once per render.
    fn take_settings_changed(&mut self) -> bool {
        let Some(events) = self.settings_events.as_mut() else {
            return true;
        };

        let mut changed = false;
        loop {
            match events.try_recv() {
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => changed = true,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => {
                    self.settings_events = None;
                    return true;
                }
            }
        }
        changed
    }

    /// Update messages from backend
    fn update_messages(&mut self) -> bool {
        let mut new_messages =
            backend::get_messages(&self.config.channel, self.config.max_messages as u32);
        new_messages.reverse();
        new_messages.truncate(self.config.max_messages);

        if self.rows.len() == new_messages.len()
            && self
                .rows
                .iter()
                .zip(new_messages.iter())
                .all(|(row, msg)| row.matches(msg))
        {
            return false;
        }

        self.rows = new_messages.iter().map(OverlayRow::from_message).collect();
        true
    }

    /// Receive coalesced backend message publications and explicitly invalidate
    /// this secondary window only when its selected channel's bounded tail changed.
    fn listen_for_selected_channel_messages(&mut self, cx: &mut Context<Self>) {
        let mut updates = crate::backend::observers::subscribe_channel_message_updates();
        self._message_update_task = Some(cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                let _epoch = updates.borrow_and_update().epoch;
                if this
                    .update(cx, |overlay, cx| {
                        if overlay.update_messages() {
                            overlay.note_activity(cx);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn sync_window_title(&mut self, window: &mut Window) {
        if !self.title_dirty {
            return;
        }

        window.set_window_title(&format!("Chat - {}", self.config.channel));
        self.title_dirty = false;
    }

    fn reduced_motion_policy(cx: &App) -> bool {
        cx.reduce_motion() || crate::backend::settings::get_reduce_decorative_motion()
    }

    /// Synchronize the explicit Echo setting with GPUI's app-wide motion policy.
    fn sync_reduced_motion(&mut self, cx: &App) -> bool {
        let reduced_motion = Self::reduced_motion_policy(cx);
        let changed = self.reduced_motion != reduced_motion;
        self.reduced_motion = reduced_motion;
        changed
    }

    /// Update idle fade.
    fn update_opacity(&mut self) -> bool {
        let previous = self.opacity;
        if self.config.idle_fade_secs == 0 {
            self.opacity = 1.0;
            return self.opacity != previous;
        }

        let idle_duration = self.last_activity.elapsed();
        let fade_start = Duration::from_secs(self.config.idle_fade_secs);
        let fade_duration = IDLE_FADE_DURATION;

        if (self.reduced_motion && idle_duration >= fade_start)
            || idle_duration > fade_start + fade_duration
        {
            self.opacity = self.config.min_opacity;
        } else if idle_duration > fade_start {
            let fade_progress =
                (idle_duration - fade_start).as_secs_f32() / fade_duration.as_secs_f32();
            self.opacity = 1.0 - (1.0 - self.config.min_opacity) * fade_progress;
        } else {
            self.opacity = 1.0;
        }

        self.opacity != previous
    }

    fn note_activity(&mut self, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        self.opacity = 1.0;
        self.schedule_idle_fade_waker(cx);
    }

    fn next_idle_fade_wake(&self) -> Option<Duration> {
        if self.config.idle_fade_secs == 0 || self.config.min_opacity >= 1.0 {
            return None;
        }

        let fade_start = Duration::from_secs(self.config.idle_fade_secs);
        let elapsed = self.last_activity.elapsed();
        if elapsed < fade_start {
            return Some(fade_start - elapsed);
        }

        if self.reduced_motion {
            return None;
        }

        (elapsed < fade_start + IDLE_FADE_DURATION).then_some(IDLE_FADE_TICK)
    }

    fn schedule_idle_fade_waker(&mut self, cx: &mut Context<Self>) {
        self.idle_fade_generation = self.idle_fade_generation.wrapping_add(1);
        self.idle_fade_task.take();

        let Some(initial_delay) = self.next_idle_fade_wake() else {
            return;
        };
        let generation = self.idle_fade_generation;
        self.idle_fade_task = Some(cx.spawn(async move |this, cx| {
            let mut delay = initial_delay;
            loop {
                cx.background_executor().timer(delay).await;

                let next_delay = this
                    .update(cx, |overlay, cx| {
                        if overlay.idle_fade_generation != generation {
                            return None;
                        }

                        if overlay.update_opacity() {
                            crate::backend::metrics::record_ui_notify_source(
                                "streamer_overlay.idle_fade_waker",
                            );
                            cx.notify();
                        }
                        overlay.next_idle_fade_wake()
                    })
                    .ok()
                    .flatten();

                let Some(next_delay) = next_delay else {
                    break;
                };
                delay = next_delay;
            }
        }));
    }

    fn persist_window_bounds(&mut self, window: &Window) {
        persist_window_bounds(
            window,
            &mut self.last_persisted_position,
            &mut self.last_persisted_size,
            crate::backend::settings::set_overlay_window_position,
            crate::backend::settings::set_overlay_window_size,
        );
    }

    fn sync_click_through(&mut self, window: &Window) {
        if self.last_click_through == Some(self.config.click_through) {
            return;
        }

        apply_overlay_click_through(window, self.config.click_through);
        self.last_click_through = Some(self.config.click_through);
    }

    fn apply_pending_channel_switch(&mut self) -> bool {
        let next_channel = OVERLAY.take_pending();

        let Some(channel) = next_channel else {
            return false;
        };

        if channel == self.config.channel {
            OVERLAY.set_pending(channel);
            return false;
        }

        self.config.channel = channel.clone();
        self.channel_label = SharedString::from(channel.to_string());
        self.title_dirty = true;
        self.rows.clear();
        OVERLAY.set_pending(channel);
        true
    }
}

impl Render for StreamerOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        colors::begin_secondary_window_frame();
        if self.apply_pending_channel_switch() {
            self.note_activity(cx);
        }
        let settings_changed = self.take_settings_changed();
        if settings_changed {
            refresh_config_from_settings(&mut self.config);
        }
        let reduced_motion_changed = self.sync_reduced_motion(cx);
        if settings_changed || reduced_motion_changed {
            self.schedule_idle_fade_waker(cx);
        }
        self.sync_window_title(window);
        if self.update_messages() {
            self.note_activity(cx);
        }
        self.update_opacity();
        self.persist_window_bounds(window);
        self.sync_click_through(window);

        let background_alpha = if self.config.ghost_mode { 0.18 } else { 0.5 };
        let panel_padding = if self.config.ghost_mode {
            px(Spacing::XS_PLUS)
        } else {
            px(Spacing::SM)
        };
        let message_gap = if self.config.ghost_mode {
            px(0.0)
        } else {
            px(Spacing::XXS)
        };

        div()
            .id("streamer-overlay")
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, background_alpha * self.opacity))
            .rounded(Radii::lg())
            .border_1()
            .border_color(colors::text_primary().opacity(0.12 * self.opacity))
            .p(panel_padding)
            .flex()
            .flex_col()
            .gap(message_gap)
            .overflow_hidden()
            .when(!self.config.ghost_mode, |panel| {
                panel.child(
                    div()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .opacity(self.opacity)
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors::text_primary())
                                .child(self.channel_label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(format!("{} msgs", self.rows.len())),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(message_gap)
                    .overflow_hidden()
                    .opacity(self.opacity)
                    .children(self.rows.iter().map(render_overlay_row)),
            )
    }
}

/// Render a single message in overlay style
fn render_overlay_row(row: &OverlayRow) -> impl IntoElement + use<> {
    let user_color = row
        .color
        .as_deref()
        .and_then(crate::utils::parse_user_color_opt)
        .unwrap_or(colors::accent());

    div()
        .w_full()
        .flex()
        .items_start()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(user_color)
                .flex_shrink_0()
                .child(row.name_label.clone()),
        )
        .child(
            div()
                .text_xs()
                .text_color(colors::text_primary())
                .flex_1()
                .overflow_hidden()
                .child(row.content_label.clone()),
        )
}

fn refresh_config_from_settings(config: &mut OverlayConfig) {
    config.max_messages = crate::backend::settings::get_overlay_max_messages();
    config.idle_fade_secs = crate::backend::settings::get_overlay_idle_fade_secs();
    config.min_opacity = crate::backend::settings::get_overlay_min_opacity();
    config.click_through = crate::backend::settings::get_overlay_click_through();
    config.ghost_mode = crate::backend::settings::get_overlay_ghost_mode();
}

#[cfg(target_os = "windows")]
fn overlay_hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn apply_overlay_click_through(window: &Window, enabled: bool) {
    let Some(hwnd) = overlay_hwnd(window) else {
        return;
    };

    // SAFETY: `hwnd` is this overlay's live window handle; only its extended style bits are read and written.
    unsafe {
        let mut exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        exstyle |= WS_EX_LAYERED.0;
        if enabled {
            exstyle |= WS_EX_TRANSPARENT.0;
        } else {
            exstyle &= !WS_EX_TRANSPARENT.0;
        }

        let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, exstyle as i32);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_overlay_click_through(_window: &Window, _enabled: bool) {}
