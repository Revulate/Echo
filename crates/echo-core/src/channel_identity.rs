//! GPUI-free identity policy for Echo's virtual channel leaves.
//!
//! A layout leaf can hold an ordinary Twitch login, the stable Watching
//! sentinel, or the Whispers channel. The two sentinel predicates are pure, so
//! they live here directly. Resolving the Watching sentinel to the login the
//! browser currently reports is a runtime lookup, so the root installs it as a
//! port; without an installed port a Watching leaf simply resolves to nothing,
//! which is also what it does before the browser reports a selection.

use std::sync::OnceLock;

/// Stable persisted layout value for the browser Watching leaf.
pub const WATCHING_CHANNEL: &str = "__echo_watching__";

/// Stable channel name for the Whispers leaf.
pub const WHISPERS_CHANNEL: &str = "whispers";

static CURRENT_WATCHING_CHANNEL: OnceLock<fn() -> Option<String>> = OnceLock::new();

/// Install the runtime lookup for the browser-selected Watching login.
pub fn install_current_watching_channel(callback: fn() -> Option<String>) {
    let _ = CURRENT_WATCHING_CHANNEL.set(callback);
}

/// Whether this layout value is the Watching sentinel.
pub fn is_watching_channel(channel: &str) -> bool {
    channel == WATCHING_CHANNEL
}

/// Whether this layout value addresses the Whispers channel.
pub fn is_whispers_channel(channel: &str) -> bool {
    channel
        .trim()
        .trim_start_matches('#')
        .eq_ignore_ascii_case(WHISPERS_CHANNEL)
}

/// The normalized Twitch login currently selected for the Watching leaf.
pub fn current_watching_channel() -> Option<String> {
    CURRENT_WATCHING_CHANNEL
        .get()
        .and_then(|callback| callback())
}

/// Resolve a layout value to the concrete channel it addresses.
pub fn resolve_layout_channel(channel: &str) -> Option<String> {
    if is_watching_channel(channel) {
        current_watching_channel()
    } else {
        Some(channel.to_string())
    }
}
