//! Icon rendering module for Echo GPUI
//!
//! Self-contained icon system using GPUI primitives directly.

use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Sizes, Spacing};
use gpui::*;

/// Shared density scale for menu surfaces (dropdowns, context menus, split
/// menus).
///
/// Scoped deliberately: menus run denser than chat and the Zed-derived
/// overlays, so this lives beside the menu builders rather than shrinking the
/// global GPUI spacing tokens. Every menu row keeps the same fixed height so
/// keyboard/pointer hit-testing and viewport fitting can be computed from
/// [`MenuMetrics::ITEM_HEIGHT`] without measuring.
pub struct MenuMetrics;

impl MenuMetrics {
    /// Fixed interactive row height (also the minimum interaction target).
    pub const ITEM_HEIGHT: f32 = Sizes::BUTTON_SM;
    /// Horizontal inset inside a menu row.
    pub const ITEM_PADDING_X: f32 = Spacing::SM;
    /// Gap between a row's icon, label, and trailing shortcut/chevron.
    pub const ITEM_GAP: f32 = Spacing::XS_PLUS;
    /// Menu item label size.
    pub const ITEM_TEXT: f32 = FontSize::BASE;
    /// Shortcut and other secondary row text — the smallest permitted size.
    pub const SECONDARY_TEXT: f32 = FontSize::XS;
    /// Total height reserved by a divider row (1px rule plus its margins).
    pub const DIVIDER_HEIGHT: f32 = Spacing::HAIRLINE + Spacing::XS_PLUS;
    /// Fixed height of a menu header row.
    pub const HEADER_HEIGHT: f32 = 32.0;
    /// Vertical padding of the menu panel itself.
    pub const PANEL_PADDING: f32 = Spacing::XS_PLUS;
}

/// Close/dismiss glyph shared by every close button, replacing the mixed
/// multiplication-sign, heavy-X, and letter-X spellings.
pub const CLOSE_GLYPH: &str = "\u{2715}";

/// Available icons in Echo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Inbox,
    Settings,
}

impl IconName {
    /// Get the asset path for this icon
    fn path(self) -> &'static str {
        match self {
            Self::Inbox => "icons/inbox.svg",
            Self::Settings => "icons/settings.svg",
        }
    }
}

/// An SVG icon with configurable size and color
#[derive(IntoElement)]
pub struct Icon {
    path: SharedString,
    size: Option<Pixels>,
    color: Option<Hsla>,
}

impl Icon {
    /// Create a new icon from an IconName
    pub fn new(name: IconName) -> Self {
        Self {
            path: name.path().into(),
            size: None,
            color: None,
        }
    }

    /// Set the icon size in pixels
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Set the icon color
    pub fn text_color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let size = self
            .size
            .unwrap_or_else(|| window.text_style().font_size.to_pixels(window.rem_size()));

        svg()
            .path(self.path)
            .size(size)
            .flex_shrink_0()
            .text_color(color)
    }
}

/// Render an icon with the given size and color
pub fn icon(name: IconName, size: Pixels, color: Hsla) -> impl IntoElement {
    Icon::new(name).size(size).text_color(color)
}

/// Icon button styling helper - creates a styled container for an icon button
pub fn icon_button_style() -> Div {
    div()
        .size(px(Sizes::BUTTON_SM))
        .flex()
        .items_center()
        .justify_center()
        .rounded(Radii::md())
        .cursor_pointer()
}

/// Window control button styling - for minimize/maximize/close
pub fn window_control_button_style() -> Div {
    div()
        .w(px(46.0))
        .h(px(crate::backend::settings::titlebar_height_px()))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
}

/// Dropdown header styling - common header for dropdown panels
pub fn dropdown_header_style() -> Div {
    use crate::theme::colors;
    div()
        .w_full()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(colors::border())
}

/// Modal backdrop styling - fullscreen overlay for clicking outside to close
pub fn modal_backdrop_style() -> Div {
    div().absolute().inset_0().occlude()
}
