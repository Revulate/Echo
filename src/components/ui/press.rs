use gpui::{App, ClickEvent, MouseButton, StatefulInteractiveElement, Styled, Window};

/// Completed-click activation for buttons whose action must not run on press.
///
/// The press only arms the element (GPUI keeps the pending press per element
/// id, so the id must be stable and specific to the button's target); the
/// handler runs when the left button is released over the same element, and a
/// release anywhere else disarms it. Only mouse clicks fire: keyboard
/// activation stays with each control's own key handling.
pub trait PressToClick: StatefulInteractiveElement + Sized {
    /// Completed click whose press also stops propagation, so drag surfaces
    /// and backdrops behind the button never see it.
    fn on_press_click(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_completed_click(handler)
    }

    /// Completed click that lets the press propagate, so focus transfer and
    /// parent press listeners still run.
    fn on_completed_click(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.active(|style| style.opacity(0.7))
            .on_click(move |event, window, cx| {
                if matches!(event, ClickEvent::Mouse(_)) && event.standard_click() {
                    handler(event, window, cx);
                }
            })
    }

    /// Completed right click: the handler runs when the right button is
    /// released over the element it was pressed on. The press propagates.
    fn on_right_click(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_aux_click(move |event, window, cx| {
            if event.is_right_click() {
                handler(event, window, cx);
            }
        })
    }
}

impl<E: StatefulInteractiveElement> PressToClick for E {}
