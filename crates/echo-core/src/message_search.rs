use crate::message::{ChatMessage, HighlightType, SystemMessageType};
use crate::search::SearchMessage;
use crate::slot::MessageSlot;
use std::borrow::Cow;

fn slot_search_text(message: &ChatMessage) -> Option<String> {
    if message.flags.is_system || message.slots.is_empty() {
        return None;
    }
    let text = message
        .slots
        .iter()
        .map(|slot| match slot {
            MessageSlot::Text(text) => text.to_string(),
            MessageSlot::Emote(emote) => emote.name.to_string(),
            MessageSlot::TwitchEmote { name, .. } => name.to_string(),
            MessageSlot::Url { url, .. } => url.to_string(),
            MessageSlot::Mention { username } => format!("@{username}"),
            MessageSlot::Emoji { emoji } => emoji.to_string(),
            MessageSlot::TwitchGif(gif) => gif.alt_text.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn badge_key_name(value: &str) -> &str {
    value
        .split_once('/')
        .or_else(|| value.split_once(':'))
        .map(|(name, _)| name)
        .unwrap_or(value)
}

fn badge_name_matches(name: &str, expected: &str) -> bool {
    name.eq_ignore_ascii_case(expected)
        || (is_lead_moderator_badge_name(name) && is_lead_moderator_badge_name(expected))
}

fn badge_key_matches(stored: &str, expected: &str) -> bool {
    badge_name_matches(stored, expected) || badge_name_matches(badge_key_name(stored), expected)
}

fn is_lead_moderator_badge_name(value: &str) -> bool {
    value.eq_ignore_ascii_case("lead_moderator") || value.eq_ignore_ascii_case("lead-moderator")
}

fn is_moderator_badge_key(value: &str) -> bool {
    let name = badge_key_name(value);
    name.eq_ignore_ascii_case("moderator") || is_lead_moderator_badge_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderator_badges_match_only_moderator_and_lead_aliases() {
        assert!(is_moderator_badge_key("moderator/1"));
        assert!(is_moderator_badge_key("LEAD-MODERATOR:1"));
        assert!(badge_key_matches("subscriber:12", "subscriber:12"));
        assert!(badge_key_matches("lead_moderator/1", "LEAD-MODERATOR"));
        for badge in ["broadcaster/1", "staff/1", "admin/1", "global-mod/1"] {
            assert!(!is_moderator_badge_key(badge), "{badge}");
        }
    }
}

impl SearchMessage for ChatMessage {
    fn content(&self) -> &str {
        &self.content
    }

    fn searchable_text(&self) -> Option<Cow<'_, str>> {
        slot_search_text(self).map(Cow::Owned)
    }

    fn sender(&self) -> &str {
        &self.sender
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn channel(&self) -> &str {
        &self.channel
    }

    fn has_link(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, MessageSlot::Url { .. }))
            || self.content.contains("http://")
            || self.content.contains("https://")
            || contains_ignore_ascii_case(&self.content, "www.")
    }

    fn has_emote(&self) -> bool {
        self.slots.iter().any(|slot| {
            matches!(
                slot,
                MessageSlot::Emote(_) | MessageSlot::TwitchEmote { .. }
            )
        })
    }

    fn has_emoji(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, MessageSlot::Emoji { .. }))
    }

    fn has_mention(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, MessageSlot::Mention { .. }))
    }

    fn has_any_badge(&self) -> bool {
        !self.badges.is_empty()
    }

    fn has_badge(&self, badge: &str) -> bool {
        self.badges
            .iter()
            .any(|stored| badge_key_matches(stored.as_ref(), badge))
    }

    fn has_subtier(&self, tier: &str) -> bool {
        self.badges.iter().any(|stored| {
            let name = badge_key_name(stored.as_ref());
            let Some((_, value)) = stored.split_once('/').or_else(|| stored.split_once(':')) else {
                return false;
            };
            equals_ignore_ascii_case(name, "subscriber")
                && (equals_ignore_ascii_case(value, tier)
                    || match tier {
                        "1" => value.starts_with('1'),
                        "2" => value.starts_with('2'),
                        "3" => value.starts_with('3'),
                        _ => false,
                    })
        })
    }

    fn has_type(&self, value: &str) -> bool {
        match value {
            "action" => self.flags.is_action,
            "announcement" => {
                matches!(
                    self.flags.highlight_type,
                    Some(HighlightType::Announcement { .. })
                )
            }
            "automod" | "auto-mod" => {
                self.flags.is_automod
                    || (self.flags.is_system
                        && content_has_any(&self.content, &["automod", "auto mod"]))
            }
            "cheer-msg" | "cheer" | "bits" | "bits-badge" | "bitsbadgetier" => {
                matches!(self.flags.highlight_type, Some(HighlightType::BitsBadge))
                    || contains_ignore_ascii_case(&self.content, "cheer")
            }
            "connection" | "connection-status" => matches!(
                self.flags.system_type,
                Some(SystemMessageType::ConnectionStatus { .. })
            ),
            "deleted" | "disabled" => {
                self.flags.is_deleted
                    || (self.flags.is_system
                        && contains_ignore_ascii_case(&self.content, "deleted"))
            }
            "error" => matches!(
                self.flags.system_type,
                Some(SystemMessageType::Error { .. })
            ),
            "first-msg" | "first-message" => {
                self.flags.is_first_message
                    || matches!(self.flags.highlight_type, Some(HighlightType::FirstMessage))
            }
            "highlight" | "highlighted" => is_highlighted_search_match(self),
            "elevated" | "elevated-msg" | "elevated-message" => self.flags.is_elevated_message,
            "hype-chat" | "hypechat" | "paid-pinned-chat" | "pinned-chat" => {
                self.flags.is_hype_chat
            }
            "hype-train" | "hypetrain" => {
                self.flags.is_system && contains_ignore_ascii_case(&self.content, "hype train")
            }
            "info" => matches!(self.flags.system_type, Some(SystemMessageType::Info)),
            "mention" => {
                self.has_mention()
                    || matches!(
                        self.flags.highlight_type,
                        Some(
                            HighlightType::Mention
                                | HighlightType::User { .. }
                                | HighlightType::CustomUser { .. }
                                | HighlightType::ParticipatedReply { .. }
                        )
                    )
            }
            "mod" | "moderator" => self
                .badges
                .iter()
                .any(|badge| is_moderator_badge_key(badge.as_ref())),
            "vip" => self.has_badge("vip"),
            "broadcaster" | "streamer" => self.has_badge("broadcaster"),
            "staff" | "admin" | "global-mod" => self.has_badge(value),
            "founder" => self.has_badge("founder"),
            "partner" => self.has_badge("partner"),
            "prime" | "premium" => self.has_badge("premium"),
            "turbo" => self.has_badge("turbo"),
            "artist" => self.has_badge("artist-badge") || self.has_badge("artist"),
            "reply" => self.reply.is_some(),
            "restricted" | "restricted-user" => self.flags.is_restricted,
            "raid" => {
                matches!(self.flags.highlight_type, Some(HighlightType::Raid))
                    || (self.flags.is_system && contains_ignore_ascii_case(&self.content, "raid"))
            }
            "phrase" | "phrase-highlight" => {
                matches!(
                    self.flags.highlight_type,
                    Some(HighlightType::Phrase { .. } | HighlightType::CustomPhrase { .. })
                )
            }
            "poll" => self.flags.is_system && contains_ignore_ascii_case(&self.content, "poll"),
            "prediction" => {
                self.flags.is_system && contains_ignore_ascii_case(&self.content, "prediction")
            }
            "redemption" | "redeem" | "reward" | "channel-point" | "channel-points" => matches!(
                self.flags.highlight_type,
                Some(HighlightType::ChannelPointRedeem { .. })
            ),
            "returning-chatter" => matches!(
                self.flags.highlight_type,
                Some(HighlightType::ReturningChatter)
            ),
            "shared" | "shared-chat" => self.flags.is_shared_chat,
            "shield" | "shield-mode" => {
                self.flags.is_system && contains_ignore_ascii_case(&self.content, "shield")
            }
            "suspicious" | "suspicious-user" | "low-trust" => {
                self.flags.is_suspicious_user
                    || self.flags.is_restricted
                    || self.flags.is_monitored
                    || (self.flags.is_system
                        && content_has_any(
                            &self.content,
                            &["suspicious-user", "suspicious user", "low trust"],
                        ))
            }
            "monitored" | "monitoring" => self.flags.is_monitored,
            "self" | "self-message" => {
                matches!(self.flags.highlight_type, Some(HighlightType::SelfMessage))
            }
            "sub" | "subscription" | "subscriber" => is_subscription_search_match(self),
            "system" => self.flags.is_system && !is_timeout_or_ban_system_message(self),
            "timeout" | "ban" => is_timeout_or_ban_system_message(self),
            "upload" => matches!(
                self.flags.system_type,
                Some(
                    SystemMessageType::UploadStart
                        | SystemMessageType::UploadSuccess { .. }
                        | SystemMessageType::UploadError { .. }
                )
            ),
            "warning" => {
                self.flags.is_system && contains_ignore_ascii_case(&self.content, "warning")
            }
            "watch-streak" | "watchstreak" => {
                matches!(self.flags.highlight_type, Some(HighlightType::WatchStreak))
                    || (self.flags.is_system
                        && contains_ignore_ascii_case(&self.content, "watch streak"))
            }
            "whisper" => matches!(self.flags.highlight_type, Some(HighlightType::Whisper)),
            _ => false,
        }
    }
}

#[inline]
fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    crate::search::contains_ignore_ascii_case(haystack, needle_lower)
}

#[inline]
fn equals_ignore_ascii_case(left: &str, right: &str) -> bool {
    crate::search::equals_ignore_ascii_case(left, right)
}

#[inline]
fn content_has_any(content: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|needle| contains_ignore_ascii_case(content, needle))
}

#[inline]
fn is_highlighted_search_match(msg: &ChatMessage) -> bool {
    matches!(
        msg.flags.highlight_type,
        Some(HighlightType::HighlightedMessage)
    )
}

#[inline]
fn is_subscription_search_match(msg: &ChatMessage) -> bool {
    matches!(msg.flags.highlight_type, Some(HighlightType::Subscription))
        || (msg.flags.is_system && contains_ignore_ascii_case(&msg.content, "subscribed"))
}

#[inline]
fn is_timeout_or_ban_system_message(msg: &ChatMessage) -> bool {
    msg.flags.is_system && content_has_any(&msg.content, &["timed out", "timeout", "banned"])
}
