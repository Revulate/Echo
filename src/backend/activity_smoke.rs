//! Deterministic process-local fixtures for the native Activity UI smoke run.
//!
//! This module is deliberately inert until the opt-in driver calls [`seed`].
//! It performs no network work, queues no UI action, persists nothing, and
//! temporarily grows the bounded production stores instead of evicting user
//! activity. [`cleanup`] removes only entries matching the returned fixture.

use crate::backend::components::event_notification::{
    RECENT_TWITCH_EVENTS, TwitchEvent as InternalTwitchEvent,
    TwitchEventType as InternalTwitchEventType, mark_recent_event_unread,
    remove_recent_event_unread,
};
use crate::backend::lock_utils::RwLockExt;
use crate::backend::models::ChatMessage;
use crate::backend::{irc::GLOBAL_MENTIONS, whispers};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Stable identities needed by the UI driver to exercise row actions.
#[derive(Clone, Debug)]
pub struct ActivitySmokeFixture {
    pub mention_channel: String,
    pub mention_id: String,
    pub whisper_username: String,
    pub event_timestamp: i64,
    identity: String,
    source_timestamp: i64,
}

/// Activity-source state observed for one fixture and for all retained rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivitySmokeSnapshot {
    pub mention_present: bool,
    pub whisper_present: bool,
    pub whisper_unread: u32,
    pub event_present: bool,
    pub mention_count: usize,
    pub whisper_count: usize,
    pub event_count: usize,
}

/// Seed one mention, one unread whisper, and one Twitch event in memory.
///
/// Identities contain both the process ID and a monotonic process-local
/// sequence, allowing multiple runs in one process to coexist and clean up
/// independently. Production residency limits are not applied here: evicting
/// real user activity would make a supposedly safe smoke run destructive.
pub fn seed() -> ActivitySmokeFixture {
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let identity = format!("echo_activity_smoke_{}_{}", std::process::id(), sequence);
    let mention_channel = format!("{identity}_channel");
    let mention_id = format!("{identity}_mention");
    let whisper_username = format!("{identity}_whisper");
    let source_timestamp = now_millis();

    let mut event_timestamp = source_timestamp.saturating_add(10_000);
    let mut recent_events = RECENT_TWITCH_EVENTS.write_or_recover();
    while recent_events
        .iter()
        .any(|event| event.timestamp == event_timestamp)
    {
        event_timestamp = event_timestamp.saturating_add(1);
    }
    recent_events.push(InternalTwitchEvent {
        event_type: InternalTwitchEventType::StreamOnline,
        channel: mention_channel.clone(),
        timestamp: event_timestamp,
        event_title: Some(identity.clone()),
        event_detail: Some("Deterministic Activity automation event".to_string()),
        ..InternalTwitchEvent::default()
    });
    drop(recent_events);
    mark_recent_event_unread(event_timestamp);

    let mention = ChatMessage {
        id: Arc::from(mention_id.as_str()),
        channel: Arc::from(mention_channel.as_str()),
        sender: Arc::from("echoactivityfixture"),
        display_name: Arc::from("Activity Fixture"),
        content: Arc::from("Deterministic Activity automation mention"),
        color: Some(Arc::from("#9147FF")),
        timestamp: source_timestamp,
        ..ChatMessage::default()
    };
    GLOBAL_MENTIONS
        .write()
        .push_back((mention_channel.clone(), mention));
    crate::backend::irc::mark_global_mention_unread(&mention_channel, &mention_id);

    let inserted_whisper = whispers::insert_activity_smoke_thread(
        &whisper_username,
        "Deterministic Activity automation whisper",
        source_timestamp.saturating_add(1),
    );
    debug_assert!(inserted_whisper, "Activity smoke whisper identity collided");

    ActivitySmokeFixture {
        mention_channel,
        mention_id,
        whisper_username,
        event_timestamp,
        identity,
        source_timestamp: source_timestamp.saturating_add(1),
    }
}

/// Observe fixture presence and current source sizes without mutating state.
pub fn snapshot(fixture: &ActivitySmokeFixture) -> ActivitySmokeSnapshot {
    let mentions = GLOBAL_MENTIONS.read();
    let mention_present = mentions.iter().any(|(channel, message)| {
        channel == &fixture.mention_channel && message.id.as_ref() == fixture.mention_id.as_str()
    });
    let mention_count = mentions.len();
    drop(mentions);

    let (whisper_present, whisper_unread, whisper_count) =
        whispers::activity_smoke_thread_snapshot(&fixture.whisper_username);

    let recent_events = RECENT_TWITCH_EVENTS.read_or_recover();
    let event_present = recent_events
        .iter()
        .any(|event| event_matches(fixture, event));
    let event_count = recent_events.len();

    ActivitySmokeSnapshot {
        mention_present,
        whisper_present,
        whisper_unread,
        event_present,
        mention_count,
        whisper_count,
        event_count,
    }
}

/// Remove only entries created by this fixture and return the resulting state.
pub fn cleanup(fixture: &ActivitySmokeFixture) -> ActivitySmokeSnapshot {
    GLOBAL_MENTIONS.write().retain(|(channel, message)| {
        channel != &fixture.mention_channel || message.id.as_ref() != fixture.mention_id.as_str()
    });
    crate::backend::irc::remove_global_mention_unread(
        &fixture.mention_channel,
        &fixture.mention_id,
    );

    let _ =
        whispers::remove_activity_smoke_thread(&fixture.whisper_username, fixture.source_timestamp);

    RECENT_TWITCH_EVENTS
        .write_or_recover()
        .retain(|event| !event_matches(fixture, event));
    remove_recent_event_unread(fixture.event_timestamp);

    snapshot(fixture)
}

fn event_matches(fixture: &ActivitySmokeFixture, event: &InternalTwitchEvent) -> bool {
    event.timestamp == fixture.event_timestamp
        && event.channel == fixture.mention_channel
        && event.event_title.as_deref() == Some(fixture.identity.as_str())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(1)
}
