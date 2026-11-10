//! The EventSub event feed: recent events, unread counts, and the open
//! prediction outcomes per channel.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::sync::RwLock;

use crate::backend::lock_utils::RwLockExt;

/// Twitch event types
#[derive(Clone, PartialEq, Eq, Debug)]
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PredictionOutcomeSnapshot {
    pub id: String,
    pub title: String,
    pub users: Option<u64>,
    pub channel_points: Option<u64>,
}

#[derive(Clone, PartialEq, Debug)]
struct ActivePredictionCacheEntry {
    outcomes: Vec<PredictionOutcomeSnapshot>,
    updated_at: i64,
}

/// Twitch event data
#[derive(Clone, PartialEq, Debug)]
pub struct TwitchEvent {
    pub event_type: TwitchEventType,
    pub channel: String,
    pub timestamp: i64,
    pub user_name: Option<String>,
    pub user_login: Option<String>,
    pub user_id: Option<String>,
    pub tier: Option<String>,
    pub is_gift: bool,
    pub cumulative_months: Option<u32>,
    pub streak_months: Option<u32>,
    pub message: Option<String>,
    pub total: Option<u32>,
    pub cumulative_total: Option<u32>,
    pub is_anonymous: bool,
    pub from_channel: Option<String>,
    pub viewers: Option<u32>,
    pub reward_id: Option<String>,
    pub reward_title: Option<String>,
    pub reward_cost: Option<u32>,
    pub user_input: Option<String>,
    pub message_id: Option<String>,
    pub moderator_name: Option<String>,
    pub reason: Option<String>,
    pub status: Option<String>,
    pub event_title: Option<String>,
    pub event_detail: Option<String>,
    pub prediction_outcomes: Vec<PredictionOutcomeSnapshot>,
    pub winning_outcome_id: Option<String>,
    pub is_permanent: bool,
}

impl Default for TwitchEvent {
    fn default() -> Self {
        Self {
            event_type: TwitchEventType::Subscribe,
            channel: String::new(),
            timestamp: 0,
            user_name: None,
            user_login: None,
            user_id: None,
            tier: None,
            is_gift: false,
            cumulative_months: None,
            streak_months: None,
            message: None,
            total: None,
            cumulative_total: None,
            is_anonymous: false,
            from_channel: None,
            viewers: None,
            reward_id: None,
            reward_title: None,
            reward_cost: None,
            user_input: None,
            message_id: None,
            moderator_name: None,
            reason: None,
            status: None,
            event_title: None,
            event_detail: None,
            prediction_outcomes: Vec::new(),
            winning_outcome_id: None,
            is_permanent: false,
        }
    }
}

/// Global signal for event notifications
pub static TWITCH_EVENTS: LazyLock<RwLock<Vec<TwitchEvent>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

const MAX_RECENT_EVENTS: usize = 50;
const MAX_ACTIVE_PREDICTION_CHANNELS: usize = 64;

/// Bounded history for Activity Events.
pub static RECENT_TWITCH_EVENTS: LazyLock<RwLock<Vec<TwitchEvent>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Read state is independent from retained event history, matching mentions and
/// whispers: attention can clear without destroying the Activity row.
static UNREAD_RECENT_TWITCH_EVENTS: LazyLock<RwLock<HashSet<i64>>> =
    LazyLock::new(|| RwLock::new(HashSet::with_capacity(MAX_RECENT_EVENTS)));

/// Subset whose EventSub payload was also inserted into chat. Events Twitch IRC
/// already represents remain Activity-only and must not auto-clear just because
/// their channel has a live-following split.
static CHAT_VISIBLE_RECENT_TWITCH_EVENTS: LazyLock<RwLock<HashSet<i64>>> =
    LazyLock::new(|| RwLock::new(HashSet::with_capacity(MAX_RECENT_EVENTS)));

static ACTIVE_PREDICTION_OUTCOMES: LazyLock<RwLock<HashMap<String, ActivePredictionCacheEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn now_millis() -> i64 {
    crate::utils::unix_now_ms() as i64
}

fn normalized_channel(channel: &str) -> String {
    channel.trim().trim_start_matches('#').to_lowercase()
}

fn is_open_prediction_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("ACTIVE") || status.eq_ignore_ascii_case("LOCKED")
}

fn is_automod_event(event: &TwitchEvent) -> bool {
    matches!(
        &event.event_type,
        TwitchEventType::AutomodMessageHold | TwitchEventType::AutomodMessageUpdate
    )
}

fn should_replace_recent_automod_hold(existing: &TwitchEvent, incoming: &TwitchEvent) -> bool {
    matches!(
        (&existing.event_type, &incoming.event_type),
        (
            TwitchEventType::AutomodMessageHold,
            TwitchEventType::AutomodMessageUpdate
        )
    ) && existing.message_id.is_some()
        && existing.message_id == incoming.message_id
}

/// Track the latest open prediction outcomes seen through EventSub.
pub fn update_active_prediction(event: &TwitchEvent) {
    update_active_prediction_in(&ACTIVE_PREDICTION_OUTCOMES, event);
}

fn update_active_prediction_in(
    active_predictions: &RwLock<HashMap<String, ActivePredictionCacheEntry>>,
    event: &TwitchEvent,
) {
    let is_prediction_update = matches!(
        &event.event_type,
        TwitchEventType::PredictionBegin
            | TwitchEventType::PredictionProgress
            | TwitchEventType::PredictionLock
    );
    let should_remove = event.event_type == TwitchEventType::PredictionEnd
        || event
            .status
            .as_deref()
            .is_some_and(|status| !is_open_prediction_status(status));

    if !is_prediction_update && !should_remove {
        return;
    }

    let channel = normalized_channel(&event.channel);
    if channel.is_empty() {
        return;
    }

    let mut active = active_predictions.write_or_recover();

    if should_remove {
        active.remove(&channel);
        return;
    }

    if !is_prediction_update || event.prediction_outcomes.is_empty() {
        return;
    }

    active.insert(
        channel.clone(),
        ActivePredictionCacheEntry {
            outcomes: event.prediction_outcomes.clone(),
            updated_at: now_millis(),
        },
    );

    let overflow = active.len().saturating_sub(MAX_ACTIVE_PREDICTION_CHANNELS);
    for _ in 0..overflow {
        let Some(oldest_channel) = active
            .iter()
            .min_by_key(|(_, entry)| entry.updated_at)
            .map(|(channel, _)| channel.clone())
        else {
            break;
        };
        active.remove(&oldest_channel);
    }
}

pub fn active_prediction_outcomes(channel: &str) -> Vec<PredictionOutcomeSnapshot> {
    active_prediction_outcomes_from(&ACTIVE_PREDICTION_OUTCOMES, channel)
}

fn active_prediction_outcomes_from(
    active_predictions: &RwLock<HashMap<String, ActivePredictionCacheEntry>>,
    channel: &str,
) -> Vec<PredictionOutcomeSnapshot> {
    let channel = normalized_channel(channel);
    if channel.is_empty() {
        return Vec::new();
    }

    active_predictions
        .read_or_recover()
        .get(&channel)
        .map(|entry| entry.outcomes.clone())
        .unwrap_or_default()
}

/// Add a new event to the queue
pub fn push_event(event: TwitchEvent) {
    push_event_into(
        event,
        &RECENT_TWITCH_EVENTS,
        &UNREAD_RECENT_TWITCH_EVENTS,
        &CHAT_VISIBLE_RECENT_TWITCH_EVENTS,
        &TWITCH_EVENTS,
        || {
            crate::state::ui_invalidation::request(
                crate::state::ui_invalidation::UiInvalidation::TopBarState,
            );
        },
    );
}

fn push_event_into(
    mut event: TwitchEvent,
    recent_events: &RwLock<Vec<TwitchEvent>>,
    unread_events: &RwLock<HashSet<i64>>,
    chat_visible_events: &RwLock<HashSet<i64>>,
    pending_events: &RwLock<Vec<TwitchEvent>>,
    invalidate: impl FnOnce(),
) {
    if event.timestamp <= 0 {
        event.timestamp = now_millis();
    }

    let mut replaced_timestamps = Vec::new();
    let mut evicted_timestamps = Vec::new();
    let mut recent = recent_events.write_or_recover();
    if matches!(event.event_type, TwitchEventType::AutomodMessageUpdate) {
        replaced_timestamps.extend(
            recent
                .iter()
                .filter(|existing| should_replace_recent_automod_hold(existing, &event))
                .map(|existing| existing.timestamp),
        );
        recent.retain(|existing| !should_replace_recent_automod_hold(existing, &event));
    }
    recent.push(event.clone());
    let overflow = recent.len().saturating_sub(MAX_RECENT_EVENTS);
    if overflow > 0 {
        evicted_timestamps.extend(recent.drain(0..overflow).map(|event| event.timestamp));
    }
    drop(recent);

    let mut unread = unread_events.write_or_recover();
    for timestamp in replaced_timestamps.iter().chain(evicted_timestamps.iter()) {
        unread.remove(timestamp);
    }
    unread.insert(event.timestamp);
    drop(unread);

    let mut chat_visible = chat_visible_events.write_or_recover();
    for timestamp in replaced_timestamps.iter().chain(evicted_timestamps.iter()) {
        chat_visible.remove(timestamp);
    }
    drop(chat_visible);

    invalidate();

    pending_events.write_or_recover().push(event);
}

/// Pop all pending events (drains the queue)
pub fn pop_events() -> Vec<TwitchEvent> {
    std::mem::take(&mut *TWITCH_EVENTS.write_or_recover())
}

/// Return newest events first without draining the history.
pub fn recent_events(limit: usize) -> Vec<TwitchEvent> {
    if limit == 0 {
        return Vec::new();
    }

    RECENT_TWITCH_EVENTS
        .read_or_recover()
        .iter()
        .rev()
        .take(limit)
        .cloned()
        .collect()
}

pub fn unread_recent_event_count() -> usize {
    UNREAD_RECENT_TWITCH_EVENTS.read_or_recover().len()
}

pub fn mark_all_recent_events_read() -> bool {
    let mut unread = UNREAD_RECENT_TWITCH_EVENTS.write_or_recover();
    let changed = !unread.is_empty();
    unread.clear();
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

/// A live-following split necessarily paints newly appended EventSub system rows.
/// Mark events in those exact channels read without affecting scrolled-up splits.
pub fn mark_recent_events_read_for_channels(channels: &HashSet<String>) -> bool {
    if channels.is_empty() {
        return false;
    }
    let chat_visible = CHAT_VISIBLE_RECENT_TWITCH_EVENTS.read_or_recover().clone();
    let visible_timestamps: HashSet<_> = RECENT_TWITCH_EVENTS
        .read_or_recover()
        .iter()
        .filter(|event| {
            chat_visible.contains(&event.timestamp)
                && channels.contains(&normalized_channel(&event.channel))
        })
        .map(|event| event.timestamp)
        .collect();
    if visible_timestamps.is_empty() {
        return false;
    }
    let mut unread = UNREAD_RECENT_TWITCH_EVENTS.write_or_recover();
    let before = unread.len();
    unread.retain(|timestamp| !visible_timestamps.contains(timestamp));
    let changed = unread.len() != before;
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

pub(crate) fn mark_recent_event_chat_visible(timestamp: i64) {
    if timestamp > 0 {
        CHAT_VISIBLE_RECENT_TWITCH_EVENTS
            .write_or_recover()
            .insert(timestamp);
    }
}

pub(crate) fn mark_recent_event_unread(timestamp: i64) {
    if timestamp > 0 {
        UNREAD_RECENT_TWITCH_EVENTS
            .write_or_recover()
            .insert(timestamp);
    }
}

pub(crate) fn remove_recent_event_unread(timestamp: i64) {
    UNREAD_RECENT_TWITCH_EVENTS
        .write_or_recover()
        .remove(&timestamp);
    CHAT_VISIBLE_RECENT_TWITCH_EVENTS
        .write_or_recover()
        .remove(&timestamp);
}

/// Clear in-app notification history.
pub fn clear_recent_events() {
    let mut events = RECENT_TWITCH_EVENTS.write_or_recover();
    events.clear();
    let mut unread = UNREAD_RECENT_TWITCH_EVENTS.write_or_recover();
    unread.clear();
    drop(unread);
    let mut chat_visible = CHAT_VISIBLE_RECENT_TWITCH_EVENTS.write_or_recover();
    chat_visible.clear();
    drop(chat_visible);
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

/// Clear AutoMod entries from the in-app notification history.
pub fn clear_recent_automod_events() {
    let mut events = RECENT_TWITCH_EVENTS.write_or_recover();
    let removed_timestamps: HashSet<_> = events
        .iter()
        .filter(|event| is_automod_event(event))
        .map(|event| event.timestamp)
        .collect();
    events.retain(|event| !is_automod_event(event));
    let mut unread = UNREAD_RECENT_TWITCH_EVENTS.write_or_recover();
    unread.retain(|timestamp| !removed_timestamps.contains(timestamp));
    drop(unread);
    let mut chat_visible = CHAT_VISIBLE_RECENT_TWITCH_EVENTS.write_or_recover();
    chat_visible.retain(|timestamp| !removed_timestamps.contains(timestamp));
    drop(chat_visible);
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

/// Remove one in-app notification history entry.
pub fn remove_recent_event(timestamp: i64) -> bool {
    if timestamp <= 0 {
        return false;
    }

    let mut events = RECENT_TWITCH_EVENTS.write_or_recover();

    let before = events.len();
    events.retain(|event| event.timestamp != timestamp);
    let changed = events.len() != before;
    if changed {
        UNREAD_RECENT_TWITCH_EVENTS
            .write_or_recover()
            .remove(&timestamp);
        CHAT_VISIBLE_RECENT_TWITCH_EVENTS
            .write_or_recover()
            .remove(&timestamp);
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::{
        ActivePredictionCacheEntry, MAX_RECENT_EVENTS, PredictionOutcomeSnapshot, TwitchEvent,
        TwitchEventType, active_prediction_outcomes_from, push_event_into,
        update_active_prediction_in,
    };
    use crate::backend::lock_utils::RwLockExt;
    use core::prelude::v1::test;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, RwLock};

    fn poison<T: Send + Sync + 'static>(lock: Arc<RwLock<T>>) {
        let result = std::thread::spawn(move || {
            let _guard = lock.write().expect("test lock should be available");
            panic!("deliberately poison the lock");
        })
        .join();
        assert!(result.is_err());
    }

    fn event(timestamp: i64) -> TwitchEvent {
        TwitchEvent {
            event_type: TwitchEventType::StreamOnline,
            channel: "echo".to_string(),
            timestamp,
            ..TwitchEvent::default()
        }
    }

    #[test]
    fn poisoned_recent_and_pending_locks_keep_new_event_unread_and_bounded() {
        let recent = Arc::new(RwLock::new(
            (1..=MAX_RECENT_EVENTS as i64).map(event).collect(),
        ));
        let unread = Arc::new(RwLock::new(HashSet::from([1])));
        let chat_visible = Arc::new(RwLock::new(HashSet::from([1])));
        let pending = Arc::new(RwLock::new(Vec::new()));
        poison(Arc::clone(&recent));
        poison(Arc::clone(&pending));

        let incoming = event(MAX_RECENT_EVENTS as i64 + 1);
        push_event_into(
            incoming.clone(),
            &recent,
            &unread,
            &chat_visible,
            &pending,
            || {},
        );

        let recent = recent.read_or_recover();
        assert_eq!(recent.len(), MAX_RECENT_EVENTS);
        assert!(!recent.iter().any(|event| event.timestamp == 1));
        assert!(recent.iter().any(|event| event == &incoming));
        drop(recent);
        let unread = unread.read_or_recover();
        assert!(!unread.contains(&1));
        assert!(unread.contains(&incoming.timestamp));
        drop(unread);
        assert!(chat_visible.read_or_recover().is_empty());
        assert_eq!(&*pending.read_or_recover(), &[incoming]);
    }

    #[test]
    fn poisoned_active_prediction_map_accepts_begin_then_end() {
        let active = Arc::new(RwLock::new(
            HashMap::<String, ActivePredictionCacheEntry>::new(),
        ));
        poison(Arc::clone(&active));
        let outcomes = vec![PredictionOutcomeSnapshot {
            id: "win".to_string(),
            title: "Win".to_string(),
            users: Some(1),
            channel_points: Some(100),
        }];
        let begin = TwitchEvent {
            event_type: TwitchEventType::PredictionBegin,
            channel: "#echo".to_string(),
            prediction_outcomes: outcomes.clone(),
            ..TwitchEvent::default()
        };

        update_active_prediction_in(&active, &begin);
        assert_eq!(active_prediction_outcomes_from(&active, "echo"), outcomes);

        update_active_prediction_in(
            &active,
            &TwitchEvent {
                event_type: TwitchEventType::PredictionEnd,
                channel: "echo".to_string(),
                ..TwitchEvent::default()
            },
        );
        assert!(active_prediction_outcomes_from(&active, "echo").is_empty());
    }
}
