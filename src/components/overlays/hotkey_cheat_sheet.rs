//! Hotkey Cheat Sheet Overlay
//!
//! Quick reference modal showing all keyboard shortcuts.
//! Organized by category: Navigation, Chat, Moderation, Other.
//!
//! Features:
//! - Shows current key bindings (respects user customizations)
//! - Categorized layout for easy scanning
//! - Press Ctrl+/ (Keyboard Shortcuts hotkey) to toggle
//! - Escape to dismiss

use super::traits::OverlayPointerIsolation;
use crate::backend::hotkeys::{
    self, CHAT_ACTIONS, HotkeyAction, MODERATION_ACTIONS, NAVIGATION_ACTIONS, OTHER_ACTIONS,
    SEARCH_ACTIONS, custom_hotkeys,
};
use crate::components::overlays::text_tooltip_on_hover;
use crate::components::ui::ButtonExt;
use crate::icons::CLOSE_GLYPH;
use crate::theme::tokens::FontSize;
use crate::theme::{
    colors,
    tokens::{Radii, RadiiPixels, Sizes, Spacing},
};
use gpui::*;

pub use echo_core::overlay_state::HotkeyCheatSheetState;

/// Hotkey category for grouping
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyCategory {
    Navigation,
    Chat,
    Search,
    Moderation,
    Other,
    Custom,
}

impl HotkeyCategory {
    pub fn all() -> &'static [HotkeyCategory] {
        use HotkeyCategory::*;
        &[Navigation, Chat, Search, Moderation, Other, Custom]
    }

    pub fn label(&self) -> &'static str {
        match self {
            HotkeyCategory::Navigation => "Navigation",
            HotkeyCategory::Chat => "Chat",
            HotkeyCategory::Search => "Search",
            HotkeyCategory::Moderation => "Moderation",
            HotkeyCategory::Other => "Other",
            HotkeyCategory::Custom => "Custom",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            HotkeyCategory::Navigation => "🧭",
            HotkeyCategory::Chat => "💬",
            HotkeyCategory::Search => "🔎",
            HotkeyCategory::Moderation => "🛡",
            HotkeyCategory::Other => "⚙",
            HotkeyCategory::Custom => "⌘",
        }
    }

    /// Get actions belonging to this category
    pub fn actions(&self) -> &'static [HotkeyAction] {
        match self {
            HotkeyCategory::Navigation => NAVIGATION_ACTIONS,
            HotkeyCategory::Chat => CHAT_ACTIONS,
            HotkeyCategory::Search => SEARCH_ACTIONS,
            HotkeyCategory::Moderation => MODERATION_ACTIONS,
            HotkeyCategory::Other => OTHER_ACTIONS,
            HotkeyCategory::Custom => &[],
        }
    }
}

/// Hotkey cheat sheet component
pub struct HotkeyCheatSheet {
    pub state: HotkeyCheatSheetState,
}

impl HotkeyCheatSheet {
    pub fn new() -> Self {
        Self {
            state: HotkeyCheatSheetState::default(),
        }
    }

    pub fn open(&mut self) {
        self.state.is_open = true;
    }

    pub fn close(&mut self) {
        self.state.close();
    }

    pub fn is_open(&self) -> bool {
        self.state.is_open
    }
}

impl Default for HotkeyCheatSheet {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for HotkeyCheatSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.state.is_open {
            return div().into_any_element();
        }

        super::clear_global_tooltip();
        let modal_fit =
            crate::utils::fit_overlay_to_viewport(window.viewport_size(), 600.0, 500.0, 12.0);

        div()
            .id("hotkey-cheat-sheet-backdrop")
            .absolute()
            .inset_0()
            .bg(colors::backdrop())
            .flex()
            .items_center()
            .justify_center()
            .block_overlay_pointer_passthrough()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    this.close();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("hotkey-cheat-sheet")
                    .w(px(modal_fit.width))
                    .max_h(px(modal_fit.height))
                    .bg(colors::bg_elevated())
                    .rounded(Radii::lg())
                    .border_1()
                    .border_color(colors::border())
                    .shadow_lg()
                    .overflow_hidden()
                    .block_overlay_pointer_passthrough()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(render_header(cx))
                    .child(
                        div()
                            .id("hotkey-cheat-sheet-content")
                            .w_full()
                            .flex_1()
                            .p_4()
                            .overflow_y_scroll()
                            .child(render_categories()),
                    ),
            )
            .into_any_element()
    }
}

/// Render the modal header
fn render_header(cx: &mut Context<HotkeyCheatSheet>) -> impl IntoElement + use<> {
    div()
        .px(px(Spacing::LG))
        .py(px(Spacing::MD))
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(colors::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .child(div().text_base().child("⌨"))
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_primary())
                        .child("Keyboard shortcuts"),
                ),
        )
        .child(
            div()
                .id("cheat-sheet-close")
                .role(Role::Button)
                .aria_label("Close keyboard shortcuts")
                .on_hover(text_tooltip_on_hover(
                    "cheat-sheet-close".to_string(),
                    "Close".to_string(),
                ))
                .size(px(Sizes::BUTTON_SM))
                .flex()
                .items_center()
                .justify_center()
                .rounded(Radii::sm())
                .cursor_pointer()
                .text_color(colors::text_muted())
                .hover(|s| s.bg(colors::bg_hover()).text_color(colors::text_primary()))
                .on_activate(cx.listener(|this, _, _window, cx| {
                    this.close();
                    cx.notify();
                }))
                .child(CLOSE_GLYPH),
        )
}

/// Render all hotkey categories
fn render_categories() -> impl IntoElement {
    let categories: Vec<_> = HotkeyCategory::all()
        .iter()
        .copied()
        .filter(|category| *category != HotkeyCategory::Custom || !custom_hotkeys().is_empty())
        .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::MD))
        .children(categories.into_iter().map(render_category))
}

/// Render a single category with its hotkeys
fn render_category(category: HotkeyCategory) -> impl IntoElement {
    let actions = category.actions();
    let custom_actions = if category == HotkeyCategory::Custom {
        custom_hotkeys()
    } else {
        Vec::new()
    };

    div()
        .flex()
        .flex_col()
        .gap(px(Spacing::XS))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(Spacing::SM))
                .pt(px(Spacing::SM))
                .child(div().text_sm().child(category.icon()))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_secondary())
                        .child(category.label()),
                ),
        )
        .child(
            div().flex().flex_wrap().gap(px(Spacing::XS)).children(
                actions
                    .iter()
                    .copied()
                    .map(render_hotkey_row)
                    .chain(custom_actions.into_iter().map(render_custom_hotkey_row)),
            ),
        )
}

/// Render a single hotkey row
fn render_hotkey_row(action: HotkeyAction) -> AnyElement {
    let binding = hotkeys::get_binding(action);
    let is_set = binding.is_some();
    let binding_display = binding
        .map(|b| b.to_display())
        .unwrap_or_else(|| "Not set".to_string());
    render_binding_row(
        action.display_name(),
        action.scope_label(),
        &binding_display,
        is_set,
    )
}

fn render_custom_hotkey_row(action: hotkeys::CustomHotkeyAction) -> AnyElement {
    let (is_set, binding_display) = hotkeys::custom_hotkey_binding_display(&action.id);
    let scope_label = action.scope_label();
    render_binding_row(action.name, scope_label, &binding_display, is_set)
}

fn render_binding_row(
    name: impl Into<SharedString>,
    scope_label: impl Into<SharedString>,
    binding_display: &str,
    is_set: bool,
) -> AnyElement {
    div()
        .w(px(280.0))
        .max_w_full()
        .px(px(Spacing::SM))
        .py(px(Spacing::XS))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(Spacing::SM))
        .rounded(Radii::sm())
        .bg(colors::bg_hover())
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(Spacing::XXS))
                .child(
                    div()
                        .text_sm()
                        .text_color(colors::text_primary())
                        .text_ellipsis()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(name.into()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .text_ellipsis()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(scope_label.into()),
                ),
        )
        .child(render_key_badge(binding_display, is_set))
        .into_any_element()
}

/// Render a key binding badge
fn render_key_badge(binding: &str, is_set: bool) -> impl IntoElement + use<> {
    div()
        .px(px(Spacing::XS_PLUS))
        .py(px(Spacing::XXS))
        .rounded(Radii::xs())
        .bg(if is_set {
            colors::accent_transparent(0.15)
        } else {
            colors::bg_hover()
        })
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(if is_set {
            colors::accent_text()
        } else {
            colors::text_muted()
        })
        .whitespace_nowrap()
        .child(binding.to_string())
}

/// Render an inline keyboard hint badge
/// Use this to show hotkey hints on buttons when Alt is held
pub fn keyboard_hint(key: &str) -> impl IntoElement + use<> {
    div()
        .px(px(Spacing::XS))
        .py(px(Spacing::XXS))
        .rounded(Radii::xs())
        .bg(colors::bg_elevated())
        .border_1()
        .border_color(colors::border())
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors::text_muted())
        .child(key.to_string())
}

/// Render an inline keyboard hint with optional action lookup
pub fn keyboard_hint_for_action(action: HotkeyAction) -> AnyElement {
    if let Some(binding) = hotkeys::get_binding(action) {
        keyboard_hint(&binding.to_display()).into_any_element()
    } else {
        div()
            .bg(colors::bg_elevated())
            .border_1()
            .border_color(colors::border())
            .rounded(Radii::sm())
            .shadow_lg()
            .px(px(Spacing::XS_PLUS))
            .py(px(Spacing::XS))
            .child(
                div()
                    .text_size(px(FontSize::XS))
                    .text_color(colors::text_primary())
                    .whitespace_normal()
                    .child(action.display_name()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn test_category_actions() {
        for cat in HotkeyCategory::all() {
            if *cat == HotkeyCategory::Custom {
                continue;
            }
            assert!(!cat.actions().is_empty(), "{:?} has no actions", cat);
        }
    }
}
