//! Shared Twitch EventSub notification and chat-row text formatting.

use super::types::{TwitchEvent, TwitchEventType};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TwitchEventTextPrivacy {
    pub hide_mod_actions: bool,
    pub hide_automod_terms: bool,
}

pub fn streamer_mode_twitch_event_privacy() -> TwitchEventTextPrivacy {
    TwitchEventTextPrivacy {
        hide_mod_actions: crate::backend::streamer_mode::should_hide_mod_actions(),
        hide_automod_terms: crate::backend::streamer_mode::should_hide_automod_terms(),
    }
}

/// Human-readable title for a Twitch event notification.
pub fn twitch_event_title(event_type: &TwitchEventType) -> &'static str {
    match event_type {
        TwitchEventType::Subscribe => "Subscription",
        TwitchEventType::Resubscribe => "Resubscription",
        TwitchEventType::GiftSub => "Gift sub",
        TwitchEventType::Raid => "Raid",
        TwitchEventType::ChannelPointRedemption => "Channel points",
        TwitchEventType::StreamOnline => "Stream online",
        TwitchEventType::StreamOffline => "Stream offline",
        TwitchEventType::Ban => "Moderation",
        TwitchEventType::Unban => "Moderation",
        TwitchEventType::AutomodMessageHold => "AutoMod",
        TwitchEventType::AutomodMessageUpdate => "AutoMod",
        TwitchEventType::SuspiciousUserMessage => "Suspicious user",
        TwitchEventType::SuspiciousUserUpdate => "Suspicious user",
        TwitchEventType::PollBegin => "Poll",
        TwitchEventType::PollProgress => "Poll",
        TwitchEventType::PollEnd => "Poll",
        TwitchEventType::PredictionBegin => "Prediction",
        TwitchEventType::PredictionProgress => "Prediction",
        TwitchEventType::PredictionLock => "Prediction",
        TwitchEventType::PredictionEnd => "Prediction",
        TwitchEventType::HypeTrainBegin => "Hype Train",
        TwitchEventType::HypeTrainProgress => "Hype Train",
        TwitchEventType::HypeTrainEnd => "Hype Train",
        TwitchEventType::VipAdd => "VIP",
        TwitchEventType::VipRemove => "VIP",
        TwitchEventType::ModeratorAdd => "Moderator",
        TwitchEventType::ModeratorRemove => "Moderator",
        TwitchEventType::WarningAcknowledge => "Warning",
        TwitchEventType::WarningSend => "Warning",
        TwitchEventType::ShieldModeBegin => "Shield Mode",
        TwitchEventType::ShieldModeEnd => "Shield Mode",
    }
}

/// Human-readable event body shared by chat system messages and the dropdown.
pub fn format_twitch_event_text(event: &TwitchEvent, privacy: TwitchEventTextPrivacy) -> String {
    let user = event.user_name.as_deref().unwrap_or("Someone");

    match event.event_type {
        TwitchEventType::Subscribe => {
            let tier = format_tier(event.tier.as_deref());
            format!("{} subscribed{}!", user, tier)
        }
        TwitchEventType::Resubscribe => {
            let tier = format_tier(event.tier.as_deref());
            let months = event
                .cumulative_months
                .map(|m| format!(" for {} months", m))
                .unwrap_or_default();
            format!("{} resubscribed{}{}!", user, tier, months)
        }
        TwitchEventType::GiftSub => {
            let tier = format_tier(event.tier.as_deref());
            if let Some(total) = event.total {
                format!("{} gifted {} subs{}!", user, total, tier)
            } else {
                format!("{} received a gift sub{}!", user, tier)
            }
        }
        TwitchEventType::Raid => {
            let from = event.from_channel.as_deref().unwrap_or(user);
            let viewers = event.viewers.unwrap_or(0);
            format!("{} raided with {} viewers!", from, viewers)
        }
        TwitchEventType::ChannelPointRedemption => {
            let reward = event.reward_title.as_deref().unwrap_or("a reward");
            let cost = event
                .reward_cost
                .map(|c| format!(" ({} points)", c))
                .unwrap_or_default();
            let input = event
                .user_input
                .as_deref()
                .map(str::trim)
                .filter(|input| !input.is_empty())
                .map(|input| format!(": {}", input))
                .unwrap_or_default();
            format!("{} redeemed {}{}{}", user, reward, cost, input)
        }
        TwitchEventType::StreamOnline => "Stream is now live!".to_string(),
        TwitchEventType::StreamOffline => "Stream has ended.".to_string(),
        TwitchEventType::Ban => {
            if privacy.hide_mod_actions {
                return if event.is_permanent {
                    "A user was banned.".to_string()
                } else {
                    "A user was timed out.".to_string()
                };
            }

            let mod_name = event.moderator_name.as_deref().unwrap_or("A moderator");
            let reason = reason_suffix(event.reason.as_deref());
            if event.is_permanent {
                format!("{} was banned by {}{}", user, mod_name, reason)
            } else {
                format!("{} was timed out by {}{}", user, mod_name, reason)
            }
        }
        TwitchEventType::Unban => {
            if privacy.hide_mod_actions {
                return "A user was unbanned.".to_string();
            }

            let mod_name = event.moderator_name.as_deref().unwrap_or("A moderator");
            format!("{} was unbanned by {}", user, mod_name)
        }
        TwitchEventType::AutomodMessageHold => {
            if privacy.hide_automod_terms {
                return "An AutoMod message was held.".to_string();
            }

            let detail =
                message_detail_suffix(event.message.as_deref(), "message content unavailable");
            let reason = if let Some(reason) = trimmed(event.reason.as_deref()) {
                format!(" flagged for {}", humanize_status(reason))
            } else {
                String::new()
            };
            format!("{} had a message held by AutoMod{}{}", user, reason, detail)
        }
        TwitchEventType::AutomodMessageUpdate => {
            let status = event
                .status
                .as_deref()
                .map(humanize_status)
                .unwrap_or_else(|| "updated".to_string());
            if privacy.hide_automod_terms {
                return format!("An AutoMod message was {}.", status);
            }

            let detail =
                message_detail_suffix(event.message.as_deref(), "message content unavailable");
            let reason = if let Some(reason) = trimmed(event.reason.as_deref()) {
                format!(" after {}", humanize_status(reason))
            } else {
                String::new()
            };
            format!(
                "AutoMod message from {} was {}{}{}",
                user, status, reason, detail
            )
        }
        TwitchEventType::SuspiciousUserMessage => {
            if privacy.hide_mod_actions {
                return "A suspicious user sent a message.".to_string();
            }

            let status = event
                .status
                .as_deref()
                .map(humanize_status)
                .unwrap_or_else(|| "suspicious".to_string());
            let detail =
                message_detail_suffix(event.message.as_deref(), "message content unavailable");
            format!(
                "{} ({}) sent a suspicious-user message{}",
                user, status, detail
            )
        }
        TwitchEventType::SuspiciousUserUpdate => {
            if privacy.hide_mod_actions {
                return "A suspicious user status changed.".to_string();
            }

            let status = event
                .status
                .as_deref()
                .map(humanize_status)
                .unwrap_or_else(|| "updated".to_string());
            let moderator = event
                .moderator_name
                .as_deref()
                .map(|moderator| format!(" by {moderator}"))
                .unwrap_or_default();
            format!(
                "{} suspicious-user status changed to {}{}",
                user, status, moderator
            )
        }
        TwitchEventType::PollBegin => append_event_detail(
            format!(
                "Poll started: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled poll")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PollProgress => append_event_detail(
            format!(
                "Poll update: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled poll")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PollEnd => {
            let status = event.status.as_deref().unwrap_or("ended");
            append_event_detail(
                format!(
                    "Poll ended ({}): \"{}\"",
                    status,
                    event_subject(event.event_title.as_deref(), "Untitled poll")
                ),
                event.event_detail.as_deref(),
            )
        }
        TwitchEventType::PredictionBegin => append_event_detail(
            format!(
                "Prediction started: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionProgress => append_event_detail(
            format!(
                "Prediction update: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionLock => append_event_detail(
            format!(
                "Prediction locked: \"{}\"",
                event_subject(event.event_title.as_deref(), "Untitled prediction")
            ),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::PredictionEnd => {
            let status = event.status.as_deref().unwrap_or("ended");
            append_event_detail(
                format!(
                    "Prediction ended ({}): \"{}\"",
                    status,
                    event_subject(event.event_title.as_deref(), "Untitled prediction")
                ),
                event.event_detail.as_deref(),
            )
        }
        TwitchEventType::HypeTrainBegin => append_event_detail(
            "Hype Train started".to_string(),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::HypeTrainProgress => append_event_detail(
            "Hype Train progress".to_string(),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::HypeTrainEnd => append_event_detail(
            "Hype Train ended".to_string(),
            event.event_detail.as_deref(),
        ),
        TwitchEventType::VipAdd => {
            if privacy.hide_mod_actions {
                "A user was added as a VIP.".to_string()
            } else {
                format!("{} was made a VIP.", user)
            }
        }
        TwitchEventType::VipRemove => {
            if privacy.hide_mod_actions {
                "A user was removed as a VIP.".to_string()
            } else {
                format!("{} is no longer a VIP.", user)
            }
        }
        TwitchEventType::ModeratorAdd => {
            if privacy.hide_mod_actions {
                "A user was added as a moderator.".to_string()
            } else {
                format!("{} was added as a moderator.", user)
            }
        }
        TwitchEventType::ModeratorRemove => {
            if privacy.hide_mod_actions {
                "A user was removed as a moderator.".to_string()
            } else {
                format!("{} is no longer a moderator.", user)
            }
        }
        TwitchEventType::WarningAcknowledge => {
            if privacy.hide_mod_actions {
                "A user acknowledged a warning.".to_string()
            } else {
                format!("{} acknowledged a warning.", user)
            }
        }
        TwitchEventType::WarningSend => {
            if privacy.hide_mod_actions {
                return "A user was warned.".to_string();
            }

            let moderator = event
                .moderator_name
                .as_deref()
                .map(|moderator| format!(" by {moderator}"))
                .unwrap_or_default();
            let reason = reason_suffix(event.reason.as_deref());
            format!("{} was warned{}{}", user, moderator, reason)
        }
        TwitchEventType::ShieldModeBegin => {
            if privacy.hide_mod_actions {
                "Shield Mode was activated.".to_string()
            } else {
                let moderator = event
                    .moderator_name
                    .as_deref()
                    .map(|moderator| format!(" by {moderator}"))
                    .unwrap_or_default();
                format!("Shield Mode was activated{}.", moderator)
            }
        }
        TwitchEventType::ShieldModeEnd => {
            if privacy.hide_mod_actions {
                "Shield Mode was deactivated.".to_string()
            } else {
                let moderator = event
                    .moderator_name
                    .as_deref()
                    .map(|moderator| format!(" by {moderator}"))
                    .unwrap_or_default();
                format!("Shield Mode was deactivated{}.", moderator)
            }
        }
    }
}

pub(super) fn event_subject(title: Option<&str>, fallback: &str) -> String {
    title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub(super) fn append_event_detail(mut text: String, detail: Option<&str>) -> String {
    if let Some(detail) = trimmed(detail) {
        text.push_str(" - ");
        text.push_str(detail);
    }
    text
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn reason_suffix(reason: Option<&str>) -> String {
    trimmed(reason)
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default()
}

fn message_detail_suffix(message: Option<&str>, unavailable: &str) -> String {
    match trimmed(message) {
        Some(message) => format!(" (message: {message})"),
        None => format!(" ({unavailable})"),
    }
}

fn humanize_status(status: &str) -> String {
    trimmed(Some(status))
        .map(|status| status.replace('_', " ").to_lowercase())
        .unwrap_or_default()
}

/// Format subscription tier for display.
pub(crate) fn format_tier(tier: Option<&str>) -> String {
    match tier {
        Some("2000") => " at Tier 2".to_string(),
        Some("3000") => " at Tier 3".to_string(),
        Some("Prime") => " with Prime".to_string(),
        _ => String::new(),
    }
}
