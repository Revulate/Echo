//! Settings modal for Echo GPUI
//!
//! Full settings modal and native popout with compact shared navigation.
//! Settings are organized into seven pages plus Advanced (see
//! [`settings_navigation`]); developer diagnostics stay behind an explicit
//! disclosure and out of default search results.
//!
//! Features:
//! - Keyboard navigation (Tab/Arrow keys)
//! - Global settings search/filter
//! - Reusable UI widgets
//! - Popout window support

pub(crate) mod draft_input;
pub(crate) mod editor_state;
mod sections;
mod settings_modal;
pub mod settings_navigation;
pub mod settings_page;
mod settings_popout;
mod settings_results;
pub mod settings_surface;
pub mod widgets;

pub(crate) use sections::{
    cancel_all_settings_hotkey_captures, clear_message_cache, complete_settings_hotkey_capture,
    confirm_uploader_mapping, highlight_rule_editor_request, invalidate_logging_scan,
    reset_migrated_editor_drafts_after_settings_replace, reset_mod_buttons_to_defaults,
    resolve_uploader_status, uploader_confirmation_target,
};
pub use sections::{
    clear_capture_rejection, reject_plain_typing_capture, render_debug_section_live,
};
pub use settings_modal::*;
pub use settings_navigation::*;
pub use settings_popout::*;
pub use settings_surface::*;
