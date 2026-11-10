//! Authentication UI components
//!
//! Contains login modal, account switcher, device code, account list,
//! and profile avatar components.

mod account_list;
mod device_code;
mod login_modal;
mod profile_avatar;

pub use account_list::render_account_list_inline;
pub use device_code::render_device_code_inline;
pub use login_modal::{LoginModal, LoginModalState};
pub use profile_avatar::render_profile_avatar;
