//! In-app overlays and the popout windows that host them.
//!
//! Overlays drawn inside the main window: user card, emote picker, context
//! menu, autocomplete, search, mod log, toasts, thread viewer, confirmation
//! modal, hotkey cheat sheet, metrics overlay, chatter list, quick switcher and
//! the global tooltip. Popout windows: the emote picker, user card and media
//! inspector. `image_ready` is the overlays' shared image-loaded repaint signal.

mod autocomplete;
mod chatter_list;
mod confirmation_modal;
mod context_menu;
mod emote_picker;
mod global_tooltip;
mod hotkey_cheat_sheet;
mod image_ready;
mod media_inspector;
mod metrics_overlay;
mod mod_log;
mod quick_switcher;
mod search_modal;
mod thread_viewer;
mod toast;
pub mod traits;
mod user_card;
mod user_card_popout;

pub use autocomplete::*;
pub use chatter_list::*;
pub use confirmation_modal::*;
pub use context_menu::*;
pub(crate) use emote_picker::record_emote_usage;
pub use emote_picker::*;
pub use global_tooltip::*;
pub use hotkey_cheat_sheet::*;
pub use image_ready::{
    ImageReadyConsumer, notify_animation_refilled, notify_image_loaded, take_image_ready,
};
pub use media_inspector::*;
pub use metrics_overlay::*;
pub use mod_log::*;
pub use quick_switcher::*;
pub use search_modal::*;
pub use thread_viewer::*;
pub use toast::*;
pub use user_card::*;
pub use user_card_popout::*;
