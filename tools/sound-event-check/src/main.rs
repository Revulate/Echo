mod components {
    pub mod event_notification {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum TwitchEventType {
            Subscribe,
            Resubscribe,
            GiftSub,
            Raid,
            ChannelPointRedemption,
            StreamOnline,
            StreamOffline,
            Ban,
            AutomodMessageHold,
            PollBegin,
            PredictionEnd,
            HypeTrainBegin,
            VipAdd,
            WarningSend,
            ShieldModeBegin,
        }
    }
}

mod backend {
    pub mod settings {
        pub fn get_thread_highlight_sound_enabled() -> bool {
            false
        }
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

mod sounds {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SoundEventType {
        Mention,
        Whisper,
        HighlightedMessage,
        FirstMessage,
        ReturningChatter,
        Subscription,
        Raid,
        BitsBadge,
        Announcement,
        Phrase,
        ChannelPointRedeem,
        StreamOnline,
        StreamOffline,
    }
}

#[path = "../../../src/backend/sound_events.rs"]
mod sound_events;

use components::event_notification::TwitchEventType;
use models::HighlightType;
use sound_events::{
    sound_for_highlight_type, sound_for_twitch_event_type, supports_desktop_notification,
};
use sounds::SoundEventType;

fn sound_name(sound: Option<SoundEventType>) -> &'static str {
    match sound {
        Some(SoundEventType::Mention) => "mention",
        Some(SoundEventType::Whisper) => "whisper",
        Some(SoundEventType::HighlightedMessage) => "highlighted-message",
        Some(SoundEventType::FirstMessage) => "first-message",
        Some(SoundEventType::ReturningChatter) => "returning-chatter",
        Some(SoundEventType::Subscription) => "subscription",
        Some(SoundEventType::Raid) => "raid",
        Some(SoundEventType::BitsBadge) => "bits-badge",
        Some(SoundEventType::Announcement) => "announcement",
        Some(SoundEventType::Phrase) => "phrase",
        Some(SoundEventType::ChannelPointRedeem) => "channel-point-redeem",
        Some(SoundEventType::StreamOnline) => "stream-online",
        Some(SoundEventType::StreamOffline) => "stream-offline",
        None => "none",
    }
}

fn assert_highlight_case(name: &str, highlight_type: Option<&HighlightType>, expected: &str) {
    let actual = sound_name(sound_for_highlight_type(highlight_type));
    assert_eq!(actual, expected, "highlight sound mismatch for {name}");
    println!("{name},highlight,{actual}");
}

fn assert_event_case(name: &str, event_type: TwitchEventType, expected: &str) {
    let actual = sound_name(sound_for_twitch_event_type(&event_type));
    assert_eq!(actual, expected, "EventSub sound mismatch for {name}");
    println!("{name},eventsub,{actual}");
}

fn main() {
    println!("case,source,sound");

    assert_highlight_case("highlight_none", None, "none");
    assert_highlight_case(
        "mention_highlight",
        Some(&HighlightType::Mention),
        "mention",
    );
    assert_highlight_case(
        "whisper_highlight",
        Some(&HighlightType::Whisper),
        "whisper",
    );
    assert_highlight_case(
        "highlighted_message",
        Some(&HighlightType::HighlightedMessage),
        "highlighted-message",
    );
    assert_highlight_case(
        "first_message",
        Some(&HighlightType::FirstMessage),
        "first-message",
    );
    assert_highlight_case(
        "returning_chatter",
        Some(&HighlightType::ReturningChatter),
        "returning-chatter",
    );
    assert_highlight_case(
        "subscription_usernotice",
        Some(&HighlightType::Subscription),
        "subscription",
    );
    assert_highlight_case("raid_usernotice", Some(&HighlightType::Raid), "raid");
    assert_highlight_case("bits_badge", Some(&HighlightType::BitsBadge), "bits-badge");
    assert_highlight_case(
        "announcement_usernotice",
        Some(&HighlightType::Announcement { color: None }),
        "announcement",
    );
    assert_highlight_case(
        "phrase_match",
        Some(&HighlightType::Phrase {
            pattern: "needle".to_string(),
        }),
        "phrase",
    );
    assert_highlight_case(
        "channel_point_redeem",
        Some(&HighlightType::ChannelPointRedeem {
            reward_id: "reward-id".to_string(),
            reward_title: "Reward".to_string(),
            reward_cost: 100,
        }),
        "channel-point-redeem",
    );
    assert_highlight_case(
        "user_highlight_no_sound",
        Some(&HighlightType::User {
            username: "viewer".to_string(),
        }),
        "none",
    );
    assert_highlight_case(
        "badge_highlight_no_sound",
        Some(&HighlightType::Badge {
            badge: "vip".to_string(),
        }),
        "none",
    );
    assert_highlight_case(
        "self_message_no_sound",
        Some(&HighlightType::SelfMessage),
        "none",
    );
    assert_highlight_case(
        "watch_streak_no_sound",
        Some(&HighlightType::WatchStreak),
        "none",
    );
    assert_highlight_case(
        "participated_reply_setting_off",
        Some(&HighlightType::ParticipatedReply {
            username: "thread-author".to_string(),
        }),
        "none",
    );
    assert_highlight_case(
        "custom_phrase_sound",
        Some(&HighlightType::CustomPhrase {
            pattern: "needle".to_string(),
            color: None,
        }),
        "phrase",
    );
    assert_highlight_case(
        "custom_user_sound",
        Some(&HighlightType::CustomUser {
            username: "viewer".to_string(),
            color: None,
        }),
        "phrase",
    );
    assert_highlight_case(
        "custom_badge_sound",
        Some(&HighlightType::CustomBadge {
            badge: "vip".to_string(),
            color: None,
        }),
        "phrase",
    );
    assert_highlight_case(
        "elevated_message_no_sound",
        Some(&HighlightType::Elevated),
        "none",
    );

    assert_event_case(
        "eventsub_subscribe",
        TwitchEventType::Subscribe,
        "subscription",
    );
    assert_event_case(
        "eventsub_resubscribe",
        TwitchEventType::Resubscribe,
        "subscription",
    );
    assert_event_case(
        "eventsub_gift_sub",
        TwitchEventType::GiftSub,
        "subscription",
    );
    assert_event_case("eventsub_raid", TwitchEventType::Raid, "raid");
    assert_event_case(
        "eventsub_channel_point",
        TwitchEventType::ChannelPointRedemption,
        "channel-point-redeem",
    );
    assert_event_case(
        "eventsub_stream_online",
        TwitchEventType::StreamOnline,
        "stream-online",
    );
    assert_event_case(
        "eventsub_stream_offline",
        TwitchEventType::StreamOffline,
        "stream-offline",
    );
    assert_event_case("eventsub_ban_no_sound", TwitchEventType::Ban, "none");
    assert_event_case(
        "eventsub_automod_no_sound",
        TwitchEventType::AutomodMessageHold,
        "none",
    );
    assert_event_case("eventsub_poll_no_sound", TwitchEventType::PollBegin, "none");
    assert_event_case(
        "eventsub_prediction_no_sound",
        TwitchEventType::PredictionEnd,
        "none",
    );
    assert_event_case(
        "eventsub_hype_train_no_sound",
        TwitchEventType::HypeTrainBegin,
        "none",
    );
    assert_event_case("eventsub_vip_no_sound", TwitchEventType::VipAdd, "none");
    assert_event_case(
        "eventsub_warning_no_sound",
        TwitchEventType::WarningSend,
        "none",
    );
    assert_event_case(
        "eventsub_shield_mode_no_sound",
        TwitchEventType::ShieldModeBegin,
        "none",
    );

    for event_type in [
        SoundEventType::Mention,
        SoundEventType::Whisper,
        SoundEventType::Phrase,
        SoundEventType::Subscription,
        SoundEventType::Raid,
        SoundEventType::ChannelPointRedeem,
        SoundEventType::StreamOnline,
        SoundEventType::StreamOffline,
    ] {
        assert!(supports_desktop_notification(event_type));
    }
    for event_type in [
        SoundEventType::HighlightedMessage,
        SoundEventType::FirstMessage,
        SoundEventType::ReturningChatter,
        SoundEventType::BitsBadge,
        SoundEventType::Announcement,
    ] {
        assert!(!supports_desktop_notification(event_type));
    }
}
