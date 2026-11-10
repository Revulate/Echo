#![allow(dead_code)]

mod backend {
    pub mod streamer_mode {
        pub fn should_hide_mod_actions() -> bool {
            false
        }
        pub fn should_hide_automod_terms() -> bool {
            false
        }
    }
}

mod types {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum TwitchEventType {
        Subscribe,
        Resubscribe,
        GiftSub,
        Raid,
        ChannelPointRedemption,
        StreamOnline,
        StreamOffline,
        Ban,
        Unban,
        AutomodMessageHold,
        AutomodMessageUpdate,
        SuspiciousUserMessage,
        SuspiciousUserUpdate,
        PollBegin,
        PollProgress,
        PollEnd,
        PredictionBegin,
        PredictionProgress,
        PredictionLock,
        PredictionEnd,
        HypeTrainBegin,
        HypeTrainProgress,
        HypeTrainEnd,
        VipAdd,
        VipRemove,
        ModeratorAdd,
        ModeratorRemove,
        WarningAcknowledge,
        WarningSend,
        ShieldModeBegin,
        ShieldModeEnd,
    }

    #[derive(Clone, Debug)]
    pub struct TwitchEvent {
        pub event_type: TwitchEventType,
        pub user_name: Option<String>,
        pub tier: Option<String>,
        pub cumulative_months: Option<u32>,
        pub message: Option<String>,
        pub total: Option<u32>,
        pub from_channel: Option<String>,
        pub viewers: Option<u32>,
        pub reward_title: Option<String>,
        pub reward_cost: Option<u32>,
        pub user_input: Option<String>,
        pub moderator_name: Option<String>,
        pub reason: Option<String>,
        pub status: Option<String>,
        pub event_title: Option<String>,
        pub event_detail: Option<String>,
        pub is_permanent: bool,
    }

    impl Default for TwitchEvent {
        fn default() -> Self {
            Self {
                event_type: TwitchEventType::Subscribe,
                user_name: None,
                tier: None,
                cumulative_months: None,
                message: None,
                total: None,
                from_channel: None,
                viewers: None,
                reward_title: None,
                reward_cost: None,
                user_input: None,
                moderator_name: None,
                reason: None,
                status: None,
                event_title: None,
                event_detail: None,
                is_permanent: false,
            }
        }
    }
}

#[path = "../../../src/backend/event_text.rs"]
mod event_text;

use event_text::{TwitchEventTextPrivacy, format_twitch_event_text, twitch_event_title};
use types::{TwitchEvent, TwitchEventType};

fn assert_case(name: &str, event: TwitchEvent, expected_title: &str, expected_text: &str) {
    let actual_title = twitch_event_title(&event.event_type);
    let actual_text = format_twitch_event_text(&event, TwitchEventTextPrivacy::default());
    assert_eq!(actual_title, expected_title, "title mismatch for {name}");
    assert_eq!(actual_text, expected_text, "text mismatch for {name}");
    println!("{name},{actual_title},{actual_text}");
}

fn assert_case_with_privacy(
    name: &str,
    event: TwitchEvent,
    privacy: TwitchEventTextPrivacy,
    expected_title: &str,
    expected_text: &str,
) {
    let actual_title = twitch_event_title(&event.event_type);
    let actual_text = format_twitch_event_text(&event, privacy);
    assert_eq!(actual_title, expected_title, "title mismatch for {name}");
    assert_eq!(actual_text, expected_text, "text mismatch for {name}");
    println!("{name},{actual_title},{actual_text}");
}

fn main() {
    println!("case,title,text");

    assert_case(
        "channel_point_with_input",
        TwitchEvent {
            event_type: TwitchEventType::ChannelPointRedemption,
            user_name: Some("Viewer".to_string()),
            reward_title: Some("Pick next game".to_string()),
            reward_cost: Some(250),
            user_input: Some("Crab game".to_string()),
            ..Default::default()
        },
        "Channel points",
        "Viewer redeemed Pick next game (250 points): Crab game",
    );
    assert_case(
        "channel_point_fallback",
        TwitchEvent {
            event_type: TwitchEventType::ChannelPointRedemption,
            ..Default::default()
        },
        "Channel points",
        "Someone redeemed a reward",
    );
    assert_case(
        "poll_begin_detail",
        TwitchEvent {
            event_type: TwitchEventType::PollBegin,
            event_title: Some("Next game?".to_string()),
            event_detail: Some("Crab: 12 votes | Cards: 4 votes".to_string()),
            ..Default::default()
        },
        "Poll",
        "Poll started: \"Next game?\" - Crab: 12 votes | Cards: 4 votes",
    );
    assert_case(
        "poll_progress_detail",
        TwitchEvent {
            event_type: TwitchEventType::PollProgress,
            event_title: Some("Next game?".to_string()),
            event_detail: Some("Crab: 18 votes | Cards: 9 votes".to_string()),
            ..Default::default()
        },
        "Poll",
        "Poll update: \"Next game?\" - Crab: 18 votes | Cards: 9 votes",
    );
    assert_case(
        "poll_end_status",
        TwitchEvent {
            event_type: TwitchEventType::PollEnd,
            event_title: Some("Next game?".to_string()),
            event_detail: Some("Crab won".to_string()),
            status: Some("completed".to_string()),
            ..Default::default()
        },
        "Poll",
        "Poll ended (completed): \"Next game?\" - Crab won",
    );
    assert_case(
        "poll_fallback_title",
        TwitchEvent {
            event_type: TwitchEventType::PollBegin,
            ..Default::default()
        },
        "Poll",
        "Poll started: \"Untitled poll\"",
    );
    assert_case(
        "poll_whitespace_metadata",
        TwitchEvent {
            event_type: TwitchEventType::PollBegin,
            event_title: Some("   ".to_string()),
            event_detail: Some(" \t ".to_string()),
            ..Default::default()
        },
        "Poll",
        "Poll started: \"Untitled poll\"",
    );
    assert_case(
        "poll_trimmed_metadata",
        TwitchEvent {
            event_type: TwitchEventType::PollBegin,
            event_title: Some("  Next game?  ".to_string()),
            event_detail: Some("  Crab: 12 votes  ".to_string()),
            ..Default::default()
        },
        "Poll",
        "Poll started: \"Next game?\" - Crab: 12 votes",
    );
    assert_case(
        "prediction_begin_detail",
        TwitchEvent {
            event_type: TwitchEventType::PredictionBegin,
            event_title: Some("Will boss die?".to_string()),
            event_detail: Some("Yes: 5 users / 1200 pts | No: 3 users / 400 pts".to_string()),
            ..Default::default()
        },
        "Prediction",
        "Prediction started: \"Will boss die?\" - Yes: 5 users / 1200 pts | No: 3 users / 400 pts",
    );
    assert_case(
        "prediction_progress_detail",
        TwitchEvent {
            event_type: TwitchEventType::PredictionProgress,
            event_title: Some("Will boss die?".to_string()),
            event_detail: Some("Yes: 7 users / 1600 pts | No: 4 users / 500 pts".to_string()),
            ..Default::default()
        },
        "Prediction",
        "Prediction update: \"Will boss die?\" - Yes: 7 users / 1600 pts | No: 4 users / 500 pts",
    );
    assert_case(
        "prediction_lock_detail",
        TwitchEvent {
            event_type: TwitchEventType::PredictionLock,
            event_title: Some("Will boss die?".to_string()),
            event_detail: Some("Yes: 9 users / 2400 pts | No: 5 users / 700 pts".to_string()),
            ..Default::default()
        },
        "Prediction",
        "Prediction locked: \"Will boss die?\" - Yes: 9 users / 2400 pts | No: 5 users / 700 pts",
    );
    assert_case(
        "prediction_end_status",
        TwitchEvent {
            event_type: TwitchEventType::PredictionEnd,
            event_title: Some("Will boss die?".to_string()),
            event_detail: Some("Winner: Yes".to_string()),
            status: Some("resolved".to_string()),
            ..Default::default()
        },
        "Prediction",
        "Prediction ended (resolved): \"Will boss die?\" - Winner: Yes",
    );
    assert_case(
        "prediction_fallback_title",
        TwitchEvent {
            event_type: TwitchEventType::PredictionBegin,
            ..Default::default()
        },
        "Prediction",
        "Prediction started: \"Untitled prediction\"",
    );
    assert_case(
        "hype_train_begin_detail",
        TwitchEvent {
            event_type: TwitchEventType::HypeTrainBegin,
            event_detail: Some("Level 2 500/1000 points".to_string()),
            ..Default::default()
        },
        "Hype Train",
        "Hype Train started - Level 2 500/1000 points",
    );
    assert_case(
        "hype_train_progress_detail",
        TwitchEvent {
            event_type: TwitchEventType::HypeTrainProgress,
            event_detail: Some("Level 3 1200/2000 points".to_string()),
            ..Default::default()
        },
        "Hype Train",
        "Hype Train progress - Level 3 1200/2000 points",
    );
    assert_case(
        "hype_train_end_detail",
        TwitchEvent {
            event_type: TwitchEventType::HypeTrainEnd,
            event_detail: Some("Completed at level 4".to_string()),
            ..Default::default()
        },
        "Hype Train",
        "Hype Train ended - Completed at level 4",
    );
    assert_case(
        "hype_train_no_detail",
        TwitchEvent {
            event_type: TwitchEventType::HypeTrainEnd,
            ..Default::default()
        },
        "Hype Train",
        "Hype Train ended",
    );
    assert_case(
        "vip_add",
        TwitchEvent {
            event_type: TwitchEventType::VipAdd,
            user_name: Some("HelpfulViewer".to_string()),
            ..Default::default()
        },
        "VIP",
        "HelpfulViewer was made a VIP.",
    );
    assert_case(
        "moderator_remove",
        TwitchEvent {
            event_type: TwitchEventType::ModeratorRemove,
            user_name: Some("FormerMod".to_string()),
            ..Default::default()
        },
        "Moderator",
        "FormerMod is no longer a moderator.",
    );
    assert_case(
        "warning_send",
        TwitchEvent {
            event_type: TwitchEventType::WarningSend,
            user_name: Some("RuleBreaker".to_string()),
            moderator_name: Some("ModUser".to_string()),
            reason: Some("keep chat readable".to_string()),
            ..Default::default()
        },
        "Warning",
        "RuleBreaker was warned by ModUser: keep chat readable",
    );
    assert_case(
        "warning_acknowledge",
        TwitchEvent {
            event_type: TwitchEventType::WarningAcknowledge,
            user_name: Some("RuleBreaker".to_string()),
            ..Default::default()
        },
        "Warning",
        "RuleBreaker acknowledged a warning.",
    );
    assert_case(
        "shield_mode_begin",
        TwitchEvent {
            event_type: TwitchEventType::ShieldModeBegin,
            moderator_name: Some("SafetyMod".to_string()),
            ..Default::default()
        },
        "Shield Mode",
        "Shield Mode was activated by SafetyMod.",
    );
    assert_case(
        "shield_mode_end",
        TwitchEvent {
            event_type: TwitchEventType::ShieldModeEnd,
            moderator_name: Some("SafetyMod".to_string()),
            ..Default::default()
        },
        "Shield Mode",
        "Shield Mode was deactivated by SafetyMod.",
    );
    assert_case_with_privacy(
        "privacy_hide_warning_send",
        TwitchEvent {
            event_type: TwitchEventType::WarningSend,
            user_name: Some("RuleBreaker".to_string()),
            moderator_name: Some("ModUser".to_string()),
            reason: Some("keep chat readable".to_string()),
            ..Default::default()
        },
        TwitchEventTextPrivacy {
            hide_mod_actions: true,
            ..Default::default()
        },
        "Warning",
        "A user was warned.",
    );
    assert_case_with_privacy(
        "privacy_hide_shield_mode_begin",
        TwitchEvent {
            event_type: TwitchEventType::ShieldModeBegin,
            moderator_name: Some("SafetyMod".to_string()),
            ..Default::default()
        },
        TwitchEventTextPrivacy {
            hide_mod_actions: true,
            ..Default::default()
        },
        "Shield Mode",
        "Shield Mode was activated.",
    );
    assert_case_with_privacy(
        "privacy_hide_automod_terms",
        TwitchEvent {
            event_type: TwitchEventType::AutomodMessageHold,
            user_name: Some("Viewer".to_string()),
            message: Some("blocked words".to_string()),
            ..Default::default()
        },
        TwitchEventTextPrivacy {
            hide_automod_terms: true,
            ..Default::default()
        },
        "AutoMod",
        "An AutoMod message was held.",
    );
}
