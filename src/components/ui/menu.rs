use crate::components::ui::{FocusNext, FocusPrev, PressToClick};
use crate::icons::MenuMetrics;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::Cell;
use std::rc::Rc;

/// Activation callback of one keyboard-reachable menu row.
pub type MenuAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// Keyboard highlight shared by a menu's rows and its key handler.
///
/// Clones share one cell, so the row that the pointer last crossed and the row
/// the arrow keys moved to are always the same row.
#[derive(Clone, Default)]
pub struct MenuNav {
    highlighted: Rc<Cell<Option<usize>>>,
}

impl MenuNav {
    pub fn highlighted(&self) -> Option<usize> {
        self.highlighted.get()
    }

    pub fn is_highlighted(&self, index: usize) -> bool {
        self.highlighted.get() == Some(index)
    }

    pub fn reset(&self) {
        self.highlighted.set(None);
    }

    /// Returns true when the highlight moved.
    pub fn highlight(&self, index: usize) -> bool {
        self.highlighted.replace(Some(index)) != Some(index)
    }

    fn step(&self, delta: isize, len: usize) {
        if len == 0 {
            self.highlighted.set(None);
            return;
        }
        let next = match (self.highlighted.get().filter(|index| *index < len), delta) {
            (None, delta) if delta < 0 => len - 1,
            (None, _) => 0,
            (Some(0), delta) if delta < 0 => len - 1,
            (Some(index), delta) if delta < 0 => index - 1,
            (Some(index), _) => (index + 1) % len,
        };
        self.highlighted.set(Some(next));
    }
}

/// What a menu owner hands the component that renders its panel: the focus
/// the panel tracks, the shared highlight, and how Escape closes it.
#[derive(Clone)]
pub struct MenuHost {
    pub focus: FocusHandle,
    pub nav: MenuNav,
    pub on_close: MenuAction,
}

impl MenuHost {
    pub fn close_handler(&self) -> impl Fn(&mut Window, &mut App) + 'static {
        let on_close = self.on_close.clone();
        move |window, cx| on_close(window, cx)
    }
}

/// Keys a menu reacts to. Modified presses are not menu keys, so application
/// shortcuts keep working while a menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Close,
    Previous,
    Next,
    First,
    Last,
    Activate,
}

pub fn menu_key(event: &KeyDownEvent) -> Option<MenuKey> {
    let modifiers = event.keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return None;
    }
    match event.keystroke.key.as_str() {
        "escape" => Some(MenuKey::Close),
        "up" => Some(MenuKey::Previous),
        "down" => Some(MenuKey::Next),
        "home" => Some(MenuKey::First),
        "end" => Some(MenuKey::Last),
        "enter" | "space" => Some(MenuKey::Activate),
        _ => None,
    }
}

impl MenuNav {
    /// Apply a menu key. Returns the action to run for `Activate`.
    pub fn apply(&self, key: MenuKey, actions: &[MenuAction]) -> Option<MenuAction> {
        match key {
            MenuKey::Previous => self.step(-1, actions.len()),
            MenuKey::Next => self.step(1, actions.len()),
            MenuKey::First if !actions.is_empty() => self.highlighted.set(Some(0)),
            MenuKey::Last if !actions.is_empty() => self.highlighted.set(Some(actions.len() - 1)),
            MenuKey::Activate => {
                return self
                    .highlighted
                    .get()
                    .and_then(|index| actions.get(index))
                    .cloned();
            }
            _ => {}
        }
        None
    }
}

/// Panel shared by every chrome menu: elevated surface, `Radii::md`, large
/// shadow and the [`MenuMetrics`] vertical inset.
pub fn menu_panel() -> Div {
    div()
        .bg(colors::bg_elevated())
        .rounded(Radii::md())
        .border_1()
        .border_color(colors::border())
        .shadow_lg()
        .py(px(MenuMetrics::PANEL_PADDING / 2.0))
        .overflow_hidden()
}

/// One actionable menu row at keyboard position `index`: fixed row height,
/// hover and keyboard highlight, completed-click activation.
pub fn menu_item(
    id: impl Into<ElementId>,
    index: usize,
    nav: &MenuNav,
    action: MenuAction,
) -> Stateful<Div> {
    let hover_nav = nav.clone();
    div()
        .id(id)
        .w_full()
        .min_h(px(MenuMetrics::ITEM_HEIGHT))
        .px(px(MenuMetrics::ITEM_PADDING_X))
        .flex()
        .items_center()
        .gap(px(MenuMetrics::ITEM_GAP))
        .cursor_pointer()
        .when(nav.is_highlighted(index), |row| row.bg(colors::bg_hover()))
        .hover(|style| style.bg(colors::bg_hover()))
        .on_mouse_move(move |_, window, _| {
            if hover_nav.highlight(index) {
                window.refresh();
            }
        })
        .on_press_click(move |_, window, cx| action(window, cx))
}

pub fn menu_divider() -> Div {
    div()
        .w_full()
        .h(px(1.0))
        .my(px((MenuMetrics::DIVIDER_HEIGHT - 1.0) / 2.0))
        .bg(colors::border())
}

pub fn menu_header() -> Div {
    div()
        .w_full()
        .h(px(MenuMetrics::HEADER_HEIGHT))
        .px(px(MenuMetrics::ITEM_PADDING_X))
        .flex()
        .items_center()
        .text_size(px(MenuMetrics::SECONDARY_TEXT))
        .text_color(colors::text_muted())
}

/// Keyboard contract for an open menu panel that holds `focus`: Up/Down,
/// Home/End move the highlight across `actions` (Tab and Shift+Tab step like
/// Down and Up, as in Qt menus, instead of moving focus out), Enter/Space run
/// the highlighted row, Escape calls `on_close`. Other unmodified keys stop
/// here so typing does not leak into the view behind the menu.
/// Settings is an explicit exception: its shell captures both EchoRoot Tab
/// actions (modal) and raw Tab keys (native popout) before this handler, closes
/// menus without selecting an option, then traverses to the next/previous
/// control. Other chrome menus retain highlight stepping with Tab.
pub trait MenuKeyboard: InteractiveElement + Sized {
    fn menu_keyboard(
        self,
        focus: &FocusHandle,
        nav: &MenuNav,
        actions: Vec<MenuAction>,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        let nav = nav.clone();
        let action_count = actions.len();
        let next_nav = nav.clone();
        let prev_nav = nav.clone();
        self.track_focus(focus)
            .on_action(move |_: &FocusNext, window, _| {
                next_nav.step(1, action_count);
                window.refresh();
            })
            .on_action(move |_: &FocusPrev, window, _| {
                prev_nav.step(-1, action_count);
                window.refresh();
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(key) = menu_key(event) else {
                    let modifiers = event.keystroke.modifiers;
                    if !(modifiers.control || modifiers.alt || modifiers.platform) {
                        cx.stop_propagation();
                    }
                    return;
                };
                cx.stop_propagation();
                if key == MenuKey::Close {
                    on_close(window, cx);
                } else if let Some(action) = nav.apply(key, &actions) {
                    action(window, cx);
                }
                window.refresh();
            })
    }
}

impl<E: InteractiveElement> MenuKeyboard for E {}
