//! Standalone debug popup window.

use crate::components::settings::render_debug_section_live;
use crate::components::ui::{Retarget, SingletonWindow};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::time::Duration;

const DEBUG_POPUP_ACTIVE_REFRESH_MS: u64 = 400;
/// Cadence used while the popup window is not the active window.
const DEBUG_POPUP_INACTIVE_REFRESH_MS: u64 = 2_000;

/// Pending payload: the context label to show next.
static DEBUG_POPUP: SingletonWindow<String> = SingletonWindow::new();

pub fn toggle_debug_popup(context_label: String, cx: &mut App) {
    if DEBUG_POPUP.is_open() {
        close_debug_popup(cx);
    } else {
        open_debug_popup(context_label, cx);
    }
}

pub fn open_debug_popup(context_label: String, cx: &mut App) {
    let context_label = match DEBUG_POPUP.retarget(context_label, cx) {
        Retarget::Retargeted => return,
        Retarget::NotOpen(context_label) => context_label,
        Retarget::Stale(context_label) => {
            DEBUG_POPUP.mark_closed();
            context_label
        }
    };

    let (width, height) = crate::backend::settings::get_debug_popup_window_size();
    let (bounds, display_id) = crate::utils::popup_window_placement(
        crate::backend::settings::get_debug_popup_window_position(),
        size(px(width), px(height)),
        cx,
    );
    let title = if context_label.is_empty() {
        "Echo Debug".to_string()
    } else {
        format!("Echo Debug - {}", context_label)
    };

    crate::components::ui::open_native_window(
        cx,
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: false,
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            kind: WindowKind::Floating,
            is_movable: true,
            display_id,
            window_background: WindowBackgroundAppearance::Opaque,
            app_id: Some(crate::identity::APP_ID.to_string()),
            window_min_size: Some(size(px(520.0), px(520.0))),
            ..Default::default()
        },
        move |window, cx| {
            DEBUG_POPUP.set_handle(window.window_handle());
            DEBUG_POPUP.mark_open();
            cx.new(|cx| DebugPopup::new(context_label.clone(), window, cx))
        },
    )
    .ok();
}

pub fn close_debug_popup(cx: &mut App) {
    DEBUG_POPUP.close(cx);
}

pub struct DebugPopup {
    context_label: String,
    /// Whether the native window title still lags the context label
    title_dirty: bool,
    last_persisted_position: Option<(f32, f32)>,
    last_persisted_size: (f32, f32),
    refresh_waker_running: bool,
    window_active: bool,
    _activation_subscription: Subscription,
}

impl DebugPopup {
    fn new(context_label: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation_subscription = cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            if this.window_active == active {
                return;
            }

            this.window_active = active;
            if active {
                cx.notify();
            }
        });

        Self {
            context_label,
            title_dirty: true,
            last_persisted_position: crate::backend::settings::get_debug_popup_window_position(),
            last_persisted_size: crate::backend::settings::get_debug_popup_window_size(),
            refresh_waker_running: false,
            window_active: window.is_window_active(),
            _activation_subscription: activation_subscription,
        }
    }

    fn refresh_interval(&self) -> Duration {
        Duration::from_millis(if self.window_active {
            DEBUG_POPUP_ACTIVE_REFRESH_MS
        } else {
            DEBUG_POPUP_INACTIVE_REFRESH_MS
        })
    }

    fn start_refresh_waker(&mut self, cx: &mut Context<Self>) {
        if self.refresh_waker_running {
            return;
        }

        self.refresh_waker_running = true;
        let mut interval = self.refresh_interval();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;

                let next_interval = this
                    .update(cx, |popup, cx| {
                        if !DEBUG_POPUP.is_open() {
                            popup.refresh_waker_running = false;
                            return None;
                        }

                        cx.notify();
                        Some(popup.refresh_interval())
                    })
                    .ok()
                    .flatten();

                match next_interval {
                    Some(next) => interval = next,
                    None => break,
                }
            }
        })
        .detach();
    }
}

impl Drop for DebugPopup {
    fn drop(&mut self) {
        DEBUG_POPUP.mark_closed();
    }
}

impl Render for DebugPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        colors::begin_secondary_window_frame();
        if let Some(context) = DEBUG_POPUP.take_pending()
            && context != self.context_label
        {
            self.context_label = context;
            self.title_dirty = true;
        }

        if self.title_dirty {
            let title = if self.context_label.is_empty() {
                "Echo Debug".to_string()
            } else {
                format!("Echo Debug - {}", self.context_label)
            };
            window.set_window_title(&title);
            self.title_dirty = false;
        }

        self.start_refresh_waker(cx);

        let bounds = window.window_bounds().get_bounds();
        let window_x: f32 = bounds.origin.x.into();
        let window_y: f32 = bounds.origin.y.into();
        let window_width: f32 = bounds.size.width.into();
        let window_height: f32 = bounds.size.height.into();

        let position_changed = self
            .last_persisted_position
            .map(|(x, y)| (x - window_x).abs() >= 1.0 || (y - window_y).abs() >= 1.0)
            .unwrap_or(true);
        if position_changed {
            crate::backend::settings::set_debug_popup_window_position(window_x, window_y);
            self.last_persisted_position = Some((window_x, window_y));
        }

        let size_changed = (self.last_persisted_size.0 - window_width).abs() >= 1.0
            || (self.last_persisted_size.1 - window_height).abs() >= 1.0;
        if size_changed {
            crate::backend::settings::set_debug_popup_window_size(window_width, window_height);
            self.last_persisted_size = (window_width, window_height);
        }

        div()
            .id("debug-popup")
            .size_full()
            .bg(colors::bg_elevated())
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|_this, event: &KeyDownEvent, window, _cx| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(colors::border())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child("Debug"),
                    )
                    .when(!self.context_label.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(self.context_label.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .id("debug-popup-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_4()
                    .child(render_debug_section_live(cx)),
            )
    }
}
