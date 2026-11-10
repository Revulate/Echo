//! Shared highlight-priority decisions for incoming chat rows.

use super::models::HighlightType;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivmsgHighlightInput {
    pub account_name_mention: bool,
    pub configured_highlight: Option<HighlightType>,
    pub configured_badge_highlight: Option<HighlightType>,
    pub reply_thread_username: Option<String>,
    pub filter_matched_text: Option<String>,
    pub filter_highlighted: bool,
    pub cheer: bool,
    pub first_message: bool,
    pub returning_chatter: bool,
}

pub fn select_privmsg_highlight(input: PrivmsgHighlightInput) -> Option<HighlightType> {
    if input.account_name_mention {
        return Some(HighlightType::Mention);
    }

    if let Some(highlight) = input.configured_highlight {
        return Some(highlight);
    }

    if let Some(pattern) = input
        .filter_matched_text
        .filter(|pattern| !pattern.trim().is_empty())
    {
        return Some(HighlightType::Phrase { pattern });
    }

    if input.filter_highlighted {
        return Some(HighlightType::HighlightedMessage);
    }

    if super::settings::get_thread_highlight_enabled()
        && let Some(username) = input
            .reply_thread_username
            .filter(|username| !username.trim().is_empty())
    {
        return Some(HighlightType::ParticipatedReply { username });
    }

    if let Some(highlight) = input.configured_badge_highlight {
        return Some(highlight);
    }

    if input.cheer {
        Some(HighlightType::BitsBadge)
    } else if input.first_message {
        Some(HighlightType::FirstMessage)
    } else if input.returning_chatter {
        Some(HighlightType::ReturningChatter)
    } else {
        None
    }
}
