//! Decoding EventSub notification payloads into [`TwitchEvent`]s, and the
//! policies that decide how a decoded event is surfaced.

use serde::Deserialize;

use super::EVENTSUB_NOTIFICATION_START_MS;
use crate::backend::components::event_notification::{
    PredictionOutcomeSnapshot, TwitchEvent, TwitchEventType,
};
use crate::backend::components::mod_log::{ModAction, ModActionType, add_mod_action};
use crate::backend::sound_events::sound_for_twitch_event_type;
use crate::backend::sounds::SOUND_STORE;

/// Decodes the notification `event` for subscription type `sub_type`, or
/// `None` for a type Echo does not handle.
pub(super) fn parse_notification(
    sub_type: &str,
    event: &serde_json::Value,
) -> Option<Result<TwitchEvent, String>> {
    Some(match sub_type {
        "channel.subscribe" => parse_subscribe_event(event),
        "channel.subscription.message" => parse_resubscribe_event(event),
        "channel.subscription.gift" => parse_gift_sub_event(event),
        "channel.raid" => parse_raid_event(event),
        "channel.channel_points_custom_reward_redemption.add" => parse_channel_point_event(event),
        "stream.online" => parse_stream_online_event(event),
        "stream.offline" => parse_stream_offline_event(event),
        "channel.ban" => parse_ban_event(event),
        "channel.unban" => parse_unban_event(event),
        "automod.message.hold" => {
            parse_automod_message_event(event, TwitchEventType::AutomodMessageHold)
        }
        "automod.message.update" => {
            parse_automod_message_event(event, TwitchEventType::AutomodMessageUpdate)
        }
        "channel.suspicious_user.message" => parse_suspicious_user_message_event(event),
        "channel.suspicious_user.update" => parse_suspicious_user_update_event(event),
        "channel.poll.begin" => parse_poll_event(event, TwitchEventType::PollBegin),
        "channel.poll.progress" => parse_poll_event(event, TwitchEventType::PollProgress),
        "channel.poll.end" => parse_poll_event(event, TwitchEventType::PollEnd),
        "channel.prediction.begin" => {
            parse_prediction_event(event, TwitchEventType::PredictionBegin)
        }
        "channel.prediction.progress" => {
            parse_prediction_event(event, TwitchEventType::PredictionProgress)
        }
        "channel.prediction.lock" => parse_prediction_event(event, TwitchEventType::PredictionLock),
        "channel.prediction.end" => parse_prediction_event(event, TwitchEventType::PredictionEnd),
        "channel.hype_train.begin" => {
            parse_hype_train_event(event, TwitchEventType::HypeTrainBegin)
        }
        "channel.hype_train.progress" => {
            parse_hype_train_event(event, TwitchEventType::HypeTrainProgress)
        }
        "channel.hype_train.end" => parse_hype_train_event(event, TwitchEventType::HypeTrainEnd),
        "channel.vip.add" => parse_user_role_event(event, TwitchEventType::VipAdd),
        "channel.vip.remove" => parse_user_role_event(event, TwitchEventType::VipRemove),
        "channel.moderator.add" => parse_user_role_event(event, TwitchEventType::ModeratorAdd),
        "channel.moderator.remove" => {
            parse_user_role_event(event, TwitchEventType::ModeratorRemove)
        }
        "channel.warning.acknowledge" => parse_warning_acknowledge_event(event),
        "channel.warning.send" => parse_warning_send_event(event),
        "channel.shield_mode.begin" => {
            parse_shield_mode_event(event, TwitchEventType::ShieldModeBegin)
        }
        "channel.shield_mode.end" => parse_shield_mode_event(event, TwitchEventType::ShieldModeEnd),
        _ => return None,
    })
}

fn decode<T: serde::de::DeserializeOwned>(
    event: &serde_json::Value,
    label: &str,
) -> Result<T, String> {
    serde_json::from_value(event.clone()).map_err(|e| format!("Failed to parse {label}: {e}"))
}

/// Subscribe event
#[derive(Debug, Deserialize)]
struct SubscribeEvent {
    user_name: String,
    #[serde(default)]
    broadcaster_user_login: String,
    tier: String,
    is_gift: bool,
}

/// Resubscribe event (subscription message)
#[derive(Debug, Deserialize)]
struct ResubscribeEvent {
    user_name: String,
    #[serde(default)]
    broadcaster_user_login: String,
    tier: String,
    #[serde(default)]
    cumulative_months: u32,
    #[serde(default)]
    streak_months: Option<u32>,
    message: Option<ResubMessage>,
}

#[derive(Debug, Deserialize)]
struct ResubMessage {
    text: String,
}

/// Gift sub event
#[derive(Debug, Deserialize)]
struct GiftSubEvent {
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    broadcaster_user_login: String,
    tier: String,
    total: u32,
    #[serde(default)]
    cumulative_total: Option<u32>,
    is_anonymous: bool,
}

/// Raid event
#[derive(Debug, Deserialize)]
struct RaidEvent {
    from_broadcaster_user_login: String,
    to_broadcaster_user_login: String,
    viewers: u32,
}

/// Channel point redemption event
#[derive(Debug, Deserialize)]
struct ChannelPointRedemptionEvent {
    user_name: String,
    #[serde(default)]
    broadcaster_user_login: String,
    reward: RewardInfo,
    #[serde(default)]
    user_input: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RewardInfo {
    #[serde(default)]
    id: String,
    title: String,
    cost: u32,
}

/// Stream online event
#[derive(Debug, Deserialize)]
struct StreamOnlineEvent {
    broadcaster_user_login: String,
}

/// Stream offline event
#[derive(Debug, Deserialize)]
struct StreamOfflineEvent {
    broadcaster_user_login: String,
}

/// Ban event
#[derive(Debug, Deserialize)]
struct BanEvent {
    user_name: String,
    broadcaster_user_login: String,
    moderator_user_name: String,
    #[serde(default)]
    reason: Option<String>,
    is_permanent: bool,
}

/// Unban event
#[derive(Debug, Deserialize)]
struct UnbanEvent {
    user_name: String,
    broadcaster_user_login: String,
    moderator_user_name: String,
}

#[derive(Debug, Deserialize)]
struct EventSubChatMessage {
    #[serde(default)]
    text: String,
}

/// AutoMod held or updated message event.
#[derive(Debug, Deserialize)]
struct AutomodMessageEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    user_id: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    message_id: String,
    message: EventSubChatMessage,
    #[serde(default)]
    moderator_user_name: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// Suspicious user status update event.
#[derive(Debug, Deserialize)]
struct SuspiciousUserUpdateEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    user_id: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    moderator_user_name: Option<String>,
    #[serde(default)]
    low_trust_status: String,
}

/// Suspicious user chat message event.
#[derive(Debug, Deserialize)]
struct SuspiciousUserMessageEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    user_id: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    low_trust_status: String,
    message: EventSubChatMessage,
}

#[derive(Debug, Deserialize)]
struct PollChoice {
    #[serde(default)]
    title: String,
    #[serde(default)]
    votes: u64,
}

#[derive(Debug, Deserialize)]
struct PollEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    choices: Vec<PollChoice>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PredictionOutcome {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    users: Option<u64>,
    #[serde(default)]
    channel_points: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PredictionEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    outcomes: Vec<PredictionOutcome>,
    #[serde(default)]
    winning_outcome_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HypeTrainEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    progress: u64,
    #[serde(default)]
    goal: u64,
    #[serde(default)]
    level: u64,
    #[serde(default)]
    is_shared_train: bool,
    #[serde(default, rename = "type")]
    train_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserRoleEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
}

#[derive(Debug, Deserialize)]
struct WarningAcknowledgeEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
}

#[derive(Debug, Deserialize)]
struct WarningSendEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    user_name: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    moderator_user_name: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShieldModeEvent {
    #[serde(default)]
    broadcaster_user_login: String,
    #[serde(default)]
    moderator_user_name: Option<String>,
}

fn nonempty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn summarize_poll_choices(choices: &[PollChoice]) -> Option<String> {
    let mut parts: Vec<String> = choices
        .iter()
        .filter_map(|choice| {
            let title = choice.title.trim();
            if title.is_empty() {
                return None;
            }

            if choice.votes > 0 {
                Some(format!("{} ({} votes)", title, choice.votes))
            } else {
                Some(title.to_string())
            }
        })
        .take(4)
        .collect();

    if parts.is_empty() {
        return None;
    }

    let extra = choices.len().saturating_sub(parts.len());
    if extra > 0 {
        parts.push(format!("+{} more", extra));
    }

    Some(format!("Choices: {}", parts.join(", ")))
}

fn summarize_prediction_outcomes(
    outcomes: &[PredictionOutcome],
    winning_outcome_id: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = outcomes
        .iter()
        .filter_map(|outcome| {
            let title = outcome.title.trim();
            if title.is_empty() {
                return None;
            }

            let mut details = Vec::new();
            if winning_outcome_id.is_some_and(|winner| winner == outcome.id.as_str()) {
                details.push("winner".to_string());
            }
            if let Some(users) = outcome.users.filter(|users| *users > 0) {
                details.push(format!("{} users", users));
            }
            if let Some(points) = outcome.channel_points.filter(|points| *points > 0) {
                details.push(format!("{} points", points));
            }

            if details.is_empty() {
                Some(title.to_string())
            } else {
                Some(format!("{} ({})", title, details.join(", ")))
            }
        })
        .take(4)
        .collect();

    if parts.is_empty() {
        return None;
    }

    let extra = outcomes.len().saturating_sub(parts.len());
    if extra > 0 {
        parts.push(format!("+{} more", extra));
    }

    Some(format!("Outcomes: {}", parts.join(", ")))
}

fn prediction_outcome_snapshot(outcome: &PredictionOutcome) -> Option<PredictionOutcomeSnapshot> {
    let title = outcome.title.trim();
    if title.is_empty() {
        return None;
    }

    Some(PredictionOutcomeSnapshot {
        id: outcome.id.clone(),
        title: title.to_string(),
        users: outcome.users,
        channel_points: outcome.channel_points,
    })
}

fn summarize_hype_train(event: &HypeTrainEvent) -> Option<String> {
    let mut parts = Vec::new();

    if event.level > 0 {
        parts.push(format!("Level {}", event.level));
    }
    if event.goal > 0 {
        parts.push(format!("{} / {}", event.progress, event.goal));
    } else if event.progress > 0 {
        parts.push(format!("{} progress", event.progress));
    }
    if event.total > 0 {
        parts.push(format!("{} total", event.total));
    }
    if event.is_shared_train {
        parts.push("shared".to_string());
    }
    if let Some(train_type) = event
        .train_type
        .as_deref()
        .map(str::trim)
        .filter(|train_type| {
            let train_type = *train_type;
            !train_type.is_empty() && train_type != "regular"
        })
    {
        parts.push(train_type.to_string());
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// Parse subscribe event
fn parse_subscribe_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: SubscribeEvent = decode(event, "subscribe")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::Subscribe,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        tier: Some(e.tier),
        is_gift: e.is_gift,
        ..Default::default()
    })
}

/// Parse resubscribe event
fn parse_resubscribe_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: ResubscribeEvent = decode(event, "resubscribe")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::Resubscribe,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        tier: Some(e.tier),
        cumulative_months: Some(e.cumulative_months),
        streak_months: e.streak_months,
        message: e.message.map(|m| m.text),
        ..Default::default()
    })
}

/// Parse gift sub event
fn parse_gift_sub_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: GiftSubEvent = decode(event, "gift sub")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::GiftSub,
        channel: e.broadcaster_user_login,
        user_name: e.user_name,
        tier: Some(e.tier),
        total: Some(e.total),
        cumulative_total: e.cumulative_total,
        is_anonymous: e.is_anonymous,
        ..Default::default()
    })
}

/// Parse raid event
fn parse_raid_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: RaidEvent = decode(event, "raid")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::Raid,
        channel: e.to_broadcaster_user_login,
        from_channel: Some(e.from_broadcaster_user_login),
        viewers: Some(e.viewers),
        ..Default::default()
    })
}

/// Parse channel point redemption event
fn parse_channel_point_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: ChannelPointRedemptionEvent = decode(event, "channel point")?;
    let RewardInfo { id, title, cost } = e.reward;

    Ok(TwitchEvent {
        event_type: TwitchEventType::ChannelPointRedemption,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        reward_id: (!id.is_empty()).then_some(id),
        reward_title: Some(title),
        reward_cost: Some(cost),
        user_input: e.user_input,
        ..Default::default()
    })
}

/// Parse stream online event
fn parse_stream_online_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: StreamOnlineEvent = decode(event, "stream online")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::StreamOnline,
        channel: e.broadcaster_user_login,
        ..Default::default()
    })
}

/// Parse stream offline event
fn parse_stream_offline_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: StreamOfflineEvent = decode(event, "stream offline")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::StreamOffline,
        channel: e.broadcaster_user_login,
        ..Default::default()
    })
}

/// Parse ban event
fn parse_ban_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: BanEvent = decode(event, "ban")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::Ban,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        moderator_name: Some(e.moderator_user_name),
        reason: e.reason,
        is_permanent: e.is_permanent,
        ..Default::default()
    })
}

/// Parse unban event
fn parse_unban_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: UnbanEvent = decode(event, "unban")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::Unban,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        moderator_name: Some(e.moderator_user_name),
        ..Default::default()
    })
}

fn parse_automod_message_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: AutomodMessageEvent = decode(event, "automod message")?;

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        user_id: (!e.user_id.is_empty()).then_some(e.user_id),
        message_id: (!e.message_id.is_empty()).then_some(e.message_id),
        moderator_name: e.moderator_user_name,
        message: (!e.message.text.is_empty()).then_some(e.message.text),
        reason: e.reason,
        status: e.status,
        ..Default::default()
    })
}

fn parse_suspicious_user_message_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: SuspiciousUserMessageEvent = decode(event, "suspicious user message")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::SuspiciousUserMessage,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        user_id: (!e.user_id.is_empty()).then_some(e.user_id),
        message: (!e.message.text.is_empty()).then_some(e.message.text),
        status: (!e.low_trust_status.is_empty()).then_some(e.low_trust_status),
        ..Default::default()
    })
}

fn parse_suspicious_user_update_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: SuspiciousUserUpdateEvent = decode(event, "suspicious user update")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::SuspiciousUserUpdate,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        user_id: (!e.user_id.is_empty()).then_some(e.user_id),
        moderator_name: e.moderator_user_name,
        status: (!e.low_trust_status.is_empty()).then_some(e.low_trust_status),
        ..Default::default()
    })
}

fn parse_poll_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: PollEvent = decode(event, "poll event")?;
    let detail = summarize_poll_choices(&e.choices);

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        event_title: nonempty(e.title),
        event_detail: detail,
        status: e.status,
        ..Default::default()
    })
}

fn parse_prediction_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: PredictionEvent = decode(event, "prediction event")?;
    let detail = summarize_prediction_outcomes(&e.outcomes, e.winning_outcome_id.as_deref());
    let prediction_outcomes = e
        .outcomes
        .iter()
        .filter_map(prediction_outcome_snapshot)
        .collect();

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        event_title: nonempty(e.title),
        event_detail: detail,
        status: e.status,
        prediction_outcomes,
        winning_outcome_id: e.winning_outcome_id,
        ..Default::default()
    })
}

fn parse_hype_train_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: HypeTrainEvent = decode(event, "hype train event")?;
    let detail = summarize_hype_train(&e);

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        event_title: Some("Hype Train".to_string()),
        event_detail: detail,
        ..Default::default()
    })
}

fn parse_user_role_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: UserRoleEvent = decode(event, "user role event")?;

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        ..Default::default()
    })
}

fn parse_warning_acknowledge_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: WarningAcknowledgeEvent = decode(event, "warning acknowledge event")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::WarningAcknowledge,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        ..Default::default()
    })
}

fn parse_warning_send_event(event: &serde_json::Value) -> Result<TwitchEvent, String> {
    let e: WarningSendEvent = decode(event, "warning send event")?;

    Ok(TwitchEvent {
        event_type: TwitchEventType::WarningSend,
        channel: e.broadcaster_user_login,
        user_name: Some(e.user_name),
        user_login: (!e.user_login.is_empty()).then_some(e.user_login),
        moderator_name: e.moderator_user_name,
        reason: e.reason,
        ..Default::default()
    })
}

fn parse_shield_mode_event(
    event: &serde_json::Value,
    event_type: TwitchEventType,
) -> Result<TwitchEvent, String> {
    let e: ShieldModeEvent = decode(event, "shield mode event")?;

    Ok(TwitchEvent {
        event_type,
        channel: e.broadcaster_user_login,
        moderator_name: e.moderator_user_name,
        ..Default::default()
    })
}

/// Play sound for event type
pub(super) fn play_event_sound(event: &TwitchEvent) {
    if matches!(
        event.event_type,
        TwitchEventType::Raid
            | TwitchEventType::Subscribe
            | TwitchEventType::Resubscribe
            | TwitchEventType::GiftSub
    ) {
        return;
    }

    if let Some(sound) = sound_for_twitch_event_type(&event.event_type) {
        SOUND_STORE.play_sound_for_channel(sound, &event.channel);
    }
}

pub(super) fn should_surface_event(event: &TwitchEvent) -> bool {
    match event.event_type {
        TwitchEventType::StreamOnline | TwitchEventType::StreamOffline => {
            if !crate::backend::settings::get_live_notifications_enabled() {
                return false;
            }

            let startup_delay_ms =
                crate::backend::settings::get_live_notification_startup_delay_secs() as u64 * 1000;
            if startup_delay_ms == 0 {
                return true;
            }

            crate::utils::unix_now_ms().saturating_sub(*EVENTSUB_NOTIFICATION_START_MS)
                >= startup_delay_ms
        }
        _ => true,
    }
}

/// Rows IRC already delivers to every client are omitted here. Raid/sub notices and
/// ban/timeout CLEARCHATs reach all chatters, while the matching EventSub families
/// only reach moderators and broadcasters, so inserting them would show elevated
/// users a second copy of every ban and timeout. Unban has no CLEARCHAT counterpart,
/// so EventSub still owns that row (the moderator attribution survives in the mod log).
pub(super) fn should_insert_event_message(event: &TwitchEvent) -> bool {
    matches!(
        event.event_type,
        TwitchEventType::ChannelPointRedemption
            | TwitchEventType::StreamOnline
            | TwitchEventType::StreamOffline
            | TwitchEventType::Unban
            | TwitchEventType::AutomodMessageHold
            | TwitchEventType::AutomodMessageUpdate
            | TwitchEventType::SuspiciousUserMessage
            | TwitchEventType::SuspiciousUserUpdate
            | TwitchEventType::PollBegin
            | TwitchEventType::PollProgress
            | TwitchEventType::PollEnd
            | TwitchEventType::PredictionBegin
            | TwitchEventType::PredictionProgress
            | TwitchEventType::PredictionLock
            | TwitchEventType::PredictionEnd
            | TwitchEventType::HypeTrainBegin
            | TwitchEventType::HypeTrainProgress
            | TwitchEventType::HypeTrainEnd
            | TwitchEventType::VipAdd
            | TwitchEventType::VipRemove
            | TwitchEventType::ModeratorAdd
            | TwitchEventType::ModeratorRemove
            | TwitchEventType::WarningAcknowledge
            | TwitchEventType::WarningSend
            | TwitchEventType::ShieldModeBegin
            | TwitchEventType::ShieldModeEnd
    )
}

pub(super) fn record_mod_log_event(event: &TwitchEvent) {
    let Some((action_type, details)) = event_mod_log_details(event) else {
        return;
    };

    add_mod_action(ModAction::new(
        action_type,
        event
            .user_name
            .clone()
            .unwrap_or_else(|| "unknown user".to_string()),
        event.channel.clone(),
        details,
        crate::utils::unix_now_ms() as i64,
    ));
}

fn event_mod_log_details(event: &TwitchEvent) -> Option<(ModActionType, String)> {
    let username = event.user_name.as_deref().unwrap_or("unknown user");
    let moderator = event.moderator_name.as_deref().unwrap_or("A moderator");
    let reason = event
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();

    match event.event_type {
        TwitchEventType::Ban if event.is_permanent => Some((
            ModActionType::Ban,
            format!("{username} was banned by {moderator}{reason}"),
        )),
        TwitchEventType::Ban => Some((
            ModActionType::Timeout,
            format!("{username} was timed out by {moderator}{reason}"),
        )),
        TwitchEventType::Unban => Some((
            ModActionType::Unban,
            format!("{username} was unbanned by {moderator}"),
        )),
        TwitchEventType::VipAdd => {
            Some((ModActionType::Vip, format!("{username} was made a VIP.")))
        }
        TwitchEventType::VipRemove => Some((
            ModActionType::Unvip,
            format!("{username} is no longer a VIP."),
        )),
        TwitchEventType::ModeratorAdd => Some((
            ModActionType::Mod,
            format!("{username} was added as a moderator."),
        )),
        TwitchEventType::ModeratorRemove => Some((
            ModActionType::Unmod,
            format!("{username} is no longer a moderator."),
        )),
        TwitchEventType::WarningAcknowledge => Some((
            ModActionType::Warning,
            format!("{username} acknowledged a warning."),
        )),
        TwitchEventType::WarningSend => Some((
            ModActionType::Warning,
            format!("{username} was warned by {moderator}{reason}"),
        )),
        TwitchEventType::ShieldModeBegin => Some((
            ModActionType::ShieldEnable,
            format!("Shield Mode was activated by {moderator}."),
        )),
        TwitchEventType::ShieldModeEnd => Some((
            ModActionType::ShieldDisable,
            format!("Shield Mode was deactivated by {moderator}."),
        )),
        _ => None,
    }
}
