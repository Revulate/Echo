//! Shared full-width moderation buttons for the chatter list and the mod log.
//!
//! One look for timeout, ban, role and undo actions; the colour comes from
//! [`ModButtonStyle`]. The user card keeps its own compact row.

use crate::components::ui::ButtonExt;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::*;

/// Style configuration for mod action buttons
#[derive(Clone, Copy)]
pub enum ModButtonStyle {
    /// Warning style (yellow/orange) for timeouts
    Warning,
    /// Error style (red) for bans
    Danger,
    /// Success style (green) for unbans
    Success,
}

impl ModButtonStyle {
    fn base_color(&self) -> Hsla {
        match self {
            Self::Warning => colors::warning(),
            Self::Danger => colors::error(),
            Self::Success => colors::success(),
        }
    }

    fn bg(&self) -> Hsla {
        let mut color = self.base_color();
        color.a = 0.1;
        color
    }

    fn border(&self) -> Hsla {
        let mut color = self.base_color();
        color.a = 0.2;
        color
    }

    fn hover_bg(&self) -> Hsla {
        let mut color = self.base_color();
        color.a = 0.2;
        color
    }
}

/// Render a moderation action button (timeout or ban)
///
/// # Arguments
/// * `label` - Button text (e.g., "1m", "10m", "Ban")
/// * `style` - Warning for timeouts, Danger for bans
/// * `on_click` - Callback for a completed click; the label doubles as the
///   element id, so each button needs a parent with a stable, target-specific id
///
/// # Example
/// ```ignore
/// render_mod_button(
///     "10m",
///     ModButtonStyle::Warning,
///     cx.listener(move |this, _, _window, cx| {
///         backend::timeout_user(&channel, &username, 600, None);
///         this.state.close();
///         cx.notify();
///     }),
/// )
/// ```
pub fn render_mod_button<F>(
    label: &str,
    style: ModButtonStyle,
    on_click: F,
) -> impl IntoElement + use<F>
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let text_color = style.base_color();

    div()
        .id(SharedString::from(label.to_string()))
        .flex_1()
        .py_1()
        .px_1p5()
        .rounded(Radii::sm())
        .bg(style.bg())
        .border_1()
        .border_color(style.border())
        .cursor_pointer()
        .hover(move |s| s.bg(style.hover_bg()))
        .on_activate(on_click)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(text_color)
                .child(label.to_string()),
        )
}
