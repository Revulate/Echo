//! Room state tracking per channel
//! Parses ROOMSTATE IRC messages to track chat modes (emote-only, sub-only, slow mode, etc.)

use crate::backend::lock_utils::RwLockExt;
use crate::backend::ui_actions::composer_send::{
    EligibilityStatus, SendEligibilityStore, slow_wait_seconds,
};
use rustc_hash::FxHashMap;
use std::sync::LazyLock;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use twitch_irc::message::RoomStateMessage;

/// Current room state for a channel
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomState {
    pub emote_only: bool,
    pub subscribers_only: bool,
    pub r9k: bool,
    pub slow_mode: Option<Duration>,
    pub followers_only: FollowersOnly,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FollowersOnly {
    #[default]
    Disabled,
    /// All followers can chat (0 minute requirement)
    All,
    /// Must follow for at least this duration
    Minimum(Duration),
}

/// Global room state storage
static ROOM_STATES: LazyLock<RwLock<FxHashMap<String, RoomState>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

/// Version counter for room modes and send eligibility (triggers UI refresh)
static ROOM_STATE_VERSION: LazyLock<std::sync::atomic::AtomicU64> =
    LazyLock::new(|| std::sync::atomic::AtomicU64::new(0));

/// Process a ROOMSTATE message and update stored state
pub fn process_room_state(msg: RoomStateMessage) {
    let channel = msg.channel_login.to_lowercase();

    let mut states = ROOM_STATES.write_or_recover();
    let state = states.entry(channel.clone()).or_default();
    let previous = state.clone();

    if let Some(emote_only) = msg.emote_only {
        state.emote_only = emote_only;
    }
    if let Some(subscribers_only) = msg.subscribers_only {
        state.subscribers_only = subscribers_only;
    }
    if let Some(r9k) = msg.r9k {
        state.r9k = r9k;
    }
    if let Some(slow_mode) = msg.slow_mode {
        state.slow_mode = if slow_mode.is_zero() {
            None
        } else {
            Some(slow_mode)
        };
    }
    if let Some(ref followers) = msg.followers_only {
        use twitch_irc::message::FollowersOnlyMode;
        state.followers_only = match followers {
            FollowersOnlyMode::Disabled => FollowersOnly::Disabled,
            FollowersOnlyMode::Enabled(d) if d.is_zero() => FollowersOnly::All,
            FollowersOnlyMode::Enabled(d) => FollowersOnly::Minimum(*d),
        };
    }

    let changed = previous != *state;
    let slow_interval_changed = msg.slow_mode.is_some() && previous.slow_mode != state.slow_mode;
    let clear_slow_cooldowns =
        slow_interval_changed || msg.slow_mode.is_some_and(|slow| slow.is_zero());
    if changed {
        ROOM_STATE_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    crate::log_irc!(
        debug,
        "ROOMSTATE for #{}: emote_only={}, sub_only={}, r9k={}, slow={:?}, followers={:?}",
        channel,
        state.emote_only,
        state.subscribers_only,
        state.r9k,
        state.slow_mode,
        state.followers_only
    );

    drop(states);
    if clear_slow_cooldowns
        && SEND_ELIGIBILITY
            .write_or_recover()
            .clear_slow_channel(&channel)
    {
        publish_eligibility_change(&channel);
    }
    if changed {
        crate::backend::observers::get_observer_registry().notify(
            &channel,
            crate::backend::observers::ChannelEvent::RoomStateChanged,
        );
    }
}

/// Monotonic version of the global room-state store.
///
/// Includes per-account send eligibility so existing split render gates refresh
/// when a role, acknowledgement, or self moderation event changes the composer.
#[inline]
pub fn room_state_version() -> u64 {
    ROOM_STATE_VERSION.load(std::sync::atomic::Ordering::Acquire)
}

/// Get the current room state for a channel
pub fn get_room_state(channel: &str) -> Option<RoomState> {
    let states = ROOM_STATES.read_or_recover();
    states.get(&channel.to_lowercase()).cloned()
}

/// Install deterministic ROOMSTATE only for Echo's opt-in native UI harness.
/// Ordinary runtime callers cannot mutate this store through this path.
pub(crate) fn set_room_state_for_benchmark(channel: &str, state: RoomState) -> bool {
    if !crate::benchmark_ui_fixture::disposable_workspace_enabled() {
        return false;
    }

    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }

    let changed = {
        let mut states = ROOM_STATES.write_or_recover();
        if states.get(&channel) == Some(&state) {
            false
        } else {
            states.insert(channel.clone(), state);
            true
        }
    };

    if changed {
        ROOM_STATE_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::backend::observers::get_observer_registry().notify(
            &channel,
            crate::backend::observers::ChannelEvent::RoomStateChanged,
        );
    }
    changed
}

/// Clear the stored room state for a channel.
pub fn clear_room_state(channel: &str) -> bool {
    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }

    let removed = ROOM_STATES.write_or_recover().remove(&channel).is_some();

    if removed {
        ROOM_STATE_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::backend::observers::get_observer_registry().notify(
            &channel,
            crate::backend::observers::ChannelEvent::RoomStateChanged,
        );
    }

    removed
}

static SEND_ELIGIBILITY: LazyLock<RwLock<SendEligibilityStore>> =
    LazyLock::new(|| RwLock::new(SendEligibilityStore::default()));

fn publish_eligibility_change(channel: &str) {
    ROOM_STATE_VERSION.fetch_add(1, std::sync::atomic::Ordering::Release);
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::RoomStateChanged,
    );
    crate::managers::frame_rate_controller::record_activity();
}

pub(crate) fn current_send_eligibility(channel: &str) -> Option<EligibilityStatus> {
    let account = crate::backend::auth::active_authenticated_user_id()?;
    SEND_ELIGIBILITY
        .write_or_recover()
        .status(&account, channel, Instant::now())
}

pub(crate) fn clear_current_send_restriction(channel: &str) {
    let Some(auth) = crate::backend::auth::capture_authenticated_user_resource() else {
        return;
    };
    auth.commit_if_current(|| clear_send_restriction_for_user(channel, &auth.user_id));
}

pub(crate) fn clear_send_restriction_for_user(channel: &str, account: &str) {
    let changed = SEND_ELIGIBILITY
        .write_or_recover()
        .clear_restriction(account, channel);
    if changed {
        publish_eligibility_change(channel);
    }
}

pub(crate) fn reset_send_exemptions() {
    SEND_ELIGIBILITY.write_or_recover().reset_exemptions();
}

pub(crate) fn note_self_roles(session_login: &str, channel: &str, exempt: bool) {
    let Some(auth) = crate::backend::auth::capture_authenticated_user_resource() else {
        return;
    };
    let changed = auth
        .commit_if_current(|| {
            if !crate::backend::settings::get_username_lowercase()
                .eq_ignore_ascii_case(session_login)
            {
                return false;
            }
            SEND_ELIGIBILITY.write_or_recover().set_exempt(
                &auth.user_id,
                channel,
                exempt,
                Instant::now(),
            )
        })
        .unwrap_or(false);
    if changed {
        publish_eligibility_change(channel);
    }
}

pub(crate) fn record_confirmed_send(account: &str, channel: &str) {
    let slow = get_room_state(channel).and_then(|state| state.slow_mode);
    let fallback_exempt = crate::backend::settings::get_username_lowercase()
        .eq_ignore_ascii_case(channel)
        || super::connection::get_self_state(channel).is_some_and(|(_, badges)| {
            badges.iter().any(|badge| {
                crate::backend::badges::is_moderator_badge_key(badge)
                    || crate::backend::badges::badge_key_matches(badge, "broadcaster")
                    || crate::backend::badges::badge_key_matches(badge, "vip")
            })
        });
    let changed = {
        let mut store = SEND_ELIGIBILITY.write_or_recover();
        let now = Instant::now();
        if !store.exemption_known(account, channel) {
            store.set_exempt(account, channel, fallback_exempt, now);
        }
        store.confirmed_send(account, channel, slow, now)
    };
    if changed {
        publish_eligibility_change(channel);
    }
}

pub(crate) fn note_self_moderation(target_id: &str, channel: &str, timeout: Option<Duration>) {
    let Some(auth) = crate::backend::auth::capture_authenticated_user_resource() else {
        return;
    };
    if auth.user_id != target_id {
        return;
    }
    let changed = auth
        .commit_if_current(|| {
            let mut store = SEND_ELIGIBILITY.write_or_recover();
            match timeout {
                Some(remaining) => store.timeout(target_id, channel, remaining, Instant::now()),
                None => store.ban(target_id, channel, Instant::now()),
            }
        })
        .unwrap_or(false);
    if changed {
        publish_eligibility_change(channel);
    }
}

// Called inside the submitting account's credential fence; never capture auth here.
pub(crate) fn note_send_rejection(account: &str, channel: &str, code: &str, text: &str) {
    let text_lower = text.to_ascii_lowercase();
    let changed = {
        let mut store = SEND_ELIGIBILITY.write_or_recover();
        let now = Instant::now();
        if code == "msg_banned"
            || text_lower.contains("you are banned")
            || text_lower.contains("you have been banned")
            || text_lower.contains("sender is banned")
            || text_lower.contains("user is banned")
        {
            store.ban(account, channel, now)
        } else if (code == "msg_slowmode"
            || text_lower.contains("slow mode")
            || text_lower.contains("slowmode"))
            && let Some(seconds) = slow_wait_seconds(text)
        {
            store.server_slow(account, channel, Duration::from_secs(seconds), now)
        } else if code == "msg_timedout" {
            let remaining = text_lower
                .split_once("timed out for ")
                .and_then(|(_, tail)| {
                    let digits = tail
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>();
                    let suffix = tail[digits.len()..]
                        .trim_start()
                        .trim_start_matches("more ");
                    digits.parse::<u64>().ok().filter(|seconds| {
                        *seconds > 0 && *seconds <= 14 * 24 * 3600 && suffix.starts_with("second")
                    })
                });
            remaining.is_some_and(|seconds| {
                store.timeout(account, channel, Duration::from_secs(seconds), now)
            })
        } else {
            false
        }
    };
    if changed {
        publish_eligibility_change(channel);
    }
}
