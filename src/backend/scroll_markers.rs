//! Shared scroll-map marker classification for chat rows.

use super::models::HighlightType;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScrollMarkerPriority {
    /// Low priority: first messages, returning chatters.
    Low = 0,
    /// Medium priority: subscriptions, announcements.
    Medium = 1,
    /// High priority: mentions, whispers, raids, moderation, search hits.
    High = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMarkerKind {
    SearchResult,
    Moderation,
    Mention,
    Whisper,
    Raid,
    Subscription,
    Announcement,
    WatchStreak,
    HighlightedMessage,
    ChannelPointRedeem,
    FirstMessage,
    ReturningChatter,
    BitsBadge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollMarkerDecision {
    pub kind: ScrollMarkerKind,
    pub priority: ScrollMarkerPriority,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollMarkerInput<'a> {
    pub content: &'a str,
    /// Row carries the system-message flag. The moderation needle scan is only
    /// meaningful (and only affordable) for system rows.
    pub is_system: bool,
    pub is_first_message: bool,
    pub highlight_type: Option<&'a HighlightType>,
    pub our_username: Option<&'a str>,
    pub is_search_result: bool,
}

pub fn classify_scroll_marker(input: ScrollMarkerInput<'_>) -> Option<ScrollMarkerDecision> {
    if input.is_search_result {
        return Some(marker(
            ScrollMarkerKind::SearchResult,
            ScrollMarkerPriority::High,
        ));
    }

    if input.is_system && is_moderation_system_message(input.content) {
        return Some(marker(
            ScrollMarkerKind::Moderation,
            ScrollMarkerPriority::High,
        ));
    }

    if let Some(highlight_type) = input.highlight_type
        && let Some(decision) = classify_highlight_marker(highlight_type)
    {
        return Some(decision);
    }

    if input
        .our_username
        .map(|username| contains_ignore_ascii_case(input.content, username))
        .unwrap_or(false)
    {
        Some(marker(
            ScrollMarkerKind::Mention,
            ScrollMarkerPriority::High,
        ))
    } else if input.is_first_message {
        Some(marker(
            ScrollMarkerKind::FirstMessage,
            ScrollMarkerPriority::Low,
        ))
    } else {
        None
    }
}

pub fn is_moderation_system_message(content: &str) -> bool {
    contains_ignore_ascii_case(content, "was deleted")
        || contains_ignore_ascii_case(content, "was banned by")
        || contains_ignore_ascii_case(content, "was permanently banned")
        || contains_ignore_ascii_case(content, "was timed out")
        || contains_ignore_ascii_case(content, "was unbanned")
        || contains_ignore_ascii_case(content, "chat was cleared")
        || contains_ignore_ascii_case(content, "held by automod")
        || contains_ignore_ascii_case(content, "automod message")
        || contains_ignore_ascii_case(content, "suspicious-user")
        || contains_ignore_ascii_case(content, "suspicious user")
        || contains_ignore_ascii_case(content, "was added as a vip")
        || contains_ignore_ascii_case(content, "was removed as a vip")
        || contains_ignore_ascii_case(content, "was added as a moderator")
        || contains_ignore_ascii_case(content, "was removed as a moderator")
        || contains_ignore_ascii_case(content, "acknowledged a warning")
        || contains_ignore_ascii_case(content, "was warned")
        || contains_ignore_ascii_case(content, "shield mode was activated")
        || contains_ignore_ascii_case(content, "shield mode was deactivated")
}

fn classify_highlight_marker(highlight_type: &HighlightType) -> Option<ScrollMarkerDecision> {
    match highlight_type {
        HighlightType::Mention => Some(marker(
            ScrollMarkerKind::Mention,
            ScrollMarkerPriority::High,
        )),
        HighlightType::Whisper => Some(marker(
            ScrollMarkerKind::Whisper,
            ScrollMarkerPriority::High,
        )),
        HighlightType::Raid => Some(marker(ScrollMarkerKind::Raid, ScrollMarkerPriority::High)),
        HighlightType::Subscription => Some(marker(
            ScrollMarkerKind::Subscription,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::Announcement { .. } => Some(marker(
            ScrollMarkerKind::Announcement,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::Elevated => Some(marker(
            ScrollMarkerKind::HighlightedMessage,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::WatchStreak => Some(marker(
            ScrollMarkerKind::WatchStreak,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::HighlightedMessage => Some(marker(
            ScrollMarkerKind::HighlightedMessage,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::ChannelPointRedeem { .. } => Some(marker(
            ScrollMarkerKind::ChannelPointRedeem,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::FirstMessage => Some(marker(
            ScrollMarkerKind::FirstMessage,
            ScrollMarkerPriority::Low,
        )),
        HighlightType::ReturningChatter => Some(marker(
            ScrollMarkerKind::ReturningChatter,
            ScrollMarkerPriority::Low,
        )),
        HighlightType::BitsBadge => Some(marker(
            ScrollMarkerKind::BitsBadge,
            ScrollMarkerPriority::Low,
        )),
        HighlightType::ParticipatedReply { .. } => {
            super::settings::get_thread_highlight_show_in_mentions().then_some(marker(
                ScrollMarkerKind::Mention,
                ScrollMarkerPriority::High,
            ))
        }
        HighlightType::User { .. }
        | HighlightType::Badge { .. }
        | HighlightType::CustomPhrase { .. }
        | HighlightType::CustomUser { .. }
        | HighlightType::CustomBadge { .. }
        | HighlightType::Phrase { .. } => Some(marker(
            ScrollMarkerKind::HighlightedMessage,
            ScrollMarkerPriority::Medium,
        )),
        HighlightType::SelfMessage => None,
    }
}

fn marker(kind: ScrollMarkerKind, priority: ScrollMarkerPriority) -> ScrollMarkerDecision {
    ScrollMarkerDecision { kind, priority }
}

#[inline]
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }

    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}
