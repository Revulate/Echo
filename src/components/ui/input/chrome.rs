//! Shared visual chrome for lightweight inline text inputs.
//!
//! This keeps chat input, placeholder channel input, and tab rename input
//! visually consistent without forcing them onto a single state machine.

use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;

/// Apply standard input chrome used by primary text inputs.
pub fn apply_standard_input_chrome(input: Stateful<Div>, is_focused: bool) -> Stateful<Div> {
    input
        .px(px(Spacing::SM))
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(if is_focused {
            colors::accent()
        } else {
            colors::border()
        })
        .rounded(Radii::md())
}

/// Build inline input text with a blinking cursor.
pub fn inline_text_with_cursor(
    display_text: String,
    is_placeholder: bool,
    is_focused: bool,
    caret_visible: bool,
) -> AnyElement {
    let make_cursor = || {
        div()
            .w(px(1.5))
            .h(px(16.0))
            .bg(colors::accent())
            .opacity(if caret_visible { 1.0 } else { 0.0 })
    };

    div()
        .relative()
        .flex()
        .items_center()
        .when(is_focused && is_placeholder, |el| {
            el.child(div().absolute().left_0().child(make_cursor()))
        })
        .child(
            div()
                .text_sm()
                .text_color(if is_placeholder {
                    colors::text_muted()
                } else {
                    colors::text_primary()
                })
                .child(display_text),
        )
        .when(is_focused && !is_placeholder, |el| el.child(make_cursor()))
        .into_any_element()
}
