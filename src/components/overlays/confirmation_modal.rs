//! Confirmation modal component
//!
//! Reusable confirmation dialog for destructive or important actions:
//! - Ban user
//! - Clear chat history
//! - Delete saved settings/tabs
//!
//! Features:
//! - Destructive styling (red confirm button) for dangerous actions
//! - Cancel button focused by default for destructive actions
//! - Escape to cancel, Enter to confirm
//! - Action callbacks

use super::traits::OverlayPointerIsolation;
use crate::components::ui::{FocusNext, FocusPrev, PressToClick};
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels, Spacing},
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

pub use echo_core::overlay_state::ConfirmationState;

/// Confirmation modal component
pub struct ConfirmationModal {
    pub state: ConfirmationState,
    pub on_confirm: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_alternate: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub on_tertiary: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub focus_handle: FocusHandle,
    pub uploader_owner: Option<u64>,
}

impl ConfirmationModal {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            state: ConfirmationState::default(),
            focus_handle: cx.focus_handle(),
            on_confirm: None,
            on_cancel: None,
            on_alternate: None,
            on_tertiary: None,
            uploader_owner: None,
        }
    }

    pub fn supersede_pending(&mut self) {
        if self.state.is_open
            && let Some(cancel) = self.on_cancel.take()
        {
            cancel();
        }
        self.state.close();
        self.on_confirm = None;
        self.on_cancel = None;
        self.on_alternate = None;
        self.on_tertiary = None;
        self.uploader_owner = None;
    }

    /// Open a two-button confirmation with a "Cancel" button and only a confirm callback.
    pub fn open_simple(
        &mut self,
        title: String,
        message: String,
        confirm_text: String,
        is_destructive: bool,
        on_confirm: Arc<dyn Fn() + Send + Sync + 'static>,
        cx: &mut Context<Self>,
    ) {
        self.supersede_pending();
        self.state = ConfirmationState {
            is_open: true,
            title,
            message,
            confirm_text,
            cancel_text: "Cancel".to_string(),
            alternate_text: None,
            tertiary_text: None,
            is_destructive,
            confirm_focused: !is_destructive,
        };
        self.on_confirm = Some(on_confirm);
        self.on_cancel = None;
        self.on_alternate = None;
        self.on_tertiary = None;
        cx.notify();
        // The modal draws in the main window; bring that window forward so a
        // confirmation asked for from a popout (Settings) is not hidden
        // behind it.
        cx.defer(|cx| {
            let main_window = cx
                .windows()
                .into_iter()
                .find_map(|window| window.downcast::<crate::components::ui::Root>());
            if let Some(main_window) = main_window {
                let _ = main_window.update(cx, |_, window, _| window.activate_window());
            }
        });
    }

    /// Execute confirm action
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(callback) = &self.on_confirm {
            callback();
        }
        self.state.close();
        cx.notify();
    }

    /// Execute cancel action
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(callback) = &self.on_cancel {
            callback();
        }
        self.state.close();
        cx.notify();
    }

    /// Execute the optional secondary action.
    pub fn alternate(&mut self, cx: &mut Context<Self>) {
        if let Some(callback) = &self.on_alternate {
            callback();
        }
        self.state.close();
        cx.notify();
    }

    /// Execute the optional third action.
    pub fn tertiary(&mut self, cx: &mut Context<Self>) {
        if let Some(callback) = &self.on_tertiary {
            callback();
        }
        self.state.close();
        cx.notify();
    }

    /// Toggle button focus
    pub fn toggle_focus(&mut self, cx: &mut Context<Self>) {
        self.state.toggle_focus();
        cx.notify();
    }

    /// Execute focused action (Enter key handler)
    pub fn execute_focused(&mut self, cx: &mut Context<Self>) {
        if self.state.confirm_focused {
            self.confirm(cx);
        } else {
            self.cancel(cx);
        }
    }
}

impl Render for ConfirmationModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        super::clear_global_tooltip();

        let is_destructive = self.state.is_destructive;
        let confirm_focused = self.state.confirm_focused;
        let cancel_focused = !confirm_focused;
        let title = self.state.title.clone();
        let message = self.state.message.clone();
        let confirm_text = self.state.confirm_text.clone();
        let cancel_text = self.state.cancel_text.clone();
        let alternate_text = self.state.alternate_text.clone();
        let tertiary_text = self.state.tertiary_text.clone();
        let preferred_width = if alternate_text.is_some() || tertiary_text.is_some() {
            560.0
        } else {
            400.0
        };
        let viewport = window.viewport_size();
        let fit = crate::utils::fit_overlay_to_viewport(
            viewport,
            preferred_width,
            f32::from(viewport.height),
            12.0,
        );

        div()
            .id("confirmation-backdrop")
            .absolute()
            .inset_0()
            .bg(colors::backdrop())
            .flex()
            .items_center()
            .justify_center()
            .block_overlay_pointer_passthrough()
            .when(!is_destructive, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _window, cx| {
                        this.cancel(cx);
                        cx.stop_propagation();
                    }),
                )
            })
            .child(
                div()
                    .id("confirmation-modal")
                    .w(px(fit.width))
                    .max_h(px(fit.height))
                    .bg(colors::bg_elevated())
                    .rounded(Radii::lg())
                    .border_1()
                    .border_color(colors::border())
                    .shadow_lg()
                    .overflow_hidden()
                    .overflow_y_scroll()
                    .block_overlay_pointer_passthrough()
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(|this, _: &FocusNext, _window, cx| {
                        this.toggle_focus(cx);
                        cx.stop_propagation();
                    }))
                    .on_action(cx.listener(|this, _: &FocusPrev, _window, cx| {
                        this.toggle_focus(cx);
                        cx.stop_propagation();
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.cancel(cx),
                            "enter" => {
                                if !event.is_held {
                                    this.execute_focused(cx);
                                }
                            }
                            "left" | "right" | "tab" => this.toggle_focus(cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(
                        div()
                            .px(px(Spacing::LG))
                            .pt(px(Spacing::LG))
                            .pb(px(Spacing::SM))
                            .flex()
                            .items_center()
                            .gap(px(Spacing::SM))
                            .when(is_destructive, |el| {
                                el.child(
                                    div()
                                        .size(px(24.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(Radii::full())
                                        .bg(colors::error().opacity(0.15))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(colors::error())
                                                .child("!"),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .text_base()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if is_destructive {
                                        colors::error()
                                    } else {
                                        colors::text_primary()
                                    })
                                    .whitespace_normal()
                                    .child(title),
                            ),
                    )
                    .child(
                        div().px(px(Spacing::LG)).py(px(Spacing::SM)).child(
                            div()
                                .text_sm()
                                .text_color(colors::text_secondary())
                                .whitespace_normal()
                                .child(message),
                        ),
                    )
                    .child(
                        div()
                            .px(px(Spacing::LG))
                            .py(px(Spacing::MD))
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap(px(Spacing::SM))
                            .child(
                                render_button_styled(&cancel_text, false, cancel_focused)
                                    .id("confirm-cancel-btn")
                                    .on_press_click(cx.listener(|this, _, _window, cx| {
                                        this.cancel(cx);
                                    })),
                            )
                            .when_some(tertiary_text, |el, label| {
                                el.child(
                                    render_button_styled(&label, false, false)
                                        .id("confirm-tertiary-btn")
                                        .on_press_click(cx.listener(|this, _, _window, cx| {
                                            this.tertiary(cx);
                                        })),
                                )
                            })
                            .when_some(alternate_text, |el, label| {
                                el.child(
                                    render_button_styled(&label, false, false)
                                        .id("confirm-alternate-btn")
                                        .on_press_click(cx.listener(|this, _, _window, cx| {
                                            this.alternate(cx);
                                        })),
                                )
                            })
                            .child(
                                render_button_styled(
                                    &confirm_text,
                                    is_destructive,
                                    confirm_focused,
                                )
                                .id("confirm-confirm-btn")
                                .on_press_click(cx.listener(|this, _, _window, cx| {
                                    this.confirm(cx);
                                })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Create a styled button (without click handler - add that in the caller)
fn render_button_styled(label: &str, is_destructive: bool, is_focused: bool) -> Div {
    let (bg, text_color, hover_bg) = if is_destructive {
        (
            colors::error(),
            colors::text_on(colors::error()),
            colors::error().opacity(0.85),
        )
    } else {
        (
            colors::bg_hover(),
            colors::text_primary(),
            colors::pressed_surface(),
        )
    };

    div()
        .px(px(Spacing::MD))
        .py(px(Spacing::SM))
        .min_w(px(80.0))
        .rounded(Radii::md())
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text_color)
        .max_w_full()
        .whitespace_normal()
        .bg(bg)
        .hover(|s| s.bg(hover_bg))
        .when(is_focused, |el| {
            el.border_2().border_color(colors::accent())
        })
        .when(!is_focused, |el| {
            el.border_2().border_color(gpui::transparent_black())
        })
        .flex()
        .items_center()
        .justify_center()
        .child(label.to_string())
}
