//! Toast notification system
//!
//! Overlay component for displaying temporary notifications:
//! - 4 types: Info, Success, Warning, Error
//! - Auto-dismiss (configurable 3-10s)
//! - Hover-to-pause auto-dismiss timer
//! - Manual dismiss button
//! - Action buttons (Retry, View, Custom)
//! - Click handlers for navigation
//! - Stack up to 5 toasts (top-right corner)

use crate::components::overlays::text_tooltip_on_hover;
use crate::components::ui::ButtonExt;
use crate::icons::CLOSE_GLYPH;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels, Spacing},
};
use gpui::prelude::FluentBuilder;
use gpui::*;

pub use echo_core::overlay_state::{Toast, ToastAction, ToastManagerState, ToastType};

const TOAST_WIDTH: f32 = 340.0;
const TOAST_SPACING: f32 = 8.0;
const TOAST_MARGIN: f32 = 16.0;

fn toast_colors(toast_type: &ToastType) -> (Hsla, Hsla, Hsla) {
    match toast_type {
        ToastType::Info => (
            colors::accent().opacity(0.15),
            colors::accent().opacity(0.4),
            colors::accent(),
        ),
        ToastType::Success => (
            colors::success().opacity(0.15),
            colors::success().opacity(0.4),
            colors::success(),
        ),
        ToastType::Warning => (
            colors::warning().opacity(0.15),
            colors::warning().opacity(0.4),
            colors::warning(),
        ),
        ToastType::Error => (
            colors::error().opacity(0.15),
            colors::error().opacity(0.4),
            colors::error(),
        ),
    }
}

/// Toast manager component - renders the toast stack
pub struct ToastManager {
    pub state: ToastManagerState,
}

impl ToastManager {
    pub fn new() -> Self {
        Self {
            state: ToastManagerState::default(),
        }
    }

    /// Add a toast (convenience methods)
    pub fn info(&mut self, message: impl Into<String>) {
        self.state.push(Toast::info(message));
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.state.push(Toast::success(message));
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.state.push(Toast::error(message));
    }

    /// Add a custom toast
    pub fn push(&mut self, toast: Toast) {
        self.state.push(toast);
    }

    /// Check if there are any toasts
    pub fn has_toasts(&self) -> bool {
        self.state.has_toasts()
    }

    /// Remove expired toasts, returns true if any were removed
    pub fn remove_expired(&mut self) -> bool {
        self.state.remove_expired()
    }
}

impl Default for ToastManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for ToastManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.has_toasts() {
            return div().into_any_element();
        }

        let toasts = self.state.toasts.clone();
        let viewport = window.viewport_size();
        let viewport_height = f32::from(viewport.height);
        let fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            TOAST_WIDTH,
            viewport_height,
            TOAST_MARGIN,
        );

        div()
            .id("toast-container")
            .absolute()
            .top(px(fit.margin_y))
            .right(px(fit.margin_x))
            .w(px(fit.width))
            .max_h(px(fit.height))
            .occlude()
            .flex()
            .flex_col()
            .gap(px(TOAST_SPACING))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .children(toasts.into_iter().map(|toast| render_toast(&toast, cx)))
            .into_any_element()
    }
}

/// Render a single toast notification
fn render_toast(toast: &Toast, cx: &mut Context<ToastManager>) -> impl IntoElement + use<> {
    let id = toast.id;
    let (bg_tint, _border_color, icon_color) = toast_colors(&toast.toast_type);
    let icon = toast.toast_type.icon();
    let has_click = toast.on_click.is_some();
    let on_click = toast.on_click.clone();
    let message = toast.message.clone();
    let detail = toast.detail.clone();
    let action = toast.action.clone();
    let has_action = action.has_button();
    let action_label = action.label().map(|s| s.to_string());

    div()
        .id(SharedString::from(format!("toast-{}", id)))
        .w_full()
        .relative()
        .occlude()
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .rounded(Radii::md())
        .shadow_lg()
        .overflow_hidden()
        .when(has_click && !has_action, |el| el.cursor_pointer())
        .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
            if *hovered {
                if !this.state.is_hovered(id) {
                    this.state.set_hovered(id);
                    cx.notify();
                }
            } else if this.state.is_hovered(id) {
                this.state.clear_hovered();
                cx.notify();
            }
        }))
        .on_activate(cx.listener({
            let on_click = on_click.clone();
            move |this, _event: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                if has_action {
                    return;
                }
                if let Some(ref action_fn) = on_click {
                    action_fn();
                }
                this.state.dismiss(id);
                cx.notify();
            }
        }))
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(3.0))
                .bg(icon_color),
        )
        .child(
            div()
                .py_3()
                .pr_3()
                .pl_4()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(Radii::full())
                        .bg(bg_tint)
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::BOLD)
                                .text_color(icon_color)
                                .child(icon),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .whitespace_normal()
                                .child(message),
                        )
                        .when_some(detail, |el, detail_text| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .line_clamp(4)
                                    .child(detail_text),
                            )
                        })
                        .when_some(action_label, |el, label| {
                            el.child(render_action_button(id, label, action.clone(), cx))
                        }),
                )
                .child(render_close_button(id, cx)),
        )
}

/// Render an action button for a toast
fn render_action_button(
    toast_id: u64,
    label: String,
    action: ToastAction,
    cx: &mut Context<ToastManager>,
) -> impl IntoElement + use<> {
    div()
        .id(SharedString::from(format!("toast-action-{}", toast_id)))
        .mt_1()
        .px_2()
        .py(px(Spacing::XS))
        .rounded(Radii::sm())
        .cursor_pointer()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors::accent_text())
        .bg(colors::accent().opacity(0.1))
        .hover(|s| s.bg(colors::accent().opacity(0.2)))
        .on_activate(cx.listener(move |this, _, _window, cx| {
            cx.stop_propagation();
            action.run();
            this.state.dismiss(toast_id);
            cx.notify();
        }))
        .child(label)
}

/// Render the close button for a toast
fn render_close_button(toast_id: u64, cx: &mut Context<ToastManager>) -> impl IntoElement + use<> {
    let source = format!("toast-close-{toast_id}");
    div()
        .id(SharedString::from(source.clone()))
        .role(Role::Button)
        .aria_label("Dismiss notification")
        .on_hover(text_tooltip_on_hover(source, "Dismiss".to_string()))
        .size(px(Spacing::XXL))
        .rounded(Radii::sm())
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(colors::bg_hover()))
        .on_activate(cx.listener(move |this, _, _window, cx| {
            cx.stop_propagation();
            this.state.dismiss(toast_id);
            cx.notify();
        }))
        .child(
            div()
                .text_sm()
                .text_color(colors::text_muted())
                .child(CLOSE_GLYPH),
        )
}
