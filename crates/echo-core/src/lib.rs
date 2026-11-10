//! GPUI-free domain logic shared by Echo and its verification tools.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub mod channel_identity;
pub mod channel_lifecycle;
pub mod chat_snapshot;
pub mod chatterino_import;
pub mod commands;
pub mod deduplicator;
pub mod emote_display_rules;
pub mod emote_preferences;
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

pub use fuzzy::fuzzy_score;
pub use message::ChatMessage;
pub use split_id::SplitId;
pub use workspace::{
    AddSplitDirection, DragDropState, DropZone, SplitNode, SplitOrientation, is_placeholder,
    new_placeholder,
};

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

/// IRC logging (connection, watchdog, lifecycle, supervisor)
#[macro_export]
macro_rules! log_irc {
    (trace, $($arg:tt)*) => {{
        if $crate::log_category_enabled("irc") {
            tracing::trace!(target: "echo::irc", $($arg)*)
        }
    }};
    (debug, $($arg:tt)*) => {{
        if $crate::log_category_enabled("irc") {
            tracing::debug!(target: "echo::irc", $($arg)*)
        }
    }};
    (info, $($arg:tt)*) => {{
        if $crate::log_category_enabled("irc") {
            tracing::info!(target: "echo::irc", $($arg)*)
        }
    }};
    (warn, $($arg:tt)*) => {{
        tracing::warn!(target: "echo::irc", $($arg)*)
    }};
    (error, $($arg:tt)*) => {{
        tracing::error!(target: "echo::irc", $($arg)*)
    }};
}

/// UI logging (app events, overlays, input)
#[macro_export]
macro_rules! log_ui {
    (trace, $($arg:tt)*) => {{
        if $crate::log_category_enabled("ui") {
            tracing::trace!(target: "echo::ui", $($arg)*)
        }
    }};
    (debug, $($arg:tt)*) => {{
        if $crate::log_category_enabled("ui") {
            tracing::debug!(target: "echo::ui", $($arg)*)
        }
    }};
    (info, $($arg:tt)*) => {{
        if $crate::log_category_enabled("ui") {
            tracing::info!(target: "echo::ui", $($arg)*)
        }
    }};
    (warn, $($arg:tt)*) => {{
        tracing::warn!(target: "echo::ui", $($arg)*)
    }};
    (error, $($arg:tt)*) => {{
        tracing::error!(target: "echo::ui", $($arg)*)
    }};
}

/// Emote-provider logging
#[macro_export]
macro_rules! log_emotes {
    (trace, $($arg:tt)*) => {{
        if $crate::log_category_enabled("emotes") {
            tracing::trace!(target: "echo::emotes", $($arg)*)
        }
    }};
    (debug, $($arg:tt)*) => {{
        if $crate::log_category_enabled("emotes") {
            tracing::debug!(target: "echo::emotes", $($arg)*)
        }
    }};
    (info, $($arg:tt)*) => {{
        if $crate::log_category_enabled("emotes") {
            tracing::info!(target: "echo::emotes", $($arg)*)
        }
    }};
    (warn, $($arg:tt)*) => {{
        tracing::warn!(target: "echo::emotes", $($arg)*)
    }};
    (error, $($arg:tt)*) => {{
        tracing::error!(target: "echo::emotes", $($arg)*)
    }};
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
