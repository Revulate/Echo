//! Device scale factor that chat layout is built for.
//!
//! Each canvas renders inside one native window, and windows on different
//! monitors can have different scale factors. The canvas publishes its
//! window's scale for the layouts built on the UI thread during its frame, and
//! async layout jobs carry the scale they were requested at. Layouts record the
//! scale so a canvas never reuses one shaped for another window's scale.

use std::cell::Cell;

thread_local! {
    static CURRENT_SCALE_BITS: Cell<u32> = const { Cell::new(0) };
}

/// Publish the scale factor of the window whose canvas is rendering.
#[inline]
pub fn set_current(scale: f32) {
    if scale.is_finite() && scale > 0.0 {
        CURRENT_SCALE_BITS.with(|bits| bits.set(scale.to_bits()));
    }
}

/// Scale factor layouts on this thread are built for. Threads that never
/// published one use the main window's scale.
#[inline]
pub fn current() -> f32 {
    match CURRENT_SCALE_BITS.with(Cell::get) {
        0 => crate::backend::settings::get_window_scale_factor(),
        bits => f32::from_bits(bits),
    }
}

#[inline]
pub fn current_bits() -> u32 {
    current().to_bits()
}

/// Build layouts for `scale` on this thread until the returned guard drops.
#[must_use]
pub fn enter(scale: f32) -> ScaleScope {
    let previous = CURRENT_SCALE_BITS.with(Cell::get);
    set_current(scale);
    ScaleScope { previous }
}

pub struct ScaleScope {
    previous: u32,
}

impl Drop for ScaleScope {
    fn drop(&mut self) {
        CURRENT_SCALE_BITS.with(|bits| bits.set(self.previous));
    }
}
