//! Per-canvas layout mode: whether rows carry inline moderation buttons.
//!
//! Moderation mode is a split setting, but rows are shaped by shared code and
//! cached across splits, so the rendering canvas publishes its mode for the
//! layouts built during its frame and async jobs carry the mode they were
//! requested with, like [`super::layout_scale`].

use std::cell::Cell;

thread_local! {
    static MOD_BUTTONS: Cell<bool> = const { Cell::new(false) };
}

/// Publish whether the rendering canvas shows inline moderation buttons.
#[inline]
pub fn set_mod_buttons(enabled: bool) {
    MOD_BUTTONS.with(|cell| cell.set(enabled));
}

#[inline]
pub fn mod_buttons() -> bool {
    MOD_BUTTONS.with(Cell::get)
}

/// Build layouts with `enabled` on this thread until the guard drops.
#[must_use]
pub fn enter(enabled: bool) -> ModeScope {
    let previous = mod_buttons();
    set_mod_buttons(enabled);
    ModeScope { previous }
}

pub struct ModeScope {
    previous: bool,
}

impl Drop for ModeScope {
    fn drop(&mut self) {
        set_mod_buttons(self.previous);
    }
}
