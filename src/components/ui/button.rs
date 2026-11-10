use crate::components::ui::PressToClick;
use crate::theme::colors;
use gpui::*;
use std::rc::Rc;

/// Default hover treatment for a button without its own hover colour.
pub fn button_hover(style: StyleRefinement) -> StyleRefinement {
    style.bg(colors::bg_hover())
}

/// Keyboard focus ring for a control painted on `surface`.
///
/// A spread box shadow rather than a border: GPUI borders take part in layout,
/// so widening one on focus would reflow the row. The ring color is
/// contrast-qualified, so a low-contrast custom accent falls back to a readable
/// foreground instead of vanishing.
pub fn focus_ring(surface: Hsla) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(0.0), colors::settings_focus_ring(surface))
            .spread_radius(px(2.0)),
    ]
}

/// Enter or Space without modifiers.
///
/// Modified presses are left alone so application shortcuts (Ctrl+F, Alt+Down)
/// still reach their handlers.
pub fn is_button_activation_key(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
        && !event.keystroke.modifiers.control
        && !event.keystroke.modifiers.alt
        && !event.keystroke.modifiers.platform
        && !event.keystroke.modifiers.function
}

/// Shared button behaviour on top of [`PressToClick`].
///
/// A button acts on a completed left click (press, then release over the same
/// button; dragging off cancels), shows the pressed style while held, and
/// stops its press so drag surfaces and backdrops behind it never see it,
/// which also keeps keyboard focus where it was: a pointer press never moves
/// focus onto a button. [`ButtonExt::on_activate`] buttons are tab stops, so
/// Tab reaches them, they show the focus ring while focused, and Enter/Space
/// activate them. GPUI keeps the focus handle per element id, so the id must
/// be stable.
pub trait ButtonExt: PressToClick + Styled + Sized {
    /// One activation callback for pointer, keyboard and assistive technology.
    /// Use this instead of adding a second explicit accessibility Click handler.
    fn on_accessible_activate(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let handler = Rc::new(handler);
        let pointer = handler.clone();
        self.on_activate(move |event, window, cx| pointer(event, window, cx))
            .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                handler(&ClickEvent::default(), window, cx)
            })
    }

    fn on_accessible_activate_if(
        self,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if enabled {
            self.on_accessible_activate(handler)
        } else {
            self.disabled_button()
        }
    }
    fn on_activate(self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.tab_index(0).on_activate_without_tab_stop(handler)
    }

    /// [`ButtonExt::on_activate`] for rows of lists and grids whose owner
    /// already moves through them with arrow keys (autocomplete, quick
    /// switcher, emote grid): pointer activation only, never a tab stop.
    fn on_activate_without_tab_stop(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let handler = Rc::new(handler);
        let key_handler = Rc::clone(&handler);
        let ring = focus_ring(colors::bg_surface());
        self.focus_visible(move |style| style.shadow(ring))
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if is_button_activation_key(event) {
                    cx.stop_propagation();
                    if !event.is_held {
                        key_handler(&ClickEvent::default(), window, cx);
                    }
                }
            })
            .on_any_mouse_down(|event, window, _| {
                if event.button != MouseButton::Left {
                    // A right or middle press must not focus the button either.
                    window.prevent_default();
                }
            })
            .on_press_click(move |event, window, cx| handler(event, window, cx))
    }

    /// Dimmed, not-allowed cursor, no hover or pressed style, not a tab stop,
    /// and no click or Enter/Space reaches a handler registered on the element,
    /// before or after this call: the left press and the activation keys stop
    /// in the capture phase, ahead of GPUI's click tracking and every bubble
    /// listener.
    fn disabled_button(self) -> Self {
        self.opacity(0.5)
            .a11y_synthetic_children(|builder| {
                super::accessibility::apply_description(builder);
                builder.parent_node().set_disabled();
            })
            .cursor_not_allowed()
            .hover(|style| style)
            .active(|style| style)
            .tab_stop(false)
            .capture_any_mouse_down(|event, _, cx| {
                if event.button == MouseButton::Left {
                    cx.stop_propagation();
                }
            })
            .capture_key_down(|event, _, cx| {
                if is_button_activation_key(event) {
                    cx.stop_propagation();
                }
            })
    }

    fn on_activate_if(
        self,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if enabled {
            self.on_activate(handler)
        } else {
            self.disabled_button()
        }
    }
}

impl<E: PressToClick + Styled> ButtonExt for E {}
