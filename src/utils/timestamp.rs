//! Timestamp formatting utilities for Echo
//!
//! Provides unified timestamp formatting for various display contexts:
//! - Absolute time (24h, 12h, with/without seconds)
//! - Relative time ("5m ago", "2h ago")
//! - Compact relative time ("5m", "2h")
//! - Date-time formats

use chrono::{Local, TimeZone, Timelike};

use crate::backend::settings::{self, CompiledTimestampFormatter, MessageTimestampFormat};

pub use echo_core::unix_now_ms;

/// Seconds since the Unix epoch, or 0 if the clock is before it.
#[inline]
pub fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Represents a timestamp that could be in seconds or milliseconds
#[derive(Clone, Copy)]
pub enum TimestampInput {
    Seconds(i64),
    Millis(i64),
}

impl TimestampInput {
    /// Convert to seconds
    #[inline]
    pub fn as_seconds(self) -> i64 {
        match self {
            TimestampInput::Seconds(s) => s,
            TimestampInput::Millis(ms) => ms / 1000,
        }
    }

    /// Convert to milliseconds
    #[inline]
    pub fn as_millis(self) -> i64 {
        match self {
            TimestampInput::Seconds(s) => s * 1000,
            TimestampInput::Millis(ms) => ms,
        }
    }
}

impl From<i64> for TimestampInput {
    /// Heuristic: values > year 2100 in seconds are treated as milliseconds
    fn from(value: i64) -> Self {
        if value > 4_102_444_800 {
            TimestampInput::Millis(value)
        } else {
            TimestampInput::Seconds(value)
        }
    }
}

/// Format timestamp as HH:MM:SS (24-hour with seconds)
#[inline]
pub fn format_time_24h_sec(timestamp: impl Into<TimestampInput>) -> String {
    let ts = timestamp.into();
    if let Some(dt) = Local.timestamp_opt(ts.as_seconds(), 0).single() {
        dt.format("%H:%M:%S").to_string()
    } else {
        "unknown".to_string()
    }
}

/// Format a chat-message timestamp using the user's compiled token pattern and
/// the operating system's local timezone.
///
/// Returns `None` while message timestamps are disabled. Channel-live
/// visibility is resolved by the caller because only split-scoped renderers
/// have authoritative live-state context.
#[inline]
pub fn format_message_timestamp(timestamp: impl Into<TimestampInput>) -> Option<String> {
    if !settings::get_show_message_timestamps() {
        return None;
    }
    format_message_timestamp_with_formatter(timestamp, settings::message_timestamp_formatter())
}

/// Format one message timestamp with a legacy `MessageTimestampFormat` value.
///
/// Kept for surfaces that still hold a `MessageTimestampFormat` rather than the
/// live pattern. It renders the legacy shape faithfully by decoding the enum
/// through the same token model, so it cannot drift from the real formatter —
/// but it does not reproduce custom separators or token order. Prefer
/// [`format_message_timestamp_with_formatter`].
pub fn format_message_timestamp_with_format(
    timestamp: impl Into<TimestampInput>,
    format: MessageTimestampFormat,
) -> Option<String> {
    format_message_timestamp_with_formatter(timestamp, format.into_pattern().compile(0))
}

/// Format one message timestamp with an already-loaded compiled formatter.
///
/// This is public for the canvas cache, which loads the published formatter
/// once per batch and keys cached strings by its generation instead of
/// compiling a pattern or locking settings per message. It accepts anything
/// that dereferences to a formatter, so callers can pass the published
/// `Arc` directly without cloning it per message.
pub fn format_message_timestamp_with_formatter(
    timestamp: impl Into<TimestampInput>,
    formatter: impl AsRef<CompiledTimestampFormatter>,
) -> Option<String> {
    let ts = timestamp.into();
    let datetime = Local.timestamp_millis_opt(ts.as_millis()).single()?;
    Some(formatter.as_ref().format_time(
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
        datetime.timestamp_subsec_millis(),
    ))
}

/// Format timestamp as MM/DD HH:MM for search results
#[inline]
pub fn format_date_time(timestamp: impl Into<TimestampInput>) -> String {
    let ts = timestamp.into();
    if let Some(dt) = Local.timestamp_opt(ts.as_seconds(), 0).single() {
        dt.format("%m/%d %H:%M").to_string()
    } else {
        "Unknown".to_string()
    }
}

/// Format a sidebar timestamp (milliseconds since epoch).
///
/// Returns `HH:MM` when the timestamp is from today, or `YYYY-MM-DD` otherwise.
/// Returns an empty string if the timestamp cannot be parsed.
pub fn format_sidebar_timestamp(timestamp_ms: i64) -> String {
    let Some(dt) = Local.timestamp_millis_opt(timestamp_ms).single() else {
        return String::new();
    };
    let today = Local::now().date_naive();
    if dt.date_naive() == today {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d").to_string()
    }
}
