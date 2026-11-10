//! Echo UI components - custom implementations replacing gpui-component
//!
//! Provides a minimal set of UI components needed by Echo:
//! - Root: Window wrapper component
//! - Input: Text input with full editing capabilities

pub(crate) mod accessibility;
mod button;
pub mod input;
pub mod menu;
mod native_window_root;
mod press;
mod root;
mod still_frame;
mod topmost;

pub use button::{ButtonExt, button_hover, focus_ring, is_button_activation_key};
pub use native_window_root::open_native_window;
pub use press::PressToClick;
pub use root::{Root, Tab as FocusNext, TabPrev as FocusPrev};
pub use still_frame::still_frame_source;
pub use topmost::{AlwaysOnTopFollower, refresh_windows_for_always_on_top};

/// Initialize Echo UI components (call once at startup)
pub fn init(cx: &mut gpui::App) {
    root::init(cx);
    input::init(cx);
}
