mod models {
    // Local stub mirroring the variant shapes of src/backend/models.rs
    // HighlightType that `select_privmsg_highlight` can return or accept via the
    // `configured_highlight` / `configured_badge_highlight` passthrough fields.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum HighlightType {
        Mention,
        User { username: String },
        ParticipatedReply { username: String },
        Badge { badge: String },
        HighlightedMessage,
        FirstMessage,
        ReturningChatter,
        BitsBadge,
        Phrase { pattern: String },
    }
}

// Crate-root stub for the settings lookup that highlight_priority.rs calls via
// `super::settings::get_thread_highlight_enabled`. Real signature in
// src/backend/settings.rs returns `bool`; thread highlighting is treated as
// enabled here so the ParticipatedReply branch is exercised.
mod settings {
    pub fn get_thread_highlight_enabled() -> bool {
        true
    }
}

#[path = "../../../src/backend/highlight_priority.rs"]
mod highlight_priority;

use highlight_priority::{PrivmsgHighlightInput, select_privmsg_highlight};
use models::HighlightType;

fn kind(highlight: Option<HighlightType>) -> String {
    match highlight {
        Some(HighlightType::Mention) => "mention".to_string(),
        Some(HighlightType::Phrase { pattern }) => format!("phrase:{pattern}"),
        Some(HighlightType::User { username }) => format!("user:{username}"),
        Some(HighlightType::ParticipatedReply { username }) => format!("reply:{username}"),
        Some(HighlightType::Badge { badge }) => format!("badge:{badge}"),
        Some(HighlightType::HighlightedMessage) => "highlighted".to_string(),
        Some(HighlightType::BitsBadge) => "bits".to_string(),
        Some(HighlightType::FirstMessage) => "first-message".to_string(),
        Some(HighlightType::ReturningChatter) => "returning-chatter".to_string(),
        None => "none".to_string(),
    }
}

fn main() {
    let cases = [
        // No signals -> no highlight.
        ("plain", PrivmsgHighlightInput::default(), "none"),
        // account_name_mention is checked first and wins over everything below.
        (
            "account_mention_over_all",
            PrivmsgHighlightInput {
                account_name_mention: true,
                configured_highlight: Some(HighlightType::HighlightedMessage),
                filter_matched_text: Some("kappa".to_string()),
                filter_highlighted: true,
                reply_thread_username: Some("echo_user".to_string()),
                configured_badge_highlight: Some(HighlightType::Badge {
                    badge: "vip".to_string(),
                }),
                cheer: true,
                first_message: true,
                returning_chatter: true,
            },
            "mention",
        ),
        // configured_highlight (2nd) beats filter/reply/badge/cheer below it.
        (
            "configured_highlight_over_filter",
            PrivmsgHighlightInput {
                configured_highlight: Some(HighlightType::User {
                    username: "streamer".to_string(),
                }),
                filter_matched_text: Some("kappa".to_string()),
                filter_highlighted: true,
                reply_thread_username: Some("echo_user".to_string()),
                cheer: true,
                ..Default::default()
            },
            "user:streamer",
        ),
        // filter_matched_text (3rd) -> Phrase, beats filter_highlighted/reply/cheer.
        (
            "phrase_over_highlighted",
            PrivmsgHighlightInput {
                filter_matched_text: Some("needle".to_string()),
                filter_highlighted: true,
                reply_thread_username: Some("echo_user".to_string()),
                cheer: true,
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "phrase:needle",
        ),
        // Blank filter_matched_text is trimmed to empty -> skipped; falls through
        // to filter_highlighted (4th) -> HighlightedMessage.
        (
            "blank_phrase_falls_to_highlighted",
            PrivmsgHighlightInput {
                filter_matched_text: Some("   ".to_string()),
                filter_highlighted: true,
                reply_thread_username: Some("echo_user".to_string()),
                cheer: true,
                ..Default::default()
            },
            "highlighted",
        ),
        // filter_highlighted (4th) beats reply/badge/cheer.
        (
            "highlighted_over_reply",
            PrivmsgHighlightInput {
                filter_highlighted: true,
                reply_thread_username: Some("echo_user".to_string()),
                cheer: true,
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "highlighted",
        ),
        // reply_thread_username (5th, thread highlight enabled) -> ParticipatedReply,
        // beats configured_badge_highlight/cheer below it.
        (
            "reply_over_badge",
            PrivmsgHighlightInput {
                reply_thread_username: Some("echo_user".to_string()),
                configured_badge_highlight: Some(HighlightType::Badge {
                    badge: "vip".to_string(),
                }),
                cheer: true,
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "reply:echo_user",
        ),
        // Blank reply username is trimmed to empty -> skipped; falls through to
        // configured_badge_highlight (6th).
        (
            "blank_reply_falls_to_badge",
            PrivmsgHighlightInput {
                reply_thread_username: Some("   ".to_string()),
                configured_badge_highlight: Some(HighlightType::Badge {
                    badge: "vip".to_string(),
                }),
                cheer: true,
                ..Default::default()
            },
            "badge:vip",
        ),
        // configured_badge_highlight (6th) beats cheer/first/returning.
        (
            "badge_over_bits",
            PrivmsgHighlightInput {
                configured_badge_highlight: Some(HighlightType::Badge {
                    badge: "founder".to_string(),
                }),
                cheer: true,
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "badge:founder",
        ),
        // cheer (7th) -> BitsBadge, beats first_message/returning_chatter.
        (
            "bits_over_first_message",
            PrivmsgHighlightInput {
                cheer: true,
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "bits",
        ),
        // first_message (8th) beats returning_chatter.
        (
            "first_over_returning",
            PrivmsgHighlightInput {
                first_message: true,
                returning_chatter: true,
                ..Default::default()
            },
            "first-message",
        ),
        // returning_chatter (9th) is the final fallback before None.
        (
            "returning_fallback",
            PrivmsgHighlightInput {
                returning_chatter: true,
                ..Default::default()
            },
            "returning-chatter",
        ),
    ];

    println!("case,highlight");
    for (name, input, expected) in cases {
        let actual = kind(select_privmsg_highlight(input));
        assert_eq!(actual, expected, "highlight priority mismatch for {name}");
        println!("{name},{actual}");
    }
}
