//! Plain-language reasons for a chat row's highlight.
//!
//! Used by the chat context menu ("Why is this highlighted?") and the
//! Highlights tester, so both describe a highlight the same way.

use crate::message::HighlightType;

/// Why a row is highlighted, in words a viewer understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightExplanation {
    /// Short name of the highlight kind, matching the Highlights color names.
    pub kind: &'static str,
    /// One-line reason, e.g. "Mentions your username".
    pub title: String,
    /// Extra context when it helps.
    pub detail: Option<String>,
}

/// Which kind of custom highlight rule produced a highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightRuleKind {
    Phrase,
    User,
    Badge,
}

/// The custom highlight rule a highlight came from, as recorded on the row.
///
/// Rows do not store a rule id. `value` is the rule's pattern for a phrase
/// rule, the sender login a user rule matched, or the `name/version` badge a
/// badge rule matched, and `color` is the rule's color when it matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightRuleOrigin<'a> {
    pub kind: HighlightRuleKind,
    pub value: &'a str,
    pub color: Option<&'a str>,
}

const FROM_YOUR_RULES: &str = "From your custom highlight rules in Highlights.";
const FILTER_DETAIL: &str = "One of your filters with the Highlight action matched this message.";

/// Explain `highlight` in plain language.
pub fn explain_highlight(highlight: &HighlightType) -> HighlightExplanation {
    let (kind, title, detail): (&'static str, String, Option<String>) = match highlight {
        HighlightType::Whisper => (
            "Whisper",
            "Private whisper".to_string(),
            Some("A private message between you and one other user.".to_string()),
        ),
        HighlightType::Mention => (
            "Mention",
            "Mentions your username".to_string(),
            Some(
                "It contains your username, or replies to one of your messages.".to_string(),
            ),
        ),
        HighlightType::User { username } => (
            "Highlighted user",
            labelled("From a user you highlight", username),
            None,
        ),
        HighlightType::ParticipatedReply { username } => (
            "Reply thread",
            "Reply in a thread you took part in".to_string(),
            non_empty(username).map(|username| {
                format!("You ({username}) posted in this reply thread, and thread highlights are on.")
            }),
        ),
        HighlightType::Badge { badge } => (
            "Highlighted badge",
            labelled("Sender has a badge you highlight", badge_name(badge)),
            non_empty(badge).map(|badge| format!("Matched the badge {badge}.")),
        ),
        HighlightType::HighlightedMessage => (
            "Filter highlight",
            "Matches a filter set to highlight".to_string(),
            Some(FILTER_DETAIL.to_string()),
        ),
        HighlightType::FirstMessage => (
            "First message",
            "First message in this channel".to_string(),
            Some("Twitch marks this as the sender's first message here.".to_string()),
        ),
        HighlightType::ReturningChatter => (
            "Returning chatter",
            "Returning chatter".to_string(),
            Some(
                "Twitch marks the sender as a returning chatter.".to_string(),
            ),
        ),
        HighlightType::Subscription => (
            "Subscription",
            "Subscription".to_string(),
            Some("A new subscription, resubscription or gifted subscription.".to_string()),
        ),
        HighlightType::Raid => (
            "Raid",
            "Raid".to_string(),
            Some("Another channel sent its viewers here.".to_string()),
        ),
        HighlightType::BitsBadge => (
            "Bits",
            "Bits cheer or new Bits badge".to_string(),
            Some(
                "The message cheers Bits, or the sender unlocked a new Bits badge.".to_string(),
            ),
        ),
        HighlightType::Announcement { color } => (
            "Announcement",
            "Announcement".to_string(),
            Some(match announcement_color(color.as_deref()) {
                Some(color) => format!(
                    "The broadcaster or a moderator posted this with /announce, in {color}."
                ),
                None => "The broadcaster or a moderator posted this with /announce.".to_string(),
            }),
        ),
        HighlightType::WatchStreak => (
            "Watch streak",
            "Watch streak".to_string(),
            Some("The sender shared a watch streak in this channel.".to_string()),
        ),
        HighlightType::Phrase { pattern } => (
            "Filter highlight",
            "Matches a filter set to highlight".to_string(),
            Some(match non_empty(pattern) {
                Some(pattern) => format!("A filter with the Highlight action matched `{pattern}`."),
                None => FILTER_DETAIL.to_string(),
            }),
        ),
        HighlightType::CustomPhrase { pattern, .. } => (
            "Highlight phrase",
            match non_empty(pattern) {
                Some(pattern) => format!("Matches your highlight phrase `{pattern}`"),
                None => "Matches one of your highlight phrases".to_string(),
            },
            Some(FROM_YOUR_RULES.to_string()),
        ),
        HighlightType::CustomUser { username, .. } => (
            "Highlighted user",
            labelled("From a user you highlight", username),
            Some(FROM_YOUR_RULES.to_string()),
        ),
        HighlightType::CustomBadge { badge, .. } => (
            "Highlighted badge",
            labelled("Sender has a badge you highlight", badge_name(badge)),
            Some(match non_empty(badge) {
                Some(badge) => format!("Matched the badge {badge}. {FROM_YOUR_RULES}"),
                None => FROM_YOUR_RULES.to_string(),
            }),
        ),
        HighlightType::SelfMessage => (
            "Your own message",
            "Your own message".to_string(),
            Some("\"Highlight your own messages\" is on in Highlights.".to_string()),
        ),
        HighlightType::Elevated => (
            "Elevated / Hype Chat",
            "Elevated or Hype Chat message".to_string(),
            Some(
                "The sender paid to make this message stand out, and \"Highlight elevated / Hype Chat\" is on."
                    .to_string(),
            ),
        ),
        HighlightType::ChannelPointRedeem {
            reward_title,
            reward_cost,
            ..
        } => (
            "Channel point redeem",
            match (non_empty(reward_title), reward_cost) {
                (Some(title), 0) => format!("Channel point reward: {title}"),
                (Some(title), cost) => format!("Channel point reward: {title} ({cost})"),
                (None, _) => "Channel point reward".to_string(),
            },
            Some("Someone redeemed this reward with channel points.".to_string()),
        ),
    };
    HighlightExplanation {
        kind,
        title,
        detail,
    }
}

/// The custom highlight rule behind `highlight`, when a rule produced it.
pub fn highlight_rule_origin(highlight: &HighlightType) -> Option<HighlightRuleOrigin<'_>> {
    let (kind, value, color) = match highlight {
        HighlightType::CustomPhrase { pattern, color } => {
            (HighlightRuleKind::Phrase, pattern, color)
        }
        HighlightType::CustomUser { username, color } => (HighlightRuleKind::User, username, color),
        HighlightType::CustomBadge { badge, color } => (HighlightRuleKind::Badge, badge, color),
        _ => return None,
    };
    Some(HighlightRuleOrigin {
        kind,
        value: value.as_str(),
        color: color.as_deref(),
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn labelled(label: &str, value: &str) -> String {
    match non_empty(value) {
        Some(value) => format!("{label}: {value}"),
        None => label.to_string(),
    }
}

/// `moderator/1` -> `moderator`; Twitch badge keys pair a name with a version.
fn badge_name(badge: &str) -> &str {
    badge
        .split_once('/')
        .or_else(|| badge.split_once(':'))
        .map_or(badge, |(name, _)| name)
}

/// Helix announcement colors, in words; the default `PRIMARY` needs no mention.
fn announcement_color(color: Option<&str>) -> Option<String> {
    let color = non_empty(color?)?;
    (!color.eq_ignore_ascii_case("primary")).then(|| color.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explain(highlight: HighlightType) -> HighlightExplanation {
        explain_highlight(&highlight)
    }

    /// Fails to compile when a variant is added, so the table below grows too.
    fn variant_count_guard(highlight: &HighlightType) -> u8 {
        match highlight {
            HighlightType::Whisper => 0,
            HighlightType::Mention => 1,
            HighlightType::User { .. } => 2,
            HighlightType::ParticipatedReply { .. } => 3,
            HighlightType::Badge { .. } => 4,
            HighlightType::HighlightedMessage => 5,
            HighlightType::FirstMessage => 6,
            HighlightType::ReturningChatter => 7,
            HighlightType::Subscription => 8,
            HighlightType::Raid => 9,
            HighlightType::BitsBadge => 10,
            HighlightType::Announcement { .. } => 11,
            HighlightType::WatchStreak => 12,
            HighlightType::Phrase { .. } => 13,
            HighlightType::CustomPhrase { .. } => 14,
            HighlightType::CustomUser { .. } => 15,
            HighlightType::CustomBadge { .. } => 16,
            HighlightType::SelfMessage => 17,
            HighlightType::Elevated => 18,
            HighlightType::ChannelPointRedeem { .. } => 19,
        }
    }

    fn every_variant() -> Vec<HighlightType> {
        vec![
            HighlightType::Whisper,
            HighlightType::Mention,
            HighlightType::User {
                username: "xqc".into(),
            },
            HighlightType::ParticipatedReply {
                username: "revulate".into(),
            },
            HighlightType::Badge {
                badge: "moderator/1".into(),
            },
            HighlightType::HighlightedMessage,
            HighlightType::FirstMessage,
            HighlightType::ReturningChatter,
            HighlightType::Subscription,
            HighlightType::Raid,
            HighlightType::BitsBadge,
            HighlightType::Announcement { color: None },
            HighlightType::WatchStreak,
            HighlightType::Phrase {
                pattern: "pog".into(),
            },
            HighlightType::CustomPhrase {
                pattern: "pog+".into(),
                color: None,
            },
            HighlightType::CustomUser {
                username: "xqc".into(),
                color: Some("#ff0000".into()),
            },
            HighlightType::CustomBadge {
                badge: "moderator/1".into(),
                color: None,
            },
            HighlightType::SelfMessage,
            HighlightType::Elevated,
            HighlightType::ChannelPointRedeem {
                reward_id: "id".into(),
                reward_title: "Hydrate".into(),
                reward_cost: 500,
            },
        ]
    }

    #[test]
    fn every_variant_has_a_plain_language_title() {
        let variants = every_variant();
        let mut seen: Vec<u8> = variants.iter().map(variant_count_guard).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 20, "every variant appears exactly once");

        for highlight in variants {
            let explanation = explain_highlight(&highlight);
            assert!(!explanation.title.trim().is_empty(), "{highlight:?}");
            assert!(!explanation.kind.trim().is_empty(), "{highlight:?}");
            for internal in [
                "CustomPhrase",
                "CustomUser",
                "CustomBadge",
                "HighlightedMessage",
                "ParticipatedReply",
                "BitsBadge",
                "SelfMessage",
            ] {
                assert!(!explanation.title.contains(internal), "{highlight:?}");
                assert!(
                    !explanation
                        .detail
                        .as_deref()
                        .unwrap_or_default()
                        .contains(internal),
                    "{highlight:?}"
                );
            }
        }
    }

    #[test]
    fn whisper() {
        let explanation = explain(HighlightType::Whisper);
        assert_eq!(explanation.kind, "Whisper");
        assert_eq!(explanation.title, "Private whisper");
        let detail = explanation.detail.unwrap();
        assert!(
            !detail.contains("sent you"),
            "own whispers are Whisper rows too"
        );
    }

    #[test]
    fn mention() {
        let explanation = explain(HighlightType::Mention);
        assert_eq!(explanation.kind, "Mention");
        assert_eq!(explanation.title, "Mentions your username");
        // History rows match the login as a substring, so the detail must not
        // promise a display-name or whole-word match.
        assert_eq!(
            explanation.detail.as_deref(),
            Some("It contains your username, or replies to one of your messages.")
        );
    }

    #[test]
    fn legacy_user() {
        let explanation = explain(HighlightType::User {
            username: "xqc".into(),
        });
        assert_eq!(explanation.title, "From a user you highlight: xqc");
        assert_eq!(explanation.detail, None);
    }

    #[test]
    fn participated_reply() {
        let explanation = explain(HighlightType::ParticipatedReply {
            username: "revulate".into(),
        });
        assert_eq!(explanation.kind, "Reply thread");
        assert_eq!(explanation.title, "Reply in a thread you took part in");
        assert!(explanation.detail.unwrap().contains("revulate"));
        let without_name = explain(HighlightType::ParticipatedReply {
            username: " ".into(),
        });
        assert_eq!(without_name.detail, None);
    }

    #[test]
    fn legacy_badge() {
        let explanation = explain(HighlightType::Badge {
            badge: "moderator/1".into(),
        });
        assert_eq!(
            explanation.title,
            "Sender has a badge you highlight: moderator"
        );
        assert_eq!(
            explanation.detail.as_deref(),
            Some("Matched the badge moderator/1.")
        );
    }

    #[test]
    fn highlighted_message_is_a_filter() {
        let explanation = explain(HighlightType::HighlightedMessage);
        assert_eq!(explanation.kind, "Filter highlight");
        assert_eq!(explanation.title, "Matches a filter set to highlight");
        assert_eq!(explanation.detail.as_deref(), Some(FILTER_DETAIL));
    }

    #[test]
    fn first_message() {
        let explanation = explain(HighlightType::FirstMessage);
        assert_eq!(explanation.title, "First message in this channel");
    }

    #[test]
    fn returning_chatter() {
        let explanation = explain(HighlightType::ReturningChatter);
        assert_eq!(explanation.title, "Returning chatter");
        assert_eq!(
            explanation.detail.as_deref(),
            Some("Twitch marks the sender as a returning chatter.")
        );
    }

    #[test]
    fn subscription() {
        let explanation = explain(HighlightType::Subscription);
        assert_eq!(explanation.title, "Subscription");
        assert!(explanation.detail.unwrap().contains("gifted"));
    }

    #[test]
    fn raid() {
        let explanation = explain(HighlightType::Raid);
        assert_eq!(explanation.title, "Raid");
    }

    #[test]
    fn bits() {
        let explanation = explain(HighlightType::BitsBadge);
        assert_eq!(explanation.kind, "Bits");
        assert_eq!(explanation.title, "Bits cheer or new Bits badge");
    }

    #[test]
    fn announcement_mentions_only_non_default_colors() {
        let plain = explain(HighlightType::Announcement { color: None });
        assert_eq!(plain.title, "Announcement");
        assert_eq!(
            plain.detail.as_deref(),
            Some("The broadcaster or a moderator posted this with /announce.")
        );
        let primary = explain(HighlightType::Announcement {
            color: Some("PRIMARY".into()),
        });
        assert_eq!(primary.detail, plain.detail);
        let blue = explain(HighlightType::Announcement {
            color: Some("BLUE".into()),
        });
        assert_eq!(
            blue.detail.as_deref(),
            Some("The broadcaster or a moderator posted this with /announce, in blue.")
        );
    }

    #[test]
    fn watch_streak() {
        let explanation = explain(HighlightType::WatchStreak);
        assert_eq!(explanation.title, "Watch streak");
    }

    #[test]
    fn filter_phrase_names_the_matched_text() {
        let explanation = explain(HighlightType::Phrase {
            pattern: "pog".into(),
        });
        assert_eq!(explanation.kind, "Filter highlight");
        assert_eq!(explanation.title, "Matches a filter set to highlight");
        assert_eq!(
            explanation.detail.as_deref(),
            Some("A filter with the Highlight action matched `pog`.")
        );
    }

    #[test]
    fn custom_phrase() {
        let explanation = explain(HighlightType::CustomPhrase {
            pattern: "pog+".into(),
            color: Some("#123456".into()),
        });
        assert_eq!(explanation.kind, "Highlight phrase");
        assert_eq!(explanation.title, "Matches your highlight phrase `pog+`");
        assert_eq!(explanation.detail.as_deref(), Some(FROM_YOUR_RULES));
    }

    #[test]
    fn custom_user() {
        let explanation = explain(HighlightType::CustomUser {
            username: "xqc".into(),
            color: None,
        });
        assert_eq!(explanation.title, "From a user you highlight: xqc");
    }

    #[test]
    fn custom_badge() {
        let explanation = explain(HighlightType::CustomBadge {
            badge: "subscriber/12".into(),
            color: None,
        });
        assert_eq!(
            explanation.title,
            "Sender has a badge you highlight: subscriber"
        );
        assert!(
            explanation
                .detail
                .unwrap()
                .starts_with("Matched the badge subscriber/12.")
        );
    }

    #[test]
    fn self_message() {
        let explanation = explain(HighlightType::SelfMessage);
        assert_eq!(explanation.title, "Your own message");
    }

    #[test]
    fn elevated() {
        let explanation = explain(HighlightType::Elevated);
        assert_eq!(explanation.title, "Elevated or Hype Chat message");
    }

    #[test]
    fn channel_point_redeem() {
        let explanation = explain(HighlightType::ChannelPointRedeem {
            reward_id: "abc".into(),
            reward_title: "Hydrate".into(),
            reward_cost: 500,
        });
        assert_eq!(explanation.title, "Channel point reward: Hydrate (500)");
        let untitled = explain(HighlightType::ChannelPointRedeem {
            reward_id: "abc".into(),
            reward_title: String::new(),
            reward_cost: 50,
        });
        assert_eq!(untitled.title, "Channel point reward");
        let free = explain(HighlightType::ChannelPointRedeem {
            reward_id: "abc".into(),
            reward_title: "Channel reward".into(),
            reward_cost: 0,
        });
        assert_eq!(free.title, "Channel point reward: Channel reward");
    }

    #[test]
    fn empty_values_fall_back_to_generic_titles() {
        assert_eq!(
            explain(HighlightType::CustomPhrase {
                pattern: String::new(),
                color: None,
            })
            .title,
            "Matches one of your highlight phrases"
        );
        assert_eq!(
            explain(HighlightType::CustomUser {
                username: String::new(),
                color: None,
            })
            .title,
            "From a user you highlight"
        );
    }

    #[test]
    fn only_custom_rules_have_a_rule_origin() {
        for highlight in every_variant() {
            let origin = highlight_rule_origin(&highlight);
            let custom = matches!(
                highlight,
                HighlightType::CustomPhrase { .. }
                    | HighlightType::CustomUser { .. }
                    | HighlightType::CustomBadge { .. }
            );
            assert_eq!(origin.is_some(), custom, "{highlight:?}");
        }
        assert_eq!(
            highlight_rule_origin(&HighlightType::CustomUser {
                username: "xqc".into(),
                color: Some("#ff0000".into()),
            }),
            Some(HighlightRuleOrigin {
                kind: HighlightRuleKind::User,
                value: "xqc",
                color: Some("#ff0000"),
            })
        );
        assert_eq!(
            highlight_rule_origin(&HighlightType::CustomBadge {
                badge: "moderator/1".into(),
                color: None,
            })
            .map(|origin| (origin.kind, origin.value)),
            Some((HighlightRuleKind::Badge, "moderator/1"))
        );
    }
}
