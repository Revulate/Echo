//! Compatibility facade for slot resolution.
//!
//! The resolver is GPUI-free and lives in `echo-core`; the application installs
//! its emote-map, emoji-style, and emote-trace ports during backend startup.

pub use echo_core::slot_resolver::*;

/// Point the extracted resolver at this application's runtime lookups.
pub fn install_resolver_ports() {
    install_twitch_emote_lookup(|channel, id| {
        crate::backend::emotes::get_twitch_emote_by_id_fast(channel, id)
    });
    install_emoji_style(crate::backend::settings::get_emoji_style);
    install_emote_trace_enabled(crate::log::emote_trace_enabled);
}
