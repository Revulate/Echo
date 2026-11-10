//! Login modal component for device code authentication
//!
//! Shows the Twitch device code flow UI with verification URI and user code.

use crate::backend;
use crate::backend::auth::{LoginAttempt, LoginAttemptState};
use crate::components::overlays::traits::OverlayPointerIsolation;
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels};
use crate::utils;
use gpui::*;

/// Login modal state
#[derive(Clone, Default)]
pub struct LoginModalState {
    pub is_open: bool,
    pub copied_feedback: bool,
    attempt: Option<LoginAttempt>,
}

impl LoginModalState {
    pub fn open(&mut self) {
        self.is_open = true;
        self.copied_feedback = false;
        self.attempt = Some(backend::bridge::start_login_attempt());
    }

    pub fn close(&mut self) {
        if let Some(attempt) = self.attempt.take() {
            attempt.cancel();
        }
        self.is_open = false;
        self.copied_feedback = false;
    }
}

/// Render the login modal overlay for `entity`, or an empty element while it
/// is closed.
pub fn render_login_modal<T: 'static>(
    entity: Entity<LoginModalState>,
    viewport: Size<Pixels>,
    cx: &mut Context<T>,
) -> AnyElement {
    let state = entity.read(cx);
    if !state.is_open {
        return div().into_any_element();
    }

    crate::components::overlays::clear_global_tooltip();

    let attempt_state = state
        .attempt
        .as_ref()
        .map(LoginAttempt::state)
        .unwrap_or(LoginAttemptState::Cancelled);
    let copied_feedback = state.copied_feedback;
    let entity_for_cancel = entity.clone();
    let entity_for_close = entity.clone();
    let entity_for_copy = entity.clone();
    let modal_fit = crate::utils::fit_overlay_to_viewport(viewport, 480.0, 400.0, 12.0);

    let content: AnyElement = match attempt_state {
        LoginAttemptState::Loading => render_loading_state(entity.clone(), cx).into_any_element(),
        LoginAttemptState::DeviceCode {
            verification_uri,
            user_code,
        } => render_device_code_state(
            verification_uri,
            user_code,
            copied_feedback,
            entity_for_copy,
            cx,
        )
        .into_any_element(),
        LoginAttemptState::Succeeded { display_name } => {
            render_success_state(&display_name, entity_for_close, cx).into_any_element()
        }
        LoginAttemptState::Error(error) => {
            render_error_state(error, entity.clone(), cx).into_any_element()
        }
        LoginAttemptState::Cancelled => render_error_state(
            "This sign-in was cancelled. Try again to start a new sign-in.".to_string(),
            entity.clone(),
            cx,
        )
        .into_any_element(),
        LoginAttemptState::Superseded => render_error_state(
            "Another sign-in was started. Try again to sign in from this window.".to_string(),
            entity.clone(),
            cx,
        )
        .into_any_element(),
    };

    div()
        .id("login-modal-overlay")
        .absolute()
        .inset_0()
        .bg(colors::backdrop())
        .flex()
        .items_center()
        .justify_center()
        .block_overlay_pointer_passthrough()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_this, _event, _window, cx| {
                entity_for_cancel.update(cx, |state, _| {
                    state.close();
                });
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .id("login-modal")
                .w(px(modal_fit.width))
                .max_h(px(modal_fit.height))
                .bg(colors::bg_surface())
                .rounded(Radii::xxl())
                .border_1()
                .border_color(colors::border())
                .shadow_lg()
                .overflow_hidden()
                .overflow_y_scroll()
                .block_overlay_pointer_passthrough()
                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                    cx.stop_propagation();
                })
                .child(content),
        )
        .into_any_element()
}

fn render_loading_state<T: 'static>(
    entity: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .p_8()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child("Connecting to Twitch…"),
        )
        .child(render_spinner())
        .child(render_cancel_button(entity, cx))
}

fn render_device_code_state<T: 'static>(
    verification_uri: String,
    user_code: String,
    copied_feedback: bool,
    entity: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let user_code_for_copy = user_code.clone();
    let verification_uri_for_open = verification_uri.clone();
    let entity_for_copy = entity.clone();

    div()
        .w_full()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(colors::text_primary())
                        .child("Log in with Twitch"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_muted())
                        .child("Enter the code below on the Twitch website"),
                ),
        )
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child("Visit this URL:"),
                )
                .child(
                    div()
                        .id("verification-uri")
                        .px_4()
                        .py_2()
                        .rounded(Radii::md())
                        .bg(colors::bg_elevated())
                        .cursor_pointer()
                        .hover(|s| {
                            let mut c = colors::accent();
                            c.a = 0.15;
                            s.bg(c)
                        })
                        .on_activate(move |_event, _window, _cx| {
                            utils::open_url(&verification_uri_for_open);
                        })
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::accent_text())
                                .child(verification_uri.clone()),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_secondary())
                        .child("Enter this code:"),
                )
                .child(
                    div()
                        .px_6()
                        .py_4()
                        .rounded(Radii::lg())
                        .bg(colors::bg_void())
                        .border_1()
                        .border_color(colors::border())
                        .child(
                            div()
                                .text_size(px(FontSize::DISPLAY_SM))
                                .font_family("monospace")
                                .font_weight(FontWeight::BOLD)
                                .text_color(colors::accent_text())
                                .child(format_user_code(&user_code)),
                        ),
                )
                .child(
                    div()
                        .id("copy-code-btn")
                        .px_4()
                        .py_2()
                        .rounded(Radii::md())
                        .bg(if copied_feedback {
                            colors::success()
                        } else {
                            colors::accent()
                        })
                        .cursor_pointer()
                        .hover(|s| {
                            let mut c = if copied_feedback {
                                colors::success()
                            } else {
                                colors::accent()
                            };
                            c.a = 0.8;
                            s.bg(c)
                        })
                        .on_activate(cx.listener(move |_this, _event, _window, cx| {
                            utils::copy_text(&user_code_for_copy);
                            entity_for_copy.update(cx, |state, _| {
                                state.copied_feedback = true;
                            });
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_on(if copied_feedback {
                                    colors::success()
                                } else {
                                    colors::accent()
                                }))
                                .child(if copied_feedback {
                                    "Copied!"
                                } else {
                                    "Copy Code"
                                }),
                        ),
                ),
        )
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .child(render_spinner_small())
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Waiting for authorization…"),
                ),
        )
        .child(render_cancel_button(entity.clone(), cx))
}

fn render_error_state<T: 'static>(
    error: String,
    entity: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let entity_for_retry = entity.clone();

    div()
        .w_full()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::error())
                .child("Couldn't log in"),
        )
        .child(
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .text_align(gpui::TextAlign::Center)
                .child(error),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(
                    div()
                        .id("retry-btn")
                        .px_4()
                        .py_2()
                        .rounded(Radii::md())
                        .bg(colors::accent())
                        .cursor_pointer()
                        .hover(|s| {
                            let mut c = colors::accent();
                            c.a = 0.8;
                            s.bg(c)
                        })
                        .on_activate(cx.listener(move |_this, _event, _window, cx| {
                            entity_for_retry.update(cx, |state, _| {
                                state.open();
                            });
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_on_accent_contrast())
                                .child("Try Again"),
                        ),
                )
                .child(render_cancel_button(entity, cx)),
        )
}

fn render_success_state<T: 'static>(
    display_name: &str,
    entity: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .p_8()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(
            div()
                .w(px(64.0))
                .h(px(64.0))
                .rounded(Radii::full())
                .bg(colors::success())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(FontSize::DISPLAY))
                        .text_color(colors::text_on(colors::success()))
                        .child("✓"),
                ),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors::text_primary())
                .child("Welcome!"),
        )
        .child(
            div()
                .text_base()
                .text_color(colors::text_secondary())
                .child(format!("Logged in as {}", display_name)),
        )
        .child(
            div()
                .id("close-btn")
                .mt_2()
                .px_6()
                .py_2()
                .rounded(Radii::md())
                .bg(colors::accent())
                .cursor_pointer()
                .hover(|s| {
                    let mut c = colors::accent();
                    c.a = 0.8;
                    s.bg(c)
                })
                .on_activate(cx.listener(move |_this, _event, _window, cx| {
                    entity.update(cx, |state, _| {
                        state.close();
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_on_accent_contrast())
                        .child("Done"),
                ),
        )
}

fn render_cancel_button<T: 'static>(
    entity: Entity<LoginModalState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .id("cancel-btn")
        .px_4()
        .py_2()
        .rounded(Radii::md())
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .cursor_pointer()
        .hover(|s| s.bg(colors::bg_void()))
        .on_activate(cx.listener(move |_this, _event, _window, cx| {
            entity.update(cx, |state, _| {
                state.close();
            });
            cx.notify();
        }))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_secondary())
                .child("Cancel"),
        )
}

fn render_spinner() -> impl IntoElement {
    div()
        .w(px(32.0))
        .h(px(32.0))
        .rounded(Radii::full())
        .bg(colors::accent())
}

fn render_spinner_small() -> impl IntoElement {
    div()
        .w(px(12.0))
        .h(px(12.0))
        .rounded(Radii::full())
        .bg(colors::accent())
}

/// Twitch user codes are case-insensitive; show them upper-case.
fn format_user_code(code: &str) -> String {
    code.to_uppercase()
}
