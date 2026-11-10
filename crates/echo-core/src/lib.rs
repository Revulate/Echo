//! GPUI-free domain logic shared by Echo and its verification tools.
//!
//! # Ports
//!
//! A few behaviours need state that only the running application owns. The
//! crate asks for it through process-global hooks; each is set once (first
//! call wins), and every one is optional: without it the crate falls back to
//! the standalone behaviour named below, which is what tests and tools get.
//!
//! | Hook | Fallback when not installed |
//! | --- | --- |
//! | [`install_log_category_enabled`] | every `log_*!` category is enabled |
//! | [`channel_identity::install_current_watching_channel`] | no channel is being watched |
//! | [`channel_lifecycle::install_whispers_channel_predicate`] | no channel is the whispers channel |
//! | [`emote_preferences::install_emote_preferences_ports`] | preferences are not persisted |
//! | [`message_buffer::install_message_buffer_ports`] | default capacity, no layout-cache or embed bookkeeping |
//! | [`slot_resolver::install_twitch_emote_lookup`] | Twitch emotes resolve by id only |
//! | [`slot_resolver::install_emoji_style`] | the default emoji style |
//! | [`slot_resolver::install_emote_trace_enabled`] | emote tracing is off |

use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub mod badge_key;
pub mod channel_identity;
pub mod channel_lifecycle;
pub mod chat_snapshot;
pub mod chatterino_import;
pub mod commands;
pub mod deduplicator;
pub mod emote_display_rules;
pub mod emote_preferences;
pub mod env;
pub mod file_replace;
pub mod filter_expression;
pub mod followed_live;
pub mod freedesktop;
pub mod fuzzy;
pub mod highlight_explanation;
pub mod hotkeys;
pub mod message;
pub mod message_buffer;
mod message_search;
pub mod notification_text;
pub mod oauth_token;
pub mod observers;
pub mod overlay_state;
pub mod quick_switch;
pub mod reveal_pacer;
pub mod search;
pub mod settings_event;
pub mod slot;
pub mod slot_parser;
pub mod slot_resolver;
mod split_id;
pub mod split_overlay;
pub mod sync;
pub mod tab_state;
pub mod twitch_gif;
pub mod version_registry;
pub mod workspace;

// The root re-exports only the crate's central types; functions are reached
// through their module. `SplitId` lives only here.
pub use message::ChatMessage;
pub use split_id::SplitId;
pub use workspace::{AddSplitDirection, DragDropState, DropZone, SplitNode, SplitOrientation};

static LOG_CATEGORY_ENABLED: OnceLock<fn(&str) -> bool> = OnceLock::new();

/// Install the application's log-category predicate.
///
/// The root crate owns the `ECHO_LOG` category flags; this crate only needs to
/// ask whether a category is active. Without an installed predicate every
/// category reports enabled so extracted code logs normally when it runs
/// standalone.
pub fn install_log_category_enabled(callback: fn(&str) -> bool) {
    let _ = LOG_CATEGORY_ENABLED.set(callback);
}

pub fn log_category_enabled(category: &str) -> bool {
    LOG_CATEGORY_ENABLED
        .get()
        .is_none_or(|callback| callback(category))
}

/// Shared body of the `log_*!` macros: trace/debug/info are gated on the
/// category predicate, warn/error always pass through to the tracing filter.
#[doc(hidden)]
#[macro_export]
macro_rules! __echo_core_log {
    ($cat:literal, $target:literal, warn, $($arg:tt)*) => {{
        tracing::warn!(target: $target, $($arg)*)
    }};
    ($cat:literal, $target:literal, error, $($arg:tt)*) => {{
        tracing::error!(target: $target, $($arg)*)
    }};
    ($cat:literal, $target:literal, $level:ident, $($arg:tt)*) => {{
        if $crate::log_category_enabled($cat) {
            tracing::$level!(target: $target, $($arg)*)
        }
    }};
}

/// IRC logging (connection, watchdog, lifecycle, supervisor)
#[macro_export]
macro_rules! log_irc {
    ($($arg:tt)*) => { $crate::__echo_core_log!("irc", "echo::irc", $($arg)*) };
}

/// UI logging (app events, overlays, input)
#[macro_export]
macro_rules! log_ui {
    ($($arg:tt)*) => { $crate::__echo_core_log!("ui", "echo::ui", $($arg)*) };
}

/// Emote-provider logging
#[macro_export]
macro_rules! log_emotes {
    ($($arg:tt)*) => { $crate::__echo_core_log!("emotes", "echo::emotes", $($arg)*) };
}

/// Serde default for persisted flags that start enabled.
pub(crate) fn default_true() -> bool {
    true
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
#[inline]
pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `Instant::now() - ago`, or `None` when the monotonic clock has not run that long.
///
/// Windows `Instant` counts from boot, so plain subtraction panics early in an
/// uptime.
pub fn instant_ago(ago: Duration) -> Option<Instant> {
    Instant::now().checked_sub(ago)
}
