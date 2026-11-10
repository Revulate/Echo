//! Device code display component for OAuth device flow
//!
//! Shows one browser action and the user code for the Twitch OAuth device flow.

use crate::backend::{self, AuthState};
use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels};
use gpui::*;

pub fn render_device_code_inline(auth_state: &AuthState) -> impl IntoElement + use<> {
    div()
        .w_full()
        .p_4()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            if let (Some(uri), Some(code)) = (&auth_state.verification_uri, &auth_state.user_code) {
                render_device_code_content(uri, code)
            } else if let Some(error) = &auth_state.error {
                render_error_content(error)
            } else {
                render_loading_content()
            },
        )
}

fn render_device_code_content(uri: &str, code: &str) -> AnyElement {
    let uri_for_click = uri.to_string();
    let code_for_copy = code.to_string();
    let code_display = code.to_uppercase();

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .text_align(gpui::TextAlign::Center)
                .child("Open Twitch in your browser, then enter this code:"),
        )
        .child(
            div()
                .id("device-code-display")
                .px_4()
                .py_2()
                .rounded(Radii::md())
                .bg(colors::bg_void())
                .cursor_pointer()
                .on_activate(move |_event, _window, _cx| {
                    crate::utils::copy_text(&code_for_copy);
                })
                .child(
                    div()
                        .text_size(px(FontSize::XXL))
                        .font_family("monospace")
                        .font_weight(FontWeight::BOLD)
                        .text_color(colors::text_primary())
                        .child(code_display),
                ),
        )
        .child(
            div()
                .id("open-link-btn")
                .px_3()
                .py_1()
                .rounded(Radii::sm())
                .bg(colors::accent())
                .cursor_pointer()
                .hover(|s| {
                    let mut c = colors::accent();
                    c.a = 0.8;
                    s.bg(c)
                })
                .on_activate(move |_event, _window, _cx| {
                    crate::utils::open_url(&uri_for_click);
                })
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_on_accent_contrast())
                        .child("Open link"),
                ),
        )
        .child(
            div()
                .id("cancel-login-btn")
                .mt_1()
                .px_3()
                .py_1()
                .rounded(Radii::sm())
                .cursor_pointer()
                .hover(|s| s.bg(colors::bg_void()))
                .on_activate(|_event, _window, _cx| {
                    backend::cancel_login();
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Cancel"),
                ),
        )
        .into_any_element()
}

fn render_error_content(error: &str) -> AnyElement {
    let error_msg = error.to_string();

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(colors::error())
                .text_align(gpui::TextAlign::Center)
                .child(error_msg),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("retry-btn")
                        .px_3()
                        .py_1()
                        .rounded(Radii::sm())
                        .bg(colors::accent())
                        .cursor_pointer()
                        .on_activate(|_event, _window, _cx| {
                            backend::start_login();
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_on_accent_contrast())
                                .child("Try Again"),
                        ),
                )
                .child(
                    div()
                        .id("dismiss-error-btn")
                        .px_3()
                        .py_1()
                        .rounded(Radii::sm())
                        .cursor_pointer()
                        .hover(|s| s.bg(colors::bg_void()))
                        .on_activate(|_event, _window, _cx| {
                            backend::cancel_login();
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child("Dismiss"),
                        ),
                ),
        )
        .into_any_element()
}

fn render_loading_content() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child("Connecting…"),
        )
        .child(
            div()
                .w(px(16.0))
                .h(px(16.0))
                .rounded(Radii::full())
                .bg(colors::accent()),
        )
        .child(
            div()
                .id("cancel-connecting-btn")
                .mt_1()
                .px_3()
                .py_1()
                .rounded(Radii::sm())
                .cursor_pointer()
                .hover(|s| s.bg(colors::bg_void()))
                .on_activate(|_event, _window, _cx| {
                    backend::cancel_login();
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child("Cancel"),
                ),
        )
        .into_any_element()
}
