//! Echo's shared UI building blocks, written directly on GPUI.
//!
//! - [`Root`]: main-window wrapper with Tab/Shift-Tab focus navigation
//! - [`input`]: the single-line text input used across the app
//! - [`menu`]: keyboard-navigable chrome menus (panel, rows, highlight)
//! - buttons: completed-click activation ([`PressToClick`]), hover, focus ring
//! - `accessibility`: focus reveal and accessible names for compound controls
//! - [`open_native_window`]: the one way Echo opens a native window
//! - [`SingletonWindow`]: open flag, handle and pending payload of a one-instance popout
//! - [`still_frame_source`]: first-frame image source for still emotes
//! - [`AlwaysOnTopFollower`]: keeps popouts in step with the Always-on-top setting

pub(crate) mod accessibility;
mod button;
pub mod input;
pub mod menu;
mod native_window_root;
mod press;
mod root;
mod singleton_window;
mod still_frame;
mod topmost;

pub use button::{ButtonExt, button_hover, focus_ring, is_button_activation_key};
pub use native_window_root::open_native_window;
pub use press::PressToClick;
pub use root::{Root, Tab as FocusNext, TabPrev as FocusPrev};
pub use singleton_window::{Retarget, SingletonWindow};
pub use still_frame::still_frame_source;
pub use topmost::{AlwaysOnTopFollower, refresh_windows_for_always_on_top};

/// Initialize Echo UI components (call once at startup)
pub fn init(cx: &mut gpui::App) {
    root::init(cx);
    input::init(cx);
}
