//! Authentication UI components
//!
//! Contains the login modal, the device-code and account-list bodies of the
//! top bar's account dropdown, and the profile avatar.

mod account_list;
mod device_code;
mod login_modal;
mod profile_avatar;

pub use account_list::render_account_list_inline;
pub use device_code::render_device_code_inline;
pub use login_modal::{LoginModalState, render_login_modal};
pub use profile_avatar::render_profile_avatar;
