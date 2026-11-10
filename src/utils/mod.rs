//! Utility functions for Echo GPUI

mod atomic_file;
pub mod channel_input;
mod clipboard;
pub mod color;
pub mod fuzzy;
mod image_drops;
#[cfg(target_os = "linux")]
pub mod kwin;
mod open_url;
pub mod timestamp;
mod window;

pub use clipboard::*;
pub use color::{
    emote_provider_color, parse_hex_color_opt, parse_optional_hex_or, parse_user_color_opt,
};
pub use echo_core::notification_text::truncate_message as truncate_text;
pub use fuzzy::fuzzy_score;
pub use image_drops::drain_render_image_drops_coalesced;
pub use open_url::*;
pub use timestamp::{
    format_date_time, format_message_timestamp, format_sidebar_timestamp, format_time_24h_sec,
    unix_now_ms, unix_now_secs,
};
pub use window::*;

pub(crate) use atomic_file::{
    backup_generation_paths, json_error_summary, preserve_unloadable_file, quarantine_corrupt_file,
    replace_file, rotate_known_good_backups,
};

/// Parse an environment variable as `usize`, returning `default` if absent or unparseable.
pub(crate) fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}
