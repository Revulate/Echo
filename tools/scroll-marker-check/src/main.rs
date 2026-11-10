mod settings {
    pub fn get_thread_highlight_show_in_mentions() -> bool {
        false
    }
}

mod models {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum HighlightType {
        Whisper,
        Mention,
        User {
            username: String,
        },
        ParticipatedReply {
            username: String,
        },
        Badge {
            badge: String,
        },
        HighlightedMessage,
        FirstMessage,
        ReturningChatter,
        Subscription,
        Raid,
        BitsBadge,
        Announcement {
            color: Option<String>,
        },
        WatchStreak,
        Phrase {
            pattern: String,
        },
        CustomPhrase {
            pattern: String,
            color: Option<String>,
        },
        CustomUser {
            username: String,
            color: Option<String>,
        },
        CustomBadge {
            badge: String,
            color: Option<String>,
        },
        SelfMessage,
        Elevated,
        ChannelPointRedeem {
            reward_id: String,
            reward_title: String,
            reward_cost: u32,
        },
    }
}

#[path = "../../../src/backend/scroll_markers.rs"]
mod scroll_markers;

use models::HighlightType;
use scroll_markers::{
    ScrollMarkerInput, ScrollMarkerKind, ScrollMarkerPriority, classify_scroll_marker,
    is_moderation_system_message,
};

fn kind_name(kind: ScrollMarkerKind) -> &'static str {
    match kind {
        ScrollMarkerKind::SearchResult => "search-result",
        ScrollMarkerKind::Moderation => "moderation",
        ScrollMarkerKind::Mention => "mention",
        ScrollMarkerKind::Whisper => "whisper",
        ScrollMarkerKind::Raid => "raid",
        ScrollMarkerKind::Subscription => "subscription",
        ScrollMarkerKind::Announcement => "announcement",
        ScrollMarkerKind::WatchStreak => "watch-streak",
        ScrollMarkerKind::HighlightedMessage => "highlighted-message",
        ScrollMarkerKind::ChannelPointRedeem => "channel-point-redeem",
        ScrollMarkerKind::FirstMessage => "first-message",
        ScrollMarkerKind::ReturningChatter => "returning-chatter",
        ScrollMarkerKind::BitsBadge => "bits-badge",
    }
}

fn priority_name(priority: ScrollMarkerPriority) -> &'static str {
    match priority {
        ScrollMarkerPriority::Low => "low",
        ScrollMarkerPriority::Medium => "medium",
        ScrollMarkerPriority::High => "high",
    }
}

fn classify(input: ScrollMarkerInput<'_>) -> String {
    classify_scroll_marker(input)
        .map(|decision| {
            format!(
                "{},{}",
                kind_name(decision.kind),
                priority_name(decision.priority)
            )
        })
        .unwrap_or_else(|| "none,none".to_string())
}

fn assert_case(name: &str, input: ScrollMarkerInput<'_>, expected: &str) {
    let actual = classify(input);
    assert_eq!(actual, expected, "scroll marker mismatch for {name}");
    println!("{name},{actual}");
}

fn main() {
    println!("case,kind,priority");

    assert_case(
        "search_over_moderation",
        ScrollMarkerInput {
            content: "chat was cleared",
            is_system: true,
            is_search_result: true,
            ..Default::default()
        },
        "search-result,high",
    );
    assert_case(
        "deleted_message_unmarked",
        ScrollMarkerInput {
            content: "message a moderator deleted",
            ..Default::default()
        },
        "none,none",
    );
    assert_case(
        "ban_system_row",
        ScrollMarkerInput {
            content: "target was permanently banned by moderator",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "timeout_system_row",
        ScrollMarkerInput {
            content: "target was timed out for 600 seconds",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "clear_chat_system_row",
        ScrollMarkerInput {
            content: "chat was cleared by a moderator",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "unban_system_row",
        ScrollMarkerInput {
            content: "target was unbanned by moderator",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "automod_hold_system_row",
        ScrollMarkerInput {
            content: "viewer had a message held by AutoMod: blocked term",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "automod_update_system_row",
        ScrollMarkerInput {
            content: "AutoMod message from viewer was allowed",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "suspicious_user_system_row",
        ScrollMarkerInput {
            content: "viewer (restricted) sent a suspicious-user message: hello",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "vip_add_system_row",
        ScrollMarkerInput {
            content: "viewer was added as a VIP.",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "moderator_remove_system_row",
        ScrollMarkerInput {
            content: "viewer was removed as a moderator.",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "warning_system_row",
        ScrollMarkerInput {
            content: "viewer was warned by moderator: stop",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "shield_mode_system_row",
        ScrollMarkerInput {
            content: "Shield Mode was activated by moderator.",
            is_system: true,
            ..Default::default()
        },
        "moderation,high",
    );
    assert_case(
        "moderation_text_in_user_row_is_not_marked",
        ScrollMarkerInput {
            content: "lol he was banned by the mods yesterday",
            ..Default::default()
        },
        "none,none",
    );
    assert_case(
        "mention_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Mention),
            ..Default::default()
        },
        "mention,high",
    );
    assert_case(
        "whisper_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Whisper),
            ..Default::default()
        },
        "whisper,high",
    );
    assert_case(
        "raid_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Raid),
            ..Default::default()
        },
        "raid,high",
    );
    assert_case(
        "subscription_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Subscription),
            ..Default::default()
        },
        "subscription,medium",
    );
    assert_case(
        "announcement_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Announcement { color: None }),
            ..Default::default()
        },
        "announcement,medium",
    );
    assert_case(
        "watch_streak_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::WatchStreak),
            ..Default::default()
        },
        "watch-streak,medium",
    );
    assert_case(
        "highlighted_message",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::HighlightedMessage),
            ..Default::default()
        },
        "highlighted-message,medium",
    );
    assert_case(
        "channel_point_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::ChannelPointRedeem {
                reward_id: "reward".to_string(),
                reward_title: "Hydrate".to_string(),
                reward_cost: 100,
            }),
            ..Default::default()
        },
        "channel-point-redeem,medium",
    );
    assert_case(
        "bits_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::BitsBadge),
            ..Default::default()
        },
        "bits-badge,low",
    );
    assert_case(
        "first_message_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::FirstMessage),
            ..Default::default()
        },
        "first-message,low",
    );
    assert_case(
        "returning_chatter_highlight",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::ReturningChatter),
            ..Default::default()
        },
        "returning-chatter,low",
    );
    // Phrase rules get their own highlight mark (every highlighted message
    // shows on the scrollbar) instead of falling through to the mention scan.
    // Highlight priority already prefers Mention over Phrase when both match,
    // so a Phrase-typed row is one where the mention rule did not win.
    assert_case(
        "phrase_highlight_gets_own_mark",
        ScrollMarkerInput {
            content: "hello EchoUser",
            highlight_type: Some(&HighlightType::Phrase {
                pattern: "hello".to_string(),
            }),
            our_username: Some("echouser"),
            ..Default::default()
        },
        "highlighted-message,medium",
    );
    assert_case(
        "participated_reply_respects_disabled_setting",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::ParticipatedReply {
                username: "thread-author".to_string(),
            }),
            ..Default::default()
        },
        "none,none",
    );
    for (name, highlight_type) in [
        (
            "custom_phrase_highlight_gets_own_mark",
            HighlightType::CustomPhrase {
                pattern: "needle".to_string(),
                color: Some("#ff0000".to_string()),
            },
        ),
        (
            "custom_user_highlight_gets_own_mark",
            HighlightType::CustomUser {
                username: "viewer".to_string(),
                color: None,
            },
        ),
        (
            "custom_badge_highlight_gets_own_mark",
            HighlightType::CustomBadge {
                badge: "vip".to_string(),
                color: None,
            },
        ),
        ("elevated_message_gets_own_mark", HighlightType::Elevated),
    ] {
        assert_case(
            name,
            ScrollMarkerInput {
                highlight_type: Some(&highlight_type),
                ..Default::default()
            },
            "highlighted-message,medium",
        );
    }
    assert_case(
        "first_message_fallback",
        ScrollMarkerInput {
            is_first_message: true,
            ..Default::default()
        },
        "first-message,low",
    );
    assert_case(
        "user_highlight_gets_own_mark",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::User {
                username: "someone".to_string(),
            }),
            ..Default::default()
        },
        "highlighted-message,medium",
    );
    assert_case(
        "badge_highlight_gets_own_mark",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::Badge {
                badge: "vip".to_string(),
            }),
            ..Default::default()
        },
        "highlighted-message,medium",
    );
    assert_case(
        "self_message_no_marker",
        ScrollMarkerInput {
            highlight_type: Some(&HighlightType::SelfMessage),
            ..Default::default()
        },
        "none,none",
    );

    assert!(is_moderation_system_message("user WAS DELETED by a mod"));
    assert!(is_moderation_system_message(
        "AutoMod message from user was denied"
    ));
    assert!(is_moderation_system_message("Shield Mode was deactivated."));
    assert!(!is_moderation_system_message("ordinary chat message"));
}
