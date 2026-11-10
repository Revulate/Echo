//! Pure Twitch USERNOTICE classification helpers.

use crate::backend::slots::MessageSlot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernoticeHighlightKind {
    Subscription,
    Raid,
    Announcement,
    BitsBadge,
    WatchStreak,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsernoticeTags {
    pub msg_id: String,
    pub ritual_name: Option<String>,
    pub watch_streak_months: Option<u32>,
    pub watch_streak: Option<String>,
    pub milestone_category: Option<String>,
    pub milestone_value: Option<u32>,
    pub modiversary_months: Option<u32>,
}

/// Split a USERNOTICE system message ("KIWI_CHIPS subscribed at Tier 1.")
/// into slots where each standalone occurrence of a known display name
/// becomes a Mention slot (rendered colored + clickable, without '@').
/// `names` = [(display_name)] for sender and (when present) gift recipient.
pub fn usernotice_system_slots(text: &str, names: &[&str]) -> Vec<MessageSlot> {
    let names: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() {
        return vec![MessageSlot::text(text)];
    }

    let mut slots = Vec::new();
    let mut text_start = 0;
    let mut chars = text.char_indices().peekable();

    while let Some((word_start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }

        let mut word_end = word_start + ch.len_utf8();
        while let Some(&(index, next)) = chars.peek() {
            if next.is_whitespace() {
                break;
            }
            chars.next();
            word_end = index + next.len_utf8();
        }

        let word = &text[word_start..word_end];
        let matched_name = names.iter().copied().find(|name| {
            word == *name
                || (word.len() == name.len() + 1
                    && word.starts_with(*name)
                    && matches!(word.as_bytes().last(), Some(b'!' | b'.' | b',' | b':')))
        });

        let Some(name) = matched_name else {
            continue;
        };

        if text_start < word_start {
            slots.push(MessageSlot::text(&text[text_start..word_start]));
        }
        slots.push(MessageSlot::Mention {
            username: name.into(),
        });
        text_start = word_start + name.len();
    }

    if text_start < text.len() {
        slots.push(MessageSlot::text(&text[text_start..]));
    }

    if slots.is_empty() {
        vec![MessageSlot::text(text)]
    } else {
        slots
    }
}

pub fn event_type_label(msg_id: &str) -> &'static str {
    match msg_id {
        "sub" | "resub" => "subscription",
        "subgift" | "anonsubgift" | "submysterygift" => "gift subscription",
        "giftpaidupgrade" | "anongiftpaidupgrade" | "primepaidupgrade" => "subscription upgrade",
        "communitypayforward" => "pay it forward",
        "rewardgift" => "gift reward",
        "raid" => "raid",
        "unraid" => "raid ended",
        "ritual" => "ritual",
        "announcement" => "announcement",
        "bitsbadgetier" => "bits badge",
        "watch-streak" | "watchstreak" => "watch streak",
        "viewermilestone" => "viewer milestone",
        "modiversary" => "modiversary",
        _ => "notice",
    }
}

pub fn is_subscription_usernotice(msg_id: &str) -> bool {
    matches!(
        msg_id,
        "sub"
            | "resub"
            | "subgift"
            | "anonsubgift"
            | "submysterygift"
            | "giftpaidupgrade"
            | "anongiftpaidupgrade"
            | "primepaidupgrade"
            | "communitypayforward"
            | "rewardgift"
            | "modiversary"
    )
}

pub fn is_watch_streak_usernotice(tags: &UsernoticeTags) -> bool {
    if matches!(tags.msg_id.as_str(), "watch-streak" | "watchstreak") {
        return true;
    }

    if tags.msg_id == "viewermilestone" {
        return tags
            .milestone_category
            .as_deref()
            .is_some_and(|category| category.eq_ignore_ascii_case("watch-streak"))
            && tags.milestone_value.is_some_and(|value| value > 0);
    }

    tags.ritual_name
        .as_deref()
        .map(is_watch_streak_label)
        .unwrap_or(false)
        || tags
            .watch_streak_months
            .map(|months| months > 0)
            .unwrap_or(false)
        || tags
            .watch_streak
            .as_deref()
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

pub fn highlight_kind(tags: &UsernoticeTags) -> Option<UsernoticeHighlightKind> {
    if is_subscription_usernotice(&tags.msg_id) {
        Some(UsernoticeHighlightKind::Subscription)
    } else if tags.msg_id == "raid" {
        Some(UsernoticeHighlightKind::Raid)
    } else if tags.msg_id == "announcement" {
        Some(UsernoticeHighlightKind::Announcement)
    } else if tags.msg_id == "bitsbadgetier" {
        Some(UsernoticeHighlightKind::BitsBadge)
    } else if is_watch_streak_usernotice(tags) {
        Some(UsernoticeHighlightKind::WatchStreak)
    } else {
        None
    }
}

fn is_watch_streak_label(value: &str) -> bool {
    value.eq_ignore_ascii_case("watch-streak") || value.eq_ignore_ascii_case("watchstreak")
}
