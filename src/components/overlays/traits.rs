//! Common overlay state and input-isolation helpers.

use gpui::{InteractiveElement, MouseButton};

/// Trait for components that can be opened and closed.
pub trait Openable {
    /// Set the open state.
    fn set_open(&mut self, open: bool);

    /// Open the overlay.
    fn open(&mut self) {
        self.set_open(true);
    }

    /// Close the overlay.
    fn close(&mut self) {
        self.set_open(false);
    }
}

/// Make an overlay surface the topmost pointer target and consume its
/// non-primary pointer phases.
///
/// Callers intentionally retain the left-button handler so a backdrop can
/// close itself or a card can begin a drag. Occlusion stops covered chat
/// hitboxes from becoming eligible in the first place; the remaining handlers
/// prevent mouse movement, wheel scrolling, secondary clicks, and every
/// release from bubbling beyond the overlay after it has handled its own
/// interaction. Child controls and scroll views still receive their event
/// before the overlay's bubbling handlers run.
pub fn block_overlay_pointer_passthrough<E: InteractiveElement>(element: E) -> E {
    element
        .occlude()
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
}

/// Extension method form of [`block_overlay_pointer_passthrough`].
///
/// Keeping this on [`InteractiveElement`] implementors lets overlay renderers
/// retain their existing builder-chain layout while sharing the same isolation
/// behavior.
pub trait OverlayPointerIsolation: InteractiveElement {
    fn block_overlay_pointer_passthrough(self) -> Self
    where
        Self: Sized,
    {
        block_overlay_pointer_passthrough(self)
    }
}

impl<E: InteractiveElement> OverlayPointerIsolation for E {}
