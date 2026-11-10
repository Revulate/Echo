//! Shared sound-event routing for chat highlights and Twitch EventSub rows.

use super::components::event_notification::TwitchEventType;
use super::models::HighlightType;
use super::sounds::SoundEventType;

pub fn sound_for_highlight_type(highlight_type: Option<&HighlightType>) -> Option<SoundEventType> {
    match highlight_type? {
        HighlightType::Mention => Some(SoundEventType::Mention),
        HighlightType::Whisper => Some(SoundEventType::Whisper),
        HighlightType::HighlightedMessage => Some(SoundEventType::HighlightedMessage),
        HighlightType::FirstMessage => Some(SoundEventType::FirstMessage),
        HighlightType::ReturningChatter => Some(SoundEventType::ReturningChatter),
        HighlightType::Subscription => Some(SoundEventType::Subscription),
        HighlightType::Raid => Some(SoundEventType::Raid),
        HighlightType::BitsBadge => Some(SoundEventType::BitsBadge),
        HighlightType::Announcement { .. } => Some(SoundEventType::Announcement),
        HighlightType::Phrase { .. } => Some(SoundEventType::Phrase),
        HighlightType::CustomPhrase { .. }
        | HighlightType::CustomUser { .. }
        | HighlightType::CustomBadge { .. } => Some(SoundEventType::Phrase),
        HighlightType::ChannelPointRedeem { .. } => Some(SoundEventType::ChannelPointRedeem),
        HighlightType::ParticipatedReply { .. } => {
            crate::backend::settings::get_thread_highlight_sound_enabled()
                .then_some(SoundEventType::Mention)
        }
        HighlightType::User { .. }
        | HighlightType::Badge { .. }
        | HighlightType::SelfMessage
        | HighlightType::Elevated
        | HighlightType::WatchStreak => None,
    }
}

pub fn sound_for_twitch_event_type(event_type: &TwitchEventType) -> Option<SoundEventType> {
    match event_type {
        TwitchEventType::Subscribe | TwitchEventType::Resubscribe | TwitchEventType::GiftSub => {
            Some(SoundEventType::Subscription)
        }
        TwitchEventType::Raid => Some(SoundEventType::Raid),
        TwitchEventType::ChannelPointRedemption => Some(SoundEventType::ChannelPointRedeem),
        TwitchEventType::StreamOnline => Some(SoundEventType::StreamOnline),
        TwitchEventType::StreamOffline => Some(SoundEventType::StreamOffline),
        _ => None,
    }
}

pub fn supports_desktop_notification(event_type: SoundEventType) -> bool {
    matches!(
        event_type,
        SoundEventType::Mention
            | SoundEventType::Whisper
            | SoundEventType::Phrase
            | SoundEventType::Subscription
            | SoundEventType::Raid
            | SoundEventType::ChannelPointRedeem
            | SoundEventType::StreamOnline
            | SoundEventType::StreamOffline
    )
}
