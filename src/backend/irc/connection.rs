//! IRC connection and message handling for Twitch chat.
//!
#![allow(clippy::type_complexity)]

use super::tmi_adapter::{format_badge, format_color_rgb};
use super::user_colors::cache_user_color;
use crate::backend::badges::TWITCH_BADGE_CACHE;
use crate::backend::components::mod_log::{add_mod_action, parse_mod_action};
use crate::backend::deduplicator::MessageDeduplicator;
use crate::backend::emotes::EMOTE_CACHE;
use crate::backend::filters::{FILTER_STORE, FilterAction, TestContext};
use crate::backend::highlight_decision::{
    PrivmsgFilterOutcome, PrivmsgHighlightAlert, PrivmsgHighlightSignals, PrivmsgHighlightSound,
    decide_privmsg_highlight, fold_privmsg_filter_results, is_mention_highlight,
    privmsg_shows_in_mentions,
};
use crate::backend::models::{ChatMessage, HighlightType, ReplyInfo, SystemMessageType};
use crate::backend::notifications::{
    HighlightNotification, MentionNotification, notify_highlight, notify_mention, notify_whisper,
};
use crate::backend::settings::{get_token, get_username, get_username_lowercase};
use crate::backend::similarity::{SimilarityCandidate, detect_similarity, get_similarity_settings};
use crate::backend::slots::MessageSlot;
use crate::backend::slots::parser::{parse_emotes_tag_with_offset, parse_to_slots};
use crate::backend::slots::{compute_structure_hash, slots_semantically_equal};
use crate::backend::sound_events::sound_for_highlight_type;
use crate::backend::sounds::{SOUND_STORE, SoundEventType};
use crate::backend::twitch_api::{HelixAuthExt, TWITCH_HTTP_CLIENT, get_user_id_cached};
use crate::backend::usernotice_core::{self, UsernoticeHighlightKind, UsernoticeTags};
use crate::state::{MessageBuffer, MessageReconcileAction};
use chrono::Utc;
use dashmap::DashMap;
use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use twitch_irc::ClientConfig;
use twitch_irc::TwitchIRCClient;
use twitch_irc::login::StaticLoginCredentials;
use twitch_irc::message::ServerMessage;
use uuid::Uuid;

#[path = "join_scheduler.rs"]
mod join_scheduler;

type EchoIrcTransport = join_scheduler::JoinTransport;

const WATCHDOG_TIMEOUT_MS: u64 = 90_000;
const RESTART_DEBOUNCE_MS: u64 = 30_000;
const CONNECTION_ESCALATION_MS: u64 = 60_000;

const RECONNECT_MIN_DELAY_MS: u64 = 1_000;
const RECONNECT_MAX_DELAY_MS: u64 = 300_000;
const RECONNECT_BACKOFF_FACTOR: f64 = 2.0;
const RECONNECT_JITTER_MS: u64 = 500;

const DEDUP_MAX_SIZE: usize = 10_000;
const DEDUP_MAX_CHANNELS: usize = 50;
const PARTICIPATED_REPLY_SCAN_LIMIT: usize = 1000;

const CACHED_CHANNEL_EMOTE_REFRESH_BASE_DELAY_MS: u64 = 750;
const CACHED_CHANNEL_EMOTE_REFRESH_STAGGER_MS: u64 = 500;
const CACHED_CHANNEL_EMOTE_INITIAL_STAGGER_MS: u64 = 100;
const CACHED_CHANNEL_EMOTE_REFRESH_MAX_DELAY_MS: u64 = 4_000;
const COALESCED_CHANNEL_EMOTE_RETRY_DELAY_MS: u64 = 7_000;

/// Per-channel message deduplicator (DashMap provides per-shard locking)
static MESSAGE_DEDUP: LazyLock<DashMap<String, MessageDeduplicator>> =
    LazyLock::new(|| DashMap::with_capacity(DEDUP_MAX_CHANNELS));

/// One recent chatter, keyed by lowercase login in [`RECENT_CHATTERS`].
struct RecentChatter {
    display_name: String,
    color: Option<String>,
    /// Observed messages, used to rank mention completion.
    message_count: u32,
}

/// Recent chatters per channel for @username completion, most recent first.
///
/// PERF: an LRU keyed by login, so recording a chatter on every PRIVMSG is a
/// hash lookup instead of a scan and shift of up to `RECENT_CHATTERS_MAX`
/// entries. It grows on demand and is trimmed to `RECENT_CHATTERS_MAX`.
static RECENT_CHATTERS: LazyLock<DashMap<String, LruCache<String, RecentChatter>>> =
    LazyLock::new(DashMap::new);

struct AccountMentionMatcher {
    login: String,
    display_name: String,
    pattern: Option<regex::Regex>,
    /// When the display name was last read from settings; this runs for every
    /// message, so the settings lock is taken at most once per interval.
    checked_at: std::time::Instant,
}

const ACCOUNT_MENTION_NAME_RECHECK: std::time::Duration = std::time::Duration::from_secs(30);

impl AccountMentionMatcher {
    fn new(login: &str, display_name: &str) -> Self {
        let mut names = regex::escape(login);
        if !display_name.is_empty() && !display_name.eq_ignore_ascii_case(login) {
            names.push('|');
            names.push_str(&regex::escape(display_name));
        }
        let pattern = (!login.is_empty())
            .then(|| {
                regex::RegexBuilder::new(&format!(r"\b(?:{names})\b"))
                    .case_insensitive(true)
                    .unicode(true)
                    .build()
                    .ok()
            })
            .flatten();
        Self {
            login: login.to_string(),
            display_name: display_name.to_string(),
            pattern,
            checked_at: std::time::Instant::now(),
        }
    }

    fn is_match(&self, text: &str) -> bool {
        self.pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(text))
    }
}

static ACCOUNT_MENTION_MATCHER: LazyLock<RwLock<Option<AccountMentionMatcher>>> =
    LazyLock::new(|| RwLock::new(None));

pub(crate) fn contains_account_mention(text: &str, login: &str) -> bool {
    use crate::backend::lock_utils::MutexExt;

    if login.is_empty() {
        return false;
    }
    {
        let cache = ACCOUNT_MENTION_MATCHER.read();
        if let Some(matcher) = cache.as_ref()
            && matcher.login == login
            && matcher.checked_at.elapsed() < ACCOUNT_MENTION_NAME_RECHECK
        {
            return matcher.is_match(text);
        }
    }
    let display_name = crate::backend::settings::SETTINGS
        .lock_or_recover()
        .accounts
        .iter()
        .find(|account| account.is_active && account.username.eq_ignore_ascii_case(login))
        .and_then(|account| account.display_name.as_deref())
        .unwrap_or_default()
        .trim()
        .to_string();
    {
        let mut cache = ACCOUNT_MENTION_MATCHER.write();
        if let Some(matcher) = cache.as_mut()
            && matcher.login == login
            && matcher.display_name == display_name
        {
            matcher.checked_at = std::time::Instant::now();
            return matcher.is_match(text);
        }
    }
    let matcher = AccountMentionMatcher::new(login, &display_name);
    let matched = matcher.is_match(text);
    *ACCOUNT_MENTION_MATCHER.write() = Some(matcher);
    matched
}

struct RankedChatterCandidate {
    recency_rank: usize,
    message_count: u32,
    login: String,
    display_name: String,
    color: Option<String>,
}

fn compare_ranked_chatters(
    left: &RankedChatterCandidate,
    right: &RankedChatterCandidate,
) -> std::cmp::Ordering {
    right
        .message_count
        .cmp(&left.message_count)
        .then_with(|| left.recency_rank.cmp(&right.recency_rank))
        .then_with(|| left.login.cmp(&right.login))
}

fn compare_ranked_chatter_fields(
    message_count: u32,
    recency_rank: usize,
    login: &str,
    right: &RankedChatterCandidate,
) -> std::cmp::Ordering {
    right
        .message_count
        .cmp(&message_count)
        .then_with(|| recency_rank.cmp(&right.recency_rank))
        .then_with(|| login.cmp(&right.login))
}

#[inline]
fn chatter_prefix_matches(value: &str, normalized_prefix: &str) -> bool {
    if normalized_prefix.is_ascii() {
        value
            .as_bytes()
            .get(..normalized_prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(normalized_prefix.as_bytes()))
    } else {
        value.to_lowercase().starts_with(normalized_prefix)
    }
}

const RECENT_CHATTERS_MAX: usize = 2_000;

static DEFERRED_CHANNEL_EMOTE_REFRESHES: LazyLock<DashMap<String, ()>> =
    LazyLock::new(DashMap::new);
static DEFERRED_CHANNEL_EMOTE_REFRESH_SLOT: AtomicU64 = AtomicU64::new(0);

/// Self user's state per channel (color, badges) - updated from USERSTATE messages
/// Key: channel name (lowercase), Value: (color, badges as Vec<String>)
static SELF_CHANNEL_STATE: LazyLock<Mutex<FxHashMap<String, (Option<String>, Vec<String>)>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// Compatibility state for historic rows created by older application builds.
/// New sends never create an optimistic row; they publish only after Helix
/// supplies Twitch's real message ID.
#[derive(Clone)]
struct PendingOptimistic {
    content: String,
    timestamp_ms: i64,
    optimistic_id: Arc<str>,
}

static PENDING_OPTIMISTIC: LazyLock<Mutex<FxHashMap<String, VecDeque<PendingOptimistic>>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));
const OPTIMISTIC_MESSAGE_MAX_AGE_MS: i64 = 10_000;
const OPTIMISTIC_HISTORY_RECONCILE_WINDOW_MS: i64 = 60_000;

const MAX_PENDING_MESSAGE_EMOTE_SCOPES: usize = 1_024;

/// Shared-chat source rooms whose login is being resolved (with the messages
/// waiting on it) or recently failed to resolve, so a burst of messages from an
/// unresolved room issues one Helix lookup instead of one per message.
enum SourceRoomLookup {
    InFlight(Vec<(String, String)>),
    /// When the lookup failed, and the proxy generation it failed under: a
    /// routing change makes the failure stale.
    Failed(std::time::Instant, u64),
}

impl SourceRoomLookup {
    fn is_fresh_failure(&self, generation: u64) -> bool {
        matches!(self, SourceRoomLookup::Failed(at, failed_generation)
            if *failed_generation == generation && at.elapsed() < SOURCE_ROOM_LOOKUP_RETRY)
    }
}

static SHARED_SOURCE_ROOM_LOOKUPS: LazyLock<Mutex<HashMap<String, SourceRoomLookup>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SOURCE_ROOM_WAITER_OVERFLOW_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
const SOURCE_ROOM_LOOKUP_RETRY: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const MAX_SOURCE_ROOM_LOOKUPS: usize = 256;
const MAX_SOURCE_ROOM_WAITERS: usize = 256;

/// Register `message_id` as waiting on `source_id`; true when the caller must
/// start the lookup. The map is hard-capped: when full of in-flight lookups a
/// new source room is not looked up.
fn begin_source_room_lookup(source_id: &str, host_channel: &str, message_id: &str) -> bool {
    let generation = crate::backend::http::proxy_generation();
    let mut lookups = SHARED_SOURCE_ROOM_LOOKUPS.lock();
    match lookups.get_mut(source_id) {
        Some(SourceRoomLookup::InFlight(waiters)) => {
            if waiters.len() < MAX_SOURCE_ROOM_WAITERS {
                waiters.push((host_channel.to_string(), message_id.to_string()));
            } else if !SOURCE_ROOM_WAITER_OVERFLOW_LOGGED
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                crate::log_irc!(
                    debug,
                    "Shared-chat source room {} has {} waiting messages; dropping further waiters",
                    source_id,
                    MAX_SOURCE_ROOM_WAITERS
                );
            }
            return false;
        }
        Some(lookup) if lookup.is_fresh_failure(generation) => return false,
        _ => {}
    }
    if !lookups.contains_key(source_id) && lookups.len() >= MAX_SOURCE_ROOM_LOOKUPS {
        lookups.retain(|_, lookup| {
            matches!(lookup, SourceRoomLookup::InFlight(_)) || lookup.is_fresh_failure(generation)
        });
        if lookups.len() >= MAX_SOURCE_ROOM_LOOKUPS {
            let oldest_failed = lookups
                .iter()
                .filter_map(|(id, lookup)| match lookup {
                    SourceRoomLookup::Failed(at, _) => Some((*at, id.clone())),
                    SourceRoomLookup::InFlight(_) => None,
                })
                .min_by_key(|(at, _)| *at)
                .map(|(_, id)| id);
            match oldest_failed {
                Some(id) => {
                    lookups.remove(&id);
                }
                None => return false,
            }
        }
    }
    lookups.insert(
        source_id.to_string(),
        SourceRoomLookup::InFlight(vec![(host_channel.to_string(), message_id.to_string())]),
    );
    true
}

/// Finish a lookup: returns the waiting messages; a failure is remembered.
fn finish_source_room_lookup(source_id: &str, resolved: bool) -> Vec<(String, String)> {
    let mut lookups = SHARED_SOURCE_ROOM_LOOKUPS.lock();
    let previous = if resolved {
        lookups.remove(source_id)
    } else {
        lookups.insert(
            source_id.to_string(),
            SourceRoomLookup::Failed(
                std::time::Instant::now(),
                crate::backend::http::proxy_generation(),
            ),
        )
    };
    match previous {
        Some(SourceRoomLookup::InFlight(waiters)) => waiters,
        _ => Vec::new(),
    }
}
static PENDING_MESSAGE_EMOTE_SCOPES: LazyLock<Mutex<HashMap<(String, String), Arc<str>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Sending chat through Helix is serialized per account/channel, and bounded so
/// repeated Ctrl+Enter cannot accumulate an unbounded backlog behind Twitch.
const MAX_PENDING_CONFIRMED_SENDS_PER_LANE: usize = 8;

struct ConfirmedSendLane {
    capacity: Arc<tokio::sync::Semaphore>,
    serial: tokio::sync::Mutex<()>,
}

static CONFIRMED_SEND_LANES: LazyLock<DashMap<(String, String), Arc<ConfirmedSendLane>>> =
    LazyLock::new(DashMap::new);

/// Accepted Helix sends whose IRC echo has not yet supplied final Twitch tags.
/// This is bounded and keyed by the actual Twitch id, never message text.
const MAX_CONFIRMED_LOCAL_ECHOES: usize = 2_048;
static CONFIRMED_LOCAL_ECHOES: LazyLock<Mutex<LruCache<(String, String), ()>>> =
    LazyLock::new(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(MAX_CONFIRMED_LOCAL_ECHOES).unwrap(),
        ))
    });

/// Per-channel disconnect state for gap-filling
/// Tracks when connection was lost and the last message timestamp for each channel
#[derive(Clone, Debug)]
struct ChannelDisconnectState {
    /// When we lost the connection (Unix timestamp in milliseconds)
    disconnect_time_ms: u64,
    /// Timestamp of the last message we received before disconnect
    last_message_time_ms: i64,
}

/// Per-channel disconnect states for reconnect gap-filling
static CHANNEL_DISCONNECT_STATES: LazyLock<RwLock<FxHashMap<String, ChannelDisconnectState>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

/// Channels whose echoed self-JOIN we have already seen on the current IRC
/// session. twitch-irc's pool reconnects and re-JOINs internally without ending
/// the message stream, so a *second* self-JOIN is the only reconnect signal
/// Echo gets for an outage shorter than the 90s silence watchdog.
static SELF_JOINED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Channels with a re-JOIN gap-fill already running. A burst of re-JOINs must
/// not stack overlapping Helix fetches that would all resolve to the same rows.
static REJOIN_GAP_FILL_INFLIGHT: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Channels that re-JOINed again while their gap-fill was still in flight. The
/// running task rechecks this and makes one more pass, so a second outage is
/// deferred rather than dropped.
static REJOIN_GAP_FILL_PENDING: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Timestamp when connection was lost (for escalation tracking)
static CONNECTION_LOST_TIME: AtomicU64 = AtomicU64::new(0);

/// Current reconnection delay for read connection (exponential backoff state)
static READ_RECONNECT_DELAY: AtomicU64 = AtomicU64::new(RECONNECT_MIN_DELAY_MS);

/// Flag to signal read connection should shutdown cleanly
static READ_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Flag indicating if a supervisor is already running (only ONE should run)
/// Uses AtomicBool to avoid TOCTOU race condition
static SUPERVISOR_RUNNING: AtomicBool = AtomicBool::new(false);

/// Flag to force anonymous mode after auth failure
static USE_ANONYMOUS_MODE: AtomicBool = AtomicBool::new(false);
/// Process environment is immutable for Echo's lifetime. Cache this once so
/// the message hot path never queries the environment.
static FORCE_ANONYMOUS_MODE: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("FORCE_ANON_IRC").is_some());

/// Reset anonymous mode flag (call after successful re-auth)
pub fn reset_anonymous_mode() {
    if USE_ANONYMOUS_MODE.swap(false, Ordering::Release) {
        crate::log_irc!(info, "Anonymous mode reset - authenticated mode restored");
        crate::backend::eventsub::notify_auth_readiness_changed();
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

/// Set runtime anonymous mode without changing persisted settings.
pub fn set_runtime_anonymous_mode(enabled: bool) {
    if enabled {
        if !USE_ANONYMOUS_MODE.swap(true, Ordering::Release) {
            crate::log_irc!(info, "Anonymous mode enabled for IRC runtime");
            crate::backend::eventsub::notify_auth_readiness_changed();
        }
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    } else {
        reset_anonymous_mode();
    }
}

/// Check if anonymous mode is currently active
pub fn is_anonymous_mode() -> bool {
    USE_ANONYMOUS_MODE.load(Ordering::Acquire)
        || crate::backend::settings::get_anonymous_mode()
        || *FORCE_ANONYMOUS_MODE
}

/// Record per-channel disconnect state for gap-filling
/// Called when the read connection is lost to capture the last message timestamp
///
/// The earliest unconsumed state wins: a gap that began at system sleep, or
/// before a failed gap-fill, must not shrink when the dead socket is noticed.
fn record_channel_disconnect(channel: &str) {
    let channel_lower = channel.to_lowercase();
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;

    let last_message_time_ms = CHAT_MESSAGES
        .get(&channel_lower)
        .and_then(|buffer| buffer.iter().last().map(|m| m.timestamp))
        .unwrap_or(0);

    let state = ChannelDisconnectState {
        disconnect_time_ms: now_ms,
        last_message_time_ms,
    };

    CHANNEL_DISCONNECT_STATES
        .write()
        .entry(channel_lower.clone())
        .or_insert(state);
    crate::log_irc!(
        debug,
        "Recorded disconnect state for {}: last_msg_ts={}",
        channel_lower,
        last_message_time_ms
    );
}

/// Claim a channel's disconnect state for gap-fill, removing it from the shared
/// map in the same lock.
///
/// Taking rather than reading-then-clearing is what keeps two producers from
/// clobbering each other. The supervisor's reconnect path and the self-JOIN
/// path both record a state and then run an async fetch; with read-then-clear,
/// a fetch that was still in flight would delete whichever state was recorded
/// while it slept, and that outage would never be filled. Whoever takes a state
/// owns exactly that one, so a state recorded afterwards always survives for
/// its own pass.
fn take_channel_disconnect_state(channel: &str) -> Option<ChannelDisconnectState> {
    CHANNEL_DISCONNECT_STATES
        .write()
        .remove(&channel.to_lowercase())
}

/// Restore a claimed state after a failed fetch so a later pass can still cover
/// the gap -- unless a newer disconnect was recorded meanwhile, which describes
/// a strictly later outage and must not be overwritten by the stale one.
fn restore_channel_disconnect_state(channel: &str, state: ChannelDisconnectState) {
    CHANNEL_DISCONNECT_STATES
        .write()
        .entry(channel.to_lowercase())
        .or_insert(state);
}

/// Record disconnect state for all currently connected channels
fn record_all_channel_disconnects() {
    let channels: Vec<String> = CONNECTED_CHANNELS.read().iter().cloned().collect();

    for channel in channels {
        record_channel_disconnect(&channel);
    }
}

/// Forget every self-JOIN seen on the retired IRC session so a deliberate
/// account switch or supervisor reconnect does not look like an outage.
fn reset_self_joined_channels() {
    join_scheduler::reset_confirmed_memberships();
    REJOIN_GAP_FILL_INFLIGHT.write().clear();
    REJOIN_GAP_FILL_PENDING.write().clear();
}

/// Handle the echoed JOIN for our own login. The first one just records that we
/// are in the channel; a repeat means twitch-irc silently swapped the pool
/// connection underneath us, which the supervisor never observes because the
/// message stream survives it. Gap-fill that one channel so the outage does not
/// leave a permanent hole in the buffer.
fn handle_self_join(channel: &str) {
    let channel_norm = channel.trim_start_matches('#').to_lowercase();
    if channel_norm.is_empty() || !is_channel_desired(&channel_norm) {
        return;
    }

    let Some(first_join) = join_scheduler::commit_self_join(&channel_norm) else {
        return;
    };
    let pending = SESSION_PENDING_JOIN_CHANNELS.write().remove(&channel_norm);
    let announce = SESSION_PENDING_ANNOUNCE_CHANNELS
        .write()
        .remove(&channel_norm);
    if pending {
        handle_session_channel_joined(channel_norm, announce);
        return;
    }
    if announce {
        add_connection_status_message(&channel_norm, "Connected to chat.", "connected");
    }
    if first_join && !CHANNEL_DISCONNECT_STATES.read().contains_key(&channel_norm) {
        return;
    }

    record_channel_disconnect(&channel_norm);

    if !REJOIN_GAP_FILL_INFLIGHT
        .write()
        .insert(channel_norm.clone())
    {
        REJOIN_GAP_FILL_PENDING.write().insert(channel_norm);
        return;
    }

    crate::log_irc!(
        info,
        "Self re-JOIN for {} - gap-filling a silent pool reconnect",
        channel_norm
    );

    tokio::spawn(async move {
        loop {
            match fill_in_missing_messages(&channel_norm).await {
                Ok(count) => {
                    if count > 0 {
                        crate::log_irc!(
                            info,
                            "Gap-filled {} messages for {} after re-JOIN",
                            count,
                            channel_norm
                        );
                    }
                }
                Err(e) => {
                    crate::log_irc!(warn, "Re-JOIN gap-fill failed for {}: {}", channel_norm, e);
                }
            }
            if !REJOIN_GAP_FILL_PENDING.write().remove(&channel_norm) {
                break;
            }
        }
        REJOIN_GAP_FILL_INFLIGHT.write().remove(&channel_norm);
    });
}

/// Bump chat version to trigger UI refresh.
/// Delegates to VersionRegistry for centralized version management.
pub fn bump_chat_version() {
    crate::state::VersionRegistry::bump_chat_version();
}

/// Connection status enum matching SolidJS implementation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    /// Successfully connected to IRC
    Connected,
    /// Attempting to connect or reconnect
    #[default]
    Reconnecting,
    /// Disconnected (failed to connect or lost connection for too long)
    Disconnected,
}

pub static CHAT_MESSAGES: LazyLock<DashMap<String, MessageBuffer>> = LazyLock::new(DashMap::new);

const MAX_ACTIVITY_MENTIONS: usize = 500;

pub static GLOBAL_MENTIONS: LazyLock<RwLock<VecDeque<(String, ChatMessage)>>> =
    LazyLock::new(|| RwLock::new(VecDeque::with_capacity(MAX_ACTIVITY_MENTIONS)));

/// Unread keys are kept separately from the bounded Activity history so
/// observing a mention can clear attention without deleting its useful row.
static GLOBAL_UNREAD_MENTIONS: LazyLock<RwLock<HashSet<(String, String)>>> =
    LazyLock::new(|| RwLock::new(HashSet::with_capacity(MAX_ACTIVITY_MENTIONS)));

fn global_mention_key(channel: &str, message_id: &str) -> (String, String) {
    (
        channel.trim_start_matches('#').to_lowercase(),
        message_id.to_string(),
    )
}

pub(crate) fn mark_global_mention_unread(channel: &str, message_id: &str) {
    if message_id.is_empty() {
        return;
    }
    GLOBAL_UNREAD_MENTIONS
        .write()
        .insert(global_mention_key(channel, message_id));
    crate::backend::activity_persistence::mark_dirty();
}

pub(crate) fn remove_global_mention_unread(channel: &str, message_id: &str) -> bool {
    let changed = GLOBAL_UNREAD_MENTIONS
        .write()
        .remove(&global_mention_key(channel, message_id));
    // Removing a retained row also calls this when it was already read.
    crate::backend::activity_persistence::mark_dirty();
    changed
}

pub(crate) fn clear_global_mention_unread() -> bool {
    let mut unread = GLOBAL_UNREAD_MENTIONS.write();
    let changed = !unread.is_empty();
    unread.clear();
    crate::backend::activity_persistence::mark_dirty();
    changed
}

pub(crate) fn unread_global_mention_count() -> usize {
    GLOBAL_UNREAD_MENTIONS.read().len()
}

/// Mark only mention rows the canvas has proved are inside a visible viewport.
pub(crate) fn mark_visible_global_mentions_read(visible: &HashSet<(String, String)>) -> bool {
    if visible.is_empty() {
        return false;
    }
    let mut unread = GLOBAL_UNREAD_MENTIONS.write();
    let before = unread.len();
    unread.retain(|key| !visible.contains(key));
    let changed = before != unread.len();
    if changed {
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}

/// Drop unread keys whose retained Activity row was filtered or evicted.
pub(crate) fn reconcile_global_mention_unread() {
    let retained: HashSet<_> = GLOBAL_MENTIONS
        .read()
        .iter()
        .map(|(channel, message)| global_mention_key(channel, message.id.as_ref()))
        .collect();
    GLOBAL_UNREAD_MENTIONS
        .write()
        .retain(|key| retained.contains(key));
    crate::backend::activity_persistence::mark_dirty();
}

pub(crate) fn activity_mentions_snapshot_visibility_commit_held() -> Vec<(String, ChatMessage, bool)>
{
    let mentions = GLOBAL_MENTIONS.read();
    let unread = GLOBAL_UNREAD_MENTIONS.read();
    mentions
        .iter()
        .skip(mentions.len().saturating_sub(MAX_ACTIVITY_MENTIONS))
        .map(|(channel, message)| {
            let is_unread = unread.contains(&global_mention_key(channel, message.id.as_ref()));
            (channel.clone(), message.clone(), is_unread)
        })
        .collect()
}

pub(crate) fn clear_activity_mentions_visibility_commit_held() {
    GLOBAL_MENTIONS.write().clear();
    GLOBAL_UNREAD_MENTIONS.write().clear();
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
}

pub(crate) fn restore_activity_mentions_visibility_commit_held(
    rows: Vec<(String, ChatMessage, bool)>,
) {
    let mut mentions = GLOBAL_MENTIONS.write();
    let mut unread_keys = GLOBAL_UNREAD_MENTIONS.write();
    let resident = mentions.iter().map(|(channel, message)| {
        let unread = unread_keys.contains(&global_mention_key(channel, message.id.as_ref()));
        (channel.clone(), message.clone(), unread)
    });
    let mut merged = HashMap::new();
    for (channel, message, unread) in rows.into_iter().chain(resident) {
        if message.id.is_empty()
            || crate::backend::blocked_users::is_blocked(
                message.user_id.as_deref(),
                message.sender.as_ref(),
            )
        {
            continue;
        }
        let key = global_mention_key(&channel, message.id.as_ref());
        merged.insert(key, (channel, message, unread));
    }
    let mut rows: Vec<_> = merged.into_values().collect();
    rows.sort_by(|a, b| {
        a.1.timestamp
            .cmp(&b.1.timestamp)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    let discard = rows.len().saturating_sub(MAX_ACTIVITY_MENTIONS);
    mentions.clear();
    unread_keys.clear();
    for (channel, mut message, unread) in rows.into_iter().skip(discard) {
        if message.reply.as_ref().is_some_and(|reply| {
            crate::backend::blocked_users::is_blocked(None, reply.parent_user_login.as_ref())
        }) {
            message.reply = None;
        }
        if unread {
            unread_keys.insert(global_mention_key(&channel, message.id.as_ref()));
        }
        mentions.push_back((channel, message));
    }
}

pub static CONNECTED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Tab-activity flag: a new message arrived in the channel.
pub const TAB_ACTIVITY_MESSAGE: u8 = 0b01;
/// Tab-activity flag: a mention/highlight arrived in the channel.
pub const TAB_ACTIVITY_MENTION: u8 = 0b10;

static CHANNEL_TAB_ACTIVITY: LazyLock<Mutex<FxHashMap<String, u8>>> =
    LazyLock::new(|| Mutex::new(FxHashMap::default()));

/// Record that a channel saw activity (message and/or mention) for tab coloring.
pub fn mark_channel_tab_activity(channel: &str, flags: u8) {
    if flags == 0 {
        return;
    }
    let mut map = CHANNEL_TAB_ACTIVITY.lock();
    *map.entry(channel.to_string()).or_insert(0) |= flags;
}

/// Take and clear all pending per-channel tab-activity flags.
pub fn drain_channel_tab_activity() -> Vec<(String, u8)> {
    let mut map = CHANNEL_TAB_ACTIVITY.lock();
    if map.is_empty() {
        return Vec::new();
    }
    std::mem::take(&mut *map).into_iter().collect()
}

/// Whether the channel is on the current IRC session (JOIN queued or
/// confirmed). History and cached rows load from here so a restored channel
/// paints before the server answers; sending additionally waits for the
/// server's echo of our own JOIN (`is_channel_join_confirmed`).
pub fn is_channel_connected(channel: &str) -> bool {
    let channel = channel.trim_start_matches('#').to_lowercase();
    CONNECTED_CHANNELS.read().contains(&channel)
}

/// The server has echoed our own JOIN for this channel on this session.
pub fn is_channel_join_confirmed(channel: &str) -> bool {
    let channel = channel.trim_start_matches('#').to_lowercase();
    CONNECTED_CHANNELS.read().contains(&channel) && join_scheduler::is_join_confirmed(&channel)
}

static BADGE_LOADED_CHANNELS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

static EMOTE_LOADED_CHANNELS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// When resolving a channel's id for its emotes last failed. A later message
/// retries after [`EMOTE_CHANNEL_ID_RETRY`]; without this a shared-chat source
/// whose lookup failed once stayed without emotes for the session.
static EMOTE_CHANNEL_ID_FAILURES: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const EMOTE_CHANNEL_ID_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// When each channel's history last finished loading. Used to skip the
/// connect-time streaming reload right after the startup prefetch while still
/// allowing reconnect gap-fill (which happens outside the skip window).
static HISTORY_LOADED_AT: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const HISTORY_RELOAD_SKIP_WINDOW_SECS: u64 = 30;

fn record_history_loaded_now(channel_lower: &str) {
    HISTORY_LOADED_AT
        .lock()
        .insert(channel_lower.to_string(), std::time::Instant::now());
}

/// True once a recent-messages pass has completed for the channel since it was
/// opened. A failed pass leaves this false and posts an error row instead.
pub fn is_history_loaded(channel: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    HISTORY_LOADED_AT.lock().contains_key(&channel)
}

fn history_recently_loaded(channel: &str) -> bool {
    let channel_lower = channel.to_lowercase();
    HISTORY_LOADED_AT
        .lock()
        .get(&channel_lower)
        .is_some_and(|loaded_at| {
            loaded_at.elapsed() < std::time::Duration::from_secs(HISTORY_RELOAD_SKIP_WINDOW_SECS)
        })
}

static HISTORY_LOADED_CHANNELS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// True while `load_channel_history_async` owns the channel's first history
/// pass: it claimed the channel but has not recorded a completed load yet.
/// A failed pass releases the claim, so a later join fetches again.
fn history_load_in_flight(channel: &str) -> bool {
    let channel_lower = channel.to_lowercase();
    HISTORY_LOADED_CHANNELS.lock().contains(&channel_lower)
        && !HISTORY_LOADED_AT.lock().contains_key(&channel_lower)
}

pub static CONNECTION_STATUS: LazyLock<RwLock<ConnectionStatus>> =
    LazyLock::new(|| RwLock::new(ConnectionStatus::Reconnecting));

static CONNECTION_EVENT_TX: OnceLock<tokio::sync::broadcast::Sender<ConnectionEvent>> =
    OnceLock::new();

/// Connection events for push-based status updates
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionEvent {
    /// Successfully connected to IRC
    Connected,
    /// Lost connection, attempting to reconnect
    Reconnecting,
    /// Failed to connect after extended period
    Disconnected,
}

impl From<ConnectionStatus> for ConnectionEvent {
    fn from(status: ConnectionStatus) -> Self {
        match status {
            ConnectionStatus::Connected => ConnectionEvent::Connected,
            ConnectionStatus::Reconnecting => ConnectionEvent::Reconnecting,
            ConnectionStatus::Disconnected => ConnectionEvent::Disconnected,
        }
    }
}

/// Initialize the connection event broadcast channel
fn init_connection_broadcast() {
    if CONNECTION_EVENT_TX.get().is_none() {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let _ = CONNECTION_EVENT_TX.set(tx);
    }
}

/// Broadcast a connection event to all subscribers
fn broadcast_connection_event(event: ConnectionEvent) {
    if let Some(tx) = CONNECTION_EVENT_TX.get() {
        let _ = tx.send(event);
    }
}

pub static IRC_SENDER: LazyLock<
    arc_swap::ArcSwap<Option<TwitchIRCClient<EchoIrcTransport, StaticLoginCredentials>>>,
> = LazyLock::new(|| arc_swap::ArcSwap::from_pointee(None));

/// Get the current IRC client, if connected.
/// Clones the client out of the ArcSwap guard so callers get an owned value.
pub fn get_irc_client() -> Option<TwitchIRCClient<EchoIrcTransport, StaticLoginCredentials>> {
    let guard = IRC_SENDER.load();
    guard.as_ref().clone()
}

static LAST_HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static LAST_RESTART_TIME: AtomicU64 = AtomicU64::new(0);
static RESTART_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static WATCHDOG_HEALTHY: AtomicBool = AtomicBool::new(true);
/// Count of messages received since last connect (reset on reconnect)
/// Used by watchdog to distinguish "quiet channel" from "dead connection"
static MESSAGES_SINCE_CONNECT: AtomicU64 = AtomicU64::new(0);
/// Set between system sleep and wake so the wall-clock watchdog does not
/// count the suspend as a dead socket.
static SYSTEM_SUSPENDED: AtomicBool = AtomicBool::new(false);
/// A wake waits for a replacement IRC connection. Traffic from the socket that
/// existed before the wake must not make the UI look connected again.
static WAKE_RECONNECT_PENDING: AtomicBool = AtomicBool::new(false);
static WAKE_RECONNECT_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Serializes the wake state with the only status transition that can claim a
/// reconnect succeeded. The hot Connected path does not take this lock.
static WAKE_STATUS_TRANSITION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
/// Connection generation a restart notification was aimed at. A stored
/// `Notify` permit for an older connection must not end a fresh one; a target
/// at or above the running generation always does, because back-to-back
/// requests share one permit and the later one raises the target.
static RESTART_TARGET_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Channels from the connect-time join list still waiting for their first
/// ROOMSTATE, which is the first proof the server joined them.
static SESSION_PENDING_JOIN_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));
/// Connect-time channels not yet announced as connected. Only ROOMSTATE
/// announces, so a NOTICE or the no-ROOMSTATE fallback that runs the history
/// pass first does not claim a join that never happened.
static SESSION_PENDING_ANNOUNCE_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));
/// Bounds concurrent post-reconnect gap-fills across channels.
static RECONNECT_GAP_FILL_PERMITS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(4));
/// A connection that has produced no server message by then is treated as a
/// failed connect, so the status stays Reconnecting and backoff grows.
const CONNECT_RESPONSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);
/// Connect-time channels still without ROOMSTATE this long after a responsive
/// connect get their history pass anyway (without a join announcement), so
/// suspended or banned channels that never send ROOMSTATE still load history.
const SESSION_JOIN_FALLBACK_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
/// A sleep notice with no matching wake stops pausing the watchdog after this
/// much awake time.
const WATCHDOG_MAX_SUSPEND_PAUSE: std::time::Duration = std::time::Duration::from_secs(300);

/// Set the connection status with escalation tracking and broadcast
pub fn set_connection_status(status: ConnectionStatus) {
    let current = *CONNECTION_STATUS.read();

    if status == ConnectionStatus::Reconnecting && current == ConnectionStatus::Connected {
        let now = crate::utils::unix_now_ms();
        CONNECTION_LOST_TIME.store(now, Ordering::SeqCst);
    }

    if status == ConnectionStatus::Connected {
        CONNECTION_LOST_TIME.store(0, Ordering::SeqCst);
    }

    if current != status {
        *CONNECTION_STATUS.write() = status;
        broadcast_connection_event(status.into());
    }
}

/// Get the current connection status
pub fn get_connection_status() -> ConnectionStatus {
    *CONNECTION_STATUS.read()
}

/// Record activity (heartbeat) from an IRC connection.
fn record_activity(connection_generation: u64) {
    let now = crate::utils::unix_now_ms();
    LAST_HEARTBEAT.store(now, Ordering::SeqCst);
    WATCHDOG_HEALTHY.store(true, Ordering::SeqCst);

    if get_connection_status() == ConnectionStatus::Connected {
        return;
    }

    let _wake_status_transition = WAKE_STATUS_TRANSITION_LOCK.lock();
    if WAKE_RECONNECT_PENDING.load(Ordering::Acquire)
        && connection_generation <= WAKE_RECONNECT_GENERATION.load(Ordering::Acquire)
    {
        crate::log_irc!(
            debug,
            connection_generation,
            "Ignoring pre-wake IRC traffic while replacement connection is pending"
        );
        return;
    }

    if get_connection_status() != ConnectionStatus::Connected {
        crate::log_irc!(info, "Connection restored, resetting status to Connected");
        WAKE_RECONNECT_PENDING.store(false, Ordering::Release);
        set_connection_status(ConnectionStatus::Connected);
        CONNECTION_LOST_TIME.store(0, Ordering::SeqCst);
        RESTART_ATTEMPTS.store(0, Ordering::SeqCst);
    }
}

/// Track a chatter for username completion
/// Moves user to front of list if already present, otherwise adds them
pub fn track_chatter(channel: &str, username: &str, display_name: &str, color: Option<String>) {
    let username_lower = username.to_lowercase();
    let mut chatters = RECENT_CHATTERS
        .entry(channel.to_lowercase())
        .or_insert_with(LruCache::unbounded);

    if let Some(chatter) = chatters.get_mut(username_lower.as_str()) {
        if chatter.display_name != display_name {
            chatter.display_name = display_name.to_string();
        }
        chatter.color = color;
        chatter.message_count = chatter.message_count.saturating_add(1);
        return;
    }
    chatters.put(
        username_lower,
        RecentChatter {
            display_name: display_name.to_string(),
            color,
            message_count: 1,
        },
    );
    while chatters.len() > RECENT_CHATTERS_MAX {
        chatters.pop_lru();
    }
}

/// Get recent chatters for a channel
/// Returns list of (username, display_name, color) tuples
pub fn get_recent_chatters(channel: &str, limit: usize) -> Vec<(String, String, Option<String>)> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel_lower = channel.to_lowercase();

    RECENT_CHATTERS
        .get(&channel_lower)
        .map(|chatters| {
            chatters
                .iter()
                .take(limit)
                .map(|(login, chatter)| {
                    (
                        login.clone(),
                        chatter.display_name.clone(),
                        chatter.color.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Get prefix-matching completion chatters ranked by observed message
/// frequency, then LRU recency, then normalized login for a deterministic
/// final fallback.
///
/// The bounded visual popup requests only its visible row count. Inline Tab
/// may request the full matching set, but only after its two-character gate.
/// Both paths filter while the bounded resident index is borrowed, clone only
/// matches, and avoid copying the complete count map on every keystroke.
pub fn get_ranked_chatters(
    channel: &str,
    prefix: &str,
    limit: usize,
) -> Vec<(String, String, Option<String>)> {
    if limit == 0 || crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel_lower = channel.to_lowercase();
    let prefix = prefix.trim_start_matches('@').to_lowercase();
    let Some(chatters) = RECENT_CHATTERS.get(&channel_lower) else {
        return Vec::new();
    };
    let bounded = limit != usize::MAX;
    let mut matches = Vec::with_capacity(if bounded {
        limit.min(chatters.len())
    } else {
        0
    });
    for (recency_rank, (login, chatter)) in chatters.iter().enumerate() {
        let display_name = &chatter.display_name;
        let color = &chatter.color;
        if !prefix.is_empty()
            && !login.starts_with(&prefix)
            && !chatter_prefix_matches(display_name, &prefix)
        {
            continue;
        }

        let message_count = chatter.message_count;
        let insertion_index = if !bounded || matches.len() < limit {
            Some(matches.len())
        } else {
            let worst_index = matches
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| compare_ranked_chatters(left, right))
                .map(|(index, _)| index);
            worst_index.filter(|index| {
                compare_ranked_chatter_fields(message_count, recency_rank, login, &matches[*index])
                    .is_lt()
            })
        };
        let Some(insertion_index) = insertion_index else {
            continue;
        };
        let candidate = RankedChatterCandidate {
            recency_rank,
            message_count,
            login: login.clone(),
            display_name: display_name.clone(),
            color: color.clone(),
        };
        if insertion_index == matches.len() {
            matches.push(candidate);
        } else {
            matches[insertion_index] = candidate;
        }
    }
    matches.sort_by(compare_ranked_chatters);
    matches
        .into_iter()
        .map(|candidate| (candidate.login, candidate.display_name, candidate.color))
        .collect()
}

pub(crate) fn clear_recent_chatters_for_benchmark(channel: &str) -> bool {
    if !crate::benchmark_ui_fixture::is_enabled() {
        return false;
    }
    let channel = channel.to_lowercase();
    RECENT_CHATTERS.remove(&channel).is_some()
}

/// Cache self user's color and badges for a channel (from USERSTATE)
pub fn cache_self_state(channel: &str, color: Option<String>, badges: Vec<String>) -> bool {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let previous_blocked_visibility_role = blocked_visibility_role_state(channel);
    let next = (color, badges);
    let previous = SELF_CHANNEL_STATE
        .lock()
        .insert(channel.to_lowercase(), next.clone());
    let changed = previous.as_ref() != Some(&next);
    let next_blocked_visibility_role = blocked_visibility_role_state(channel);
    let blocked_rows_changed = previous_blocked_visibility_role != next_blocked_visibility_role
        && apply_blocked_user_filter_to_channel(channel);
    drop(visibility_commit);
    if blocked_rows_changed && !crate::backend::blocked_users::initial_visibility_pending() {
        dispatch_blocked_channel_refreshes(HashSet::from([channel.to_lowercase()]));
    }
    changed
}

/// Clear role state at an IRC-session boundary under the same gate used by
/// message producers. This prevents a producer from observing a half-published
/// moderator/broadcaster transition while committing irreversible effects.
fn clear_self_channel_states() {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    SELF_CHANNEL_STATE.lock().clear();
    drop(visibility_commit);
    crate::backend::irc::room_state::reset_send_exemptions();
}

/// Get self user's cached state for a channel (color, badges)
pub fn get_self_state(channel: &str) -> Option<(Option<String>, Vec<String>)> {
    SELF_CHANNEL_STATE
        .lock()
        .get(&channel.to_lowercase())
        .cloned()
}

/// Whether the active viewer may moderate this channel. Broadcaster ownership
/// is known from the authenticated login; moderator authority fails closed
/// until Twitch supplies the channel's USERSTATE badges.
pub fn can_moderate_channel(channel: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#');
    if channel.is_empty() {
        return false;
    }
    if crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(channel) {
        return true;
    }
    get_self_state(channel).is_some_and(|(_, badges)| {
        badges.iter().any(|badge| {
            crate::backend::badges::is_moderator_badge_key(badge)
                || crate::backend::badges::badge_key_matches(badge, "broadcaster")
        })
    })
}

/// Apply the channel-role exception only after Twitch has supplied USERSTATE.
/// A known blocked sender fails closed while moderator state is unknown: an
/// early row can otherwise escape into logs, alerts, and media requests before
/// USERSTATE arrives. Broadcaster ownership is known from the active login.
pub(crate) struct BlockedChannelVisibilitySnapshot {
    policy: Option<crate::backend::blocked_users::BlockedVisibilityPolicy>,
    hide_blocked_senders: bool,
}

impl BlockedChannelVisibilitySnapshot {
    #[inline]
    pub(crate) fn should_hide_sender(&self, user_id: Option<&str>, user_login: &str) -> bool {
        self.hide_blocked_senders
            && self
                .policy
                .as_ref()
                .is_some_and(|policy| policy.contains(user_id, user_login))
    }

    #[inline]
    pub(crate) fn should_redact_reply_parent(&self, parent_login: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.contains(None, parent_login))
    }
}

pub(crate) fn blocked_channel_visibility_snapshot(
    channel: &str,
) -> BlockedChannelVisibilitySnapshot {
    let policy = crate::backend::blocked_users::visibility_policy_snapshot();
    let hide_blocked_senders = policy.as_ref().is_some_and(|policy| {
        !blocked_visibility_role_state_for(channel, policy.visibility).unwrap_or(false)
    });
    BlockedChannelVisibilitySnapshot {
        policy,
        hide_blocked_senders,
    }
}

pub(crate) fn should_hide_blocked_channel_message(
    user_id: Option<&str>,
    user_login: &str,
    channel: &str,
) -> bool {
    blocked_channel_visibility_snapshot(channel).should_hide_sender(user_id, user_login)
}

/// `None` means the role-dependent policy is waiting for the first USERSTATE.
/// Broadcasters are known from the authenticated login before badges arrive.
fn blocked_visibility_role_state_for(
    channel: &str,
    visibility: crate::backend::settings::BlockedUserVisibility,
) -> Option<bool> {
    use crate::backend::settings::BlockedUserVisibility;
    let channel = channel.trim().trim_start_matches('#');
    let is_channel_owner = !channel.is_empty()
        && crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(channel);
    match visibility {
        BlockedUserVisibility::Never => Some(false),
        BlockedUserVisibility::IfModerator => {
            if is_channel_owner {
                return Some(true);
            }
            get_self_state(channel).map(|(_, badges)| {
                badges.iter().any(|badge| {
                    crate::backend::badges::is_moderator_badge_key(badge)
                        || crate::backend::badges::badge_key_matches(badge, "broadcaster")
                })
            })
        }
        BlockedUserVisibility::IfBroadcaster => Some(is_channel_owner),
    }
}

fn blocked_visibility_role_state(channel: &str) -> Option<bool> {
    blocked_visibility_role_state_for(
        channel,
        crate::backend::settings::get_show_blocked_users_messages(),
    )
}

/// If `content` from `sender` matches a pending optimistic message we sent,
/// remove and return the id of the optimistic row created for it so the caller
/// can upgrade that row in place with the real Twitch message. Returns `None`
/// when this is not an echo of one of our own optimistic messages.
fn take_optimistic_echo(channel: &str, sender: &str, content: &str) -> Option<Arc<str>> {
    let username = get_username_lowercase();
    if username.is_empty() || sender.to_lowercase() != username {
        return None;
    }

    let channel_lower = channel.to_lowercase();
    let now = Utc::now().timestamp_millis();

    let mut pending = PENDING_OPTIMISTIC.lock();
    let queue = pending.get_mut(&channel_lower)?;
    while let Some(front) = queue.front() {
        if now - front.timestamp_ms > OPTIMISTIC_MESSAGE_MAX_AGE_MS {
            queue.pop_front();
        } else {
            break;
        }
    }

    let pos = queue
        .iter()
        .position(|pending| pending.content == content)?;
    let entry = queue.remove(pos)?;
    crate::log_irc!(debug, "Matched optimistic echo for message in #{}", channel);
    Some(entry.optimistic_id)
}

/// Reconcile a Twitch echo of one of our own optimistic messages: upgrade the
/// existing optimistic row in place with the real Twitch message id and fields
/// rather than discarding the echo. Because the fake `optimistic-…` id was never
/// replaced with the real id, every later id-based dedup path (gap-fill, history
/// merges, CLEARMSG-by-id, reply parents, scroll markers) failed to recognize
/// the row and re-added a fetched copy as a twin. Recording the real id on the
/// row (it is already in `MESSAGE_DEDUP` from the PRIVMSG handler's dedup step)
/// closes that gap.
///
/// `content` is the reply-mention-stripped echo text — identical to the
/// optimistic row's content for our own sends — and `reply_mention_strip_len`
/// is the char offset to apply to the raw emotes-tag positions (0 for our own
/// non-reply sends). Returns true when a row was upgraded.
fn reconcile_live_optimistic_echo(
    channel_lower: &str,
    optimistic_id: &str,
    msg: &twitch_irc::message::PrivmsgMessage,
    content: &str,
    reply_mention_strip_len: usize,
) -> bool {
    let real_id: Arc<str> = Arc::from(msg.message_id.as_str());
    let user_id: Option<Arc<str>> = Some(Arc::from(msg.sender.id.as_str()));
    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    let color: Option<Arc<str>> = msg.name_color.map(|c| format_color_rgb(c.r, c.g, c.b));
    let badges: smallvec::SmallVec<[Arc<str>; 4]> = msg
        .badges
        .iter()
        .map(|b| format_badge(&b.name, &b.version))
        .collect();
    let twitch_emotes_tag: Option<Arc<str>> = msg
        .source
        .tags
        .0
        .get("emotes")
        .map(|t| Arc::from(t.as_str()));

    let twitch_emotes = twitch_emotes_tag
        .as_ref()
        .map(|tag| parse_emotes_tag_with_offset(tag, reply_mention_strip_len))
        .unwrap_or_default();
    let twitch_gifs_tag = msg
        .source
        .tags
        .0
        .get("gifs")
        .map(|tag| Arc::<str>::from(tag.as_str()));
    let new_slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        content,
        channel_lower,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
        twitch_gifs_tag.as_deref(),
        reply_mention_strip_len,
    );
    let new_structure_hash = compute_structure_hash(&new_slots);
    let new_slots: Arc<[MessageSlot]> = new_slots.into();
    let new_content: Arc<str> = Arc::from(content);

    let upgraded = match CHAT_MESSAGES.get_mut(channel_lower) {
        Some(mut buffer) => buffer.update_by_id(optimistic_id, |row| {
            row.id = real_id.clone();
            row.user_id = user_id;
            row.timestamp = timestamp;
            if color.is_some() {
                row.color = color;
            }
            if !badges.is_empty() {
                row.badges = badges;
            }
            row.content = new_content;
            row.slots = new_slots;
            row.structure_hash = new_structure_hash;
            row.twitch_emotes_tag = twitch_emotes_tag;
            row.twitch_gifs_tag = twitch_gifs_tag;
            row.reply_emote_offset = reply_mention_strip_len;
        }),
        None => false,
    };
    if !upgraded {
        return false;
    }

    crate::state::layout_eviction::queue_evicted_id(Arc::from(optimistic_id));
    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        channel_lower,
        crate::backend::observers::ChannelEvent::MessagesRetokenized,
    );
    crate::log_irc!(
        debug,
        "Reconciled optimistic row {} with real id {} in #{}",
        optimistic_id,
        real_id,
        channel_lower
    );
    true
}

fn reply_thread_has_current_user_participation(
    channel: &str,
    parent_msg_id: &str,
    username: &str,
) -> bool {
    if username.is_empty() || parent_msg_id.trim().is_empty() {
        return false;
    }

    let Some(buffer) = CHAT_MESSAGES.get(channel) else {
        return false;
    };

    buffer
        .iter_recent(PARTICIPATED_REPLY_SCAN_LIMIT)
        .any(|message| {
            if !message.sender.as_ref().eq_ignore_ascii_case(username) {
                return false;
            }

            message.id.as_ref() == parent_msg_id
                || message
                    .reply
                    .as_ref()
                    .map(|reply| reply.parent_msg_id.as_ref() == parent_msg_id)
                    .unwrap_or(false)
        })
}

/// Mark a message ID as seen for a channel
/// Returns true if the message was new, false if it was a duplicate
pub fn mark_message_seen(channel: &str, msg_id: &str) -> bool {
    if MESSAGE_DEDUP.len() > DEDUP_MAX_CHANNELS {
        let connected = CONNECTED_CHANNELS.read();
        MESSAGE_DEDUP.retain(|ch, _| connected.contains(ch));
    }

    let channel_lower = channel.to_lowercase();
    let mut entry = MESSAGE_DEDUP
        .entry(channel_lower)
        .or_insert_with(|| MessageDeduplicator::new(DEDUP_MAX_SIZE));
    entry.insert(msg_id.to_string())
}

fn admit_received_chat_message(channel: &str, msg_id: &str) -> bool {
    let mut receipts = CONFIRMED_LOCAL_ECHOES.lock();
    receipts
        .pop(&(channel.to_lowercase(), msg_id.to_string()))
        .is_some()
        || mark_message_seen(channel, msg_id)
}

fn insert_confirmed_local_row(buffer: &mut MessageBuffer, message: &mut ChatMessage) -> bool {
    if buffer.contains_id(message.id.as_ref()) {
        return false;
    }
    buffer.push_stamped(message);
    true
}

fn store_received_chat_row(buffer: &mut MessageBuffer, message: &mut ChatMessage) -> bool {
    let replaced = buffer.update_by_id(message.id.as_ref(), |row| {
        let alternate = row.flags.is_alternate_row;
        *row = message.clone();
        row.flags.is_alternate_row = alternate;
    });
    if !replaced {
        buffer.push_stamped(message);
    }
    replaced
}

/// Trigger a connection status update (watchdog detected issue)
/// Note: With the supervisor loop pattern, actual reconnection is handled by the
/// supervised connection loops, not by the watchdog directly.
pub async fn trigger_restart() -> Result<(), String> {
    let now = crate::utils::unix_now_ms();

    let last_restart = LAST_RESTART_TIME.load(Ordering::SeqCst);

    if now - last_restart < RESTART_DEBOUNCE_MS {
        crate::log_irc!(debug, "Status update debounced, too soon since last");
        return Err("Status update debounced".to_string());
    }

    let attempts = RESTART_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
    crate::log_irc!(
        debug,
        "Detected connection issue (attempt {}), supervisor will handle reconnection",
        attempts
    );

    LAST_RESTART_TIME.store(now, Ordering::SeqCst);

    set_connection_status(ConnectionStatus::Reconnecting);

    RESTART_TARGET_GENERATION.store(
        CONNECTION_GENERATION.load(Ordering::Acquire),
        Ordering::Release,
    );
    CONNECTION_RESTART_NOTIFY.notify_one();

    Ok(())
}

/// Check watchdog status and trigger status updates if needed
/// This should be called periodically (e.g., every second)
/// Note: With supervisor loops, this just monitors health - reconnection is automatic
pub fn check_watchdog() {
    if SYSTEM_SUSPENDED.load(Ordering::Acquire) {
        return;
    }
    let now = crate::utils::unix_now_ms();

    let last_heartbeat = LAST_HEARTBEAT.load(Ordering::SeqCst);
    let elapsed = now.saturating_sub(last_heartbeat);

    let status = get_connection_status();
    if status == ConnectionStatus::Reconnecting {
        let lost_time = CONNECTION_LOST_TIME.load(Ordering::SeqCst);
        if lost_time > 0 {
            let reconnect_elapsed = now.saturating_sub(lost_time);
            if reconnect_elapsed > CONNECTION_ESCALATION_MS {
                crate::log_irc!(
                    warn,
                    "Connection escalated to Disconnected after {}s (supervisor still retrying)",
                    reconnect_elapsed / 1000
                );
                set_connection_status(ConnectionStatus::Disconnected);
                for channel in get_desired_channels() {
                    add_connection_status_message(
                        &channel,
                        "Disconnected from chat. Still retrying in the background.",
                        "disconnected",
                    );
                }
            }
        }
    }

    if elapsed > WATCHDOG_TIMEOUT_MS
        && last_heartbeat > 0
        && status == ConnectionStatus::Connected
        && MESSAGES_SINCE_CONNECT.load(Ordering::Relaxed) > 0
    {
        // One warning per stall: later ticks of the same stall (a debounced
        // restart leaves the status Connected) log at debug.
        if WATCHDOG_HEALTHY.swap(false, Ordering::SeqCst) {
            crate::log_irc!(
                warn,
                "IRC watchdog: no traffic for {}ms (threshold {}ms); restarting the connection",
                elapsed,
                WATCHDOG_TIMEOUT_MS
            );
        } else {
            crate::log_irc!(debug, "IRC watchdog: still no traffic after {}ms", elapsed);
        }

        tokio::spawn(async {
            if let Err(e) = trigger_restart().await {
                crate::log_irc!(debug, "IRC watchdog restart was not triggered: {}", e);
            }
        });
    }
}

/// Start the watchdog timer (should be called once on app startup)
pub fn start_watchdog() {
    LAST_HEARTBEAT.store(crate::utils::unix_now_ms(), Ordering::SeqCst);

    tokio::spawn(async {
        let mut suspended_for = std::time::Duration::ZERO;
        loop {
            let render_mode = crate::managers::frame_rate_controller::current_mode();
            let sleep_duration = match render_mode {
                crate::managers::frame_rate_controller::RenderMode::TrueIdle => {
                    tokio::time::Duration::from_secs(10)
                }
                crate::managers::frame_rate_controller::RenderMode::DeepIdle => {
                    tokio::time::Duration::from_secs(5)
                }
                _ => tokio::time::Duration::from_secs(1),
            };
            tokio::time::sleep(sleep_duration).await;
            if SYSTEM_SUSPENDED.load(Ordering::Acquire) {
                suspended_for += sleep_duration;
                if suspended_for >= WATCHDOG_MAX_SUSPEND_PAUSE {
                    crate::log_irc!(
                        warn,
                        "No wake notification after system sleep; resuming the IRC watchdog"
                    );
                    SYSTEM_SUSPENDED.store(false, Ordering::Release);
                    suspended_for = std::time::Duration::ZERO;
                }
            } else {
                suspended_for = std::time::Duration::ZERO;
            }
            check_watchdog();
        }
    });
}
/// Calculate the next backoff delay with exponential growth and jitter
fn calculate_next_backoff(current_delay: u64) -> u64 {
    let next = (current_delay as f64 * RECONNECT_BACKOFF_FACTOR) as u64;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let jitter = now % (RECONNECT_JITTER_MS * 2);
    let delay_with_jitter = next
        .saturating_add(jitter)
        .saturating_sub(RECONNECT_JITTER_MS);

    std::cmp::min(delay_with_jitter, RECONNECT_MAX_DELAY_MS)
}

/// Reset backoff delay after successful connection
fn reset_read_backoff() {
    READ_RECONNECT_DELAY.store(RECONNECT_MIN_DELAY_MS, Ordering::SeqCst);
    crate::log_irc!(debug, "Read backoff reset to {}ms", RECONNECT_MIN_DELAY_MS);
}

/// Get and increment read backoff delay
fn get_and_increment_read_backoff() -> u64 {
    let current = READ_RECONNECT_DELAY.load(Ordering::SeqCst);
    let next = calculate_next_backoff(current);
    READ_RECONNECT_DELAY.store(next, Ordering::SeqCst);
    current
}

/// Handle Twitch rejecting IRC authentication for the active account: refresh
/// the token when a refresh token exists, otherwise fall back to anonymous mode
/// and warn. Always returns the reconnect reason as `Err`.
async fn recover_from_irc_auth_failure(
    credential_fence: Option<&crate::backend::auth::IrcCredentialFence>,
) -> Result<(), String> {
    crate::log_irc!(
        error,
        "Twitch rejected IRC authentication for the active account"
    );
    let Some(credential_fence) = credential_fence.filter(|fence| fence.is_current()) else {
        crate::log_irc!(
            info,
            "Ignoring authentication rejection from a superseded IRC credential"
        );
        return Err("Stale IRC credential was rejected; reconnecting".to_string());
    };

    if crate::backend::settings::get_refresh_token().is_some_and(|token| !token.trim().is_empty()) {
        crate::log_auth!(info, "Refreshing token after IRC authentication failure");
        match crate::backend::auth::refresh_and_save_tokens_for_irc(credential_fence).await {
            Ok(_) => {
                USE_ANONYMOUS_MODE.store(false, Ordering::Release);
                return Err(
                    "IRC authentication failed; refreshed token and reconnecting".to_string(),
                );
            }
            Err(error) => {
                crate::log_auth!(
                    warn,
                    "Token refresh after IRC authentication failure failed: {}",
                    error
                );
            }
        }
    }

    if !credential_fence.is_current() {
        return Err("IRC authentication failure was superseded by an account change".to_string());
    }

    if !crate::backend::auth::mark_runtime_auth_failed_if_current(
        credential_fence,
        "Twitch IRC rejected the active account token. Re-authenticate to send chat messages.",
    ) {
        return Err("IRC authentication failure was superseded by an account change".to_string());
    }
    USE_ANONYMOUS_MODE.store(true, Ordering::Release);
    crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
        message: "Twitch chat login failed".to_string(),
        detail: Some(
            "Twitch rejected the active account token. Re-authenticate before sending messages."
                .to_string(),
        ),
    });

    Err("Auth failed - Twitch IRC rejected the active account token".to_string())
}

/// Channels we want to be connected to (survives reconnection)
static DESIRED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Channels the user closed and has not reopened. Late async producers check
/// this before committing rows, subscriptions or preloads for a channel.
static CLOSED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// Clear the closed mark when a channel is opened again.
pub fn note_channel_opened(channel: &str) {
    crate::backend::twitch_api::clear_channel_lookup_state(channel);
    let channel = channel.trim_start_matches('#').to_lowercase();
    if CLOSED_CHANNELS.read().contains(&channel) {
        CLOSED_CHANNELS.write().remove(&channel);
    }
}

/// True when the channel was closed and has not been reopened since.
pub fn is_channel_closed(channel: &str) -> bool {
    let closed = CLOSED_CHANNELS.read();
    if closed.is_empty() {
        return false;
    }
    let channel = channel.trim_start_matches('#');
    if channel.bytes().any(|byte| byte.is_ascii_uppercase()) {
        closed.contains(&channel.to_ascii_lowercase())
    } else {
        closed.contains(channel)
    }
}

/// Notification for immediate channel join (wakes up sync loop)
static CHANNEL_JOIN_NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static CONNECTION_RESTART_NOTIFY: LazyLock<tokio::sync::Notify> =
    LazyLock::new(tokio::sync::Notify::new);
static CONNECTION_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Rebuild the IRC connection so new sends use the current account credentials.
pub fn reconnect_with_current_auth() {
    restart_connection("account credentials or network settings changed");
}

/// Stamp every joined channel's gap start before the system suspends, so the
/// post-wake gap-fill is sized for the whole sleep rather than from the moment
/// the dead socket is noticed.
pub fn record_system_sleep() {
    SYSTEM_SUSPENDED.store(true, Ordering::Release);
    record_all_channel_disconnects();
}

/// Resume the watchdog after wake and hold the UI in a truthful reconnecting
/// state until traffic arrives from the replacement IRC connection.
pub fn note_system_wake() {
    LAST_HEARTBEAT.store(crate::utils::unix_now_ms(), Ordering::SeqCst);
    SYSTEM_SUSPENDED.store(false, Ordering::Release);
    {
        let _wake_status_transition = WAKE_STATUS_TRANSITION_LOCK.lock();
        WAKE_RECONNECT_GENERATION.store(
            CONNECTION_GENERATION.load(Ordering::Acquire),
            Ordering::Release,
        );
        WAKE_RECONNECT_PENDING.store(true, Ordering::Release);
        set_connection_status(ConnectionStatus::Reconnecting);
    }
    for channel in get_desired_channels() {
        add_connection_status_message(
            &channel,
            "Waiting for network after system wake.",
            "reconnecting",
        );
    }
}

/// Replace the IRC connection after the system resumes from sleep.
pub fn reconnect_after_system_wake() {
    for channel in get_desired_channels() {
        add_connection_status_message(
            &channel,
            "Reconnecting to chat after system wake.",
            "reconnecting",
        );
    }
    restart_connection("system wake");
}

fn restart_connection(reason: &'static str) {
    clear_self_channel_states();

    if !SUPERVISOR_RUNNING.load(Ordering::Acquire) {
        return;
    }

    crate::log_irc!(info, reason, "Reconnecting IRC");
    IRC_SENDER.store(Arc::new(None));
    let running_generation = CONNECTION_GENERATION.fetch_add(1, Ordering::AcqRel);
    RESTART_TARGET_GENERATION.store(running_generation, Ordering::Release);
    CONNECTION_RESTART_NOTIFY.notify_one();
    CHANNEL_JOIN_NOTIFY.notify_one();
}

fn restart_request_applies(running: u64, target: u64, current: u64) -> bool {
    target >= running || current != running
}

/// Add a channel to the desired channels list
pub fn add_desired_channel(channel: &str) {
    note_channel_opened(channel);
    {
        let mut desired = DESIRED_CHANNELS.write();
        let channel = channel.to_lowercase();
        join_scheduler::open_channel_intent(&channel);
        desired.insert(channel);
    }

    CHANNEL_JOIN_NOTIFY.notify_one();
}

/// Remove a channel from the desired channels list.
pub fn remove_desired_channel(channel: &str) -> bool {
    let removed = {
        let mut desired = DESIRED_CHANNELS.write();
        let channel = channel.to_lowercase();
        join_scheduler::close_channel_intent(&channel);
        desired.remove(&channel)
    };

    CHANNEL_JOIN_NOTIFY.notify_one();
    removed
}

/// Leave a channel immediately when the last visible Echo split stops using it.
pub fn leave_channel_immediately(channel: &str) -> bool {
    let channel_norm = channel.to_lowercase();
    CLOSED_CHANNELS.write().insert(channel_norm.clone());
    let removed_desired = remove_desired_channel(&channel_norm);
    let was_connected = CONNECTED_CHANNELS.write().remove(&channel_norm);
    clear_channel_runtime_state(&channel_norm);

    let Some(client) = get_irc_client() else {
        crate::log_irc!(
            debug,
            "leave_channel_immediately: IRC not connected for {}",
            channel_norm
        );
        return removed_desired || was_connected;
    };

    if was_connected {
        client.part(channel_norm.clone());
        crate::log_irc!(info, "Parted channel: {}", channel_norm);
    }

    removed_desired || was_connected
}

/// Remove channel-local runtime state after the channel is no longer active.
fn clear_channel_runtime_state(channel: &str) {
    let channel_norm = channel.trim_start_matches('#').to_lowercase();
    if channel_norm.is_empty() {
        return;
    }

    CONNECTED_CHANNELS.write().remove(&channel_norm);
    SELF_JOINED_CHANNELS.write().remove(&channel_norm);
    SESSION_PENDING_JOIN_CHANNELS.write().remove(&channel_norm);
    SESSION_PENDING_ANNOUNCE_CHANNELS
        .write()
        .remove(&channel_norm);
    HISTORY_LOADED_CHANNELS.lock().remove(&channel_norm);
    HISTORY_LOADED_AT.lock().remove(&channel_norm);
    BADGE_LOADED_CHANNELS.lock().remove(&channel_norm);
    EMOTE_LOADED_CHANNELS.lock().remove(&channel_norm);
    CHANNEL_DISCONNECT_STATES.write().remove(&channel_norm);
    crate::backend::twitch_api::clear_channel_lookup_state(&channel_norm);
    crate::backend::channel_lifecycle::get_lifecycle_manager().remove_channel(&channel_norm);
    crate::backend::irc::room_state::clear_room_state(&channel_norm);

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    CHAT_MESSAGES.remove(&channel_norm);
    crate::backend::blocked_users::clear_chat_state_reconcile_channel(&channel_norm);
    MESSAGE_DEDUP.remove(&channel_norm);
    RECENT_CHATTERS.remove(&channel_norm);
    {
        let mut self_state = SELF_CHANNEL_STATE.lock();
        self_state.remove(&channel_norm);
    }

    {
        let mut pending = PENDING_OPTIMISTIC.lock();
        pending.remove(&channel_norm);
    }
    PENDING_MESSAGE_EMOTE_SCOPES
        .lock()
        .retain(|(channel, _), _| channel != &channel_norm);

    {
        let mut mentions = GLOBAL_MENTIONS.write();
        mentions.retain(|(mention_channel, _)| mention_channel != &channel_norm);
    }
    GLOBAL_UNREAD_MENTIONS
        .write()
        .retain(|(mention_channel, _)| mention_channel != &channel_norm);
    crate::backend::activity_persistence::mark_dirty();
    drop(visibility_commit);
    bump_chat_version();
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );

    crate::log_irc!(debug, "Cleared runtime state for channel {}", channel_norm);
}

/// Join a channel immediately via IRC_SENDER (bypasses 5-second poll)
/// Returns true if join was initiated, false if IRC not connected
/// NOTE: Caller is responsible for spawning history loading via spawn_history_load()
pub fn join_channel_immediately(channel: &str) -> bool {
    add_desired_channel(channel);

    let Some(client) = get_irc_client() else {
        crate::log_irc!(debug, "join_channel_immediately: IRC not connected yet");
        return false;
    };

    let channel_norm = channel.to_lowercase();

    if CONNECTED_CHANNELS.read().contains(&channel_norm) {
        crate::log_irc!(debug, "Channel {} already connected", channel);
        return true;
    }

    CONNECTED_CHANNELS.write().insert(channel_norm.clone());

    if let Err(e) = client.join(channel.to_string()) {
        crate::log_irc!(error, "Immediate join failed for {}: {}", channel, e);
        CONNECTED_CHANNELS.write().remove(&channel_norm);
        return false;
    }

    crate::log_irc!(info, "Queued channel JOIN: {}", channel);
    true
}

/// Spawn history loading for a channel (call after join_channel_immediately succeeds)
/// This is an async function that should be spawned on the runtime
pub async fn spawn_history_load(channel: String) {
    if crate::benchmark_ui_fixture::is_fixture_channel(&channel) {
        crate::log_irc!(
            debug,
            "Skipping history hydration for hermetic UI fixture channel {}",
            channel
        );
        return;
    }
    load_channel_history_async(&channel).await;
}

/// Recheck the current blocked membership at the final history commit, after
/// network/cache loading and message parsing have completed.
fn track_visible_history_chatter(channel: &str, message: &ChatMessage) {
    if message.flags.is_system
        || message.flags.is_usernotice
        || message.sender.trim().is_empty()
        || message.sender.eq_ignore_ascii_case("system")
    {
        return;
    }
    let display_name = if message.display_name.trim().is_empty() {
        message.sender.as_ref()
    } else {
        message.display_name.as_ref()
    };
    track_chatter(
        channel,
        message.sender.as_ref(),
        display_name,
        message.color.as_deref().map(str::to_owned),
    );
}

fn seed_chatters_from_visible_buffer(channel: &str) {
    let rows: Vec<_> = CHAT_MESSAGES
        .get(channel)
        .map(|buffer| {
            buffer
                .iter()
                .filter(|message| {
                    !message.flags.is_system
                        && !message.flags.is_usernotice
                        && !message.sender.trim().is_empty()
                        && !message.sender.eq_ignore_ascii_case("system")
                })
                .map(|message| {
                    (
                        message.sender.to_string(),
                        message.display_name.to_string(),
                        message.color.as_deref().map(str::to_owned),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (sender, display_name, color) in rows {
        let display_name = if display_name.trim().is_empty() {
            sender.as_str()
        } else {
            display_name.as_str()
        };
        track_chatter(channel, &sender, display_name, color);
    }
}

/// Whether a fetched history message was authored by the local user. The
/// recent-messages API fills `sender`/`display_name` from the display-name tag
/// (mixed case), so match case-insensitively against both the local login and
/// the local display name.
fn history_message_is_local_author(msg: &ChatMessage, own_login: &str, own_display: &str) -> bool {
    let sender = msg.sender.as_ref();
    let display = msg.display_name.as_ref();
    sender.eq_ignore_ascii_case(own_login)
        || sender.eq_ignore_ascii_case(own_display)
        || display.eq_ignore_ascii_case(own_login)
        || display.eq_ignore_ascii_case(own_display)
}

/// Pair fetched history/gap-fill copies of the local user's own messages with
/// still-pending optimistic rows and upgrade those rows in place, returning the
/// incoming list with the paired copies removed.
///
/// This is the silent-outage half of the optimistic-echo fix. When the read
/// socket dies before Twitch echoes our message back, the optimistic row keeps
/// its fake `optimistic-…` id (the live echo never ran), and the gap-fill fetch
/// of that same message carries the real Twitch id. That real id matches no
/// existing buffer id and — once the 10s optimistic window and the bounded
/// `MESSAGE_DEDUP` set have aged out — passes the id dedup too, so it lands as a
/// twin. Re-identifying the pair by (local author + identical trimmed content +
/// a send-latency-sized timestamp window) lets us upgrade the row instead.
fn reconcile_optimistic_history_rows(
    channel: &str,
    mut messages: Vec<ChatMessage>,
) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return messages;
    }

    let own_login = get_username_lowercase();
    if own_login.is_empty() {
        return messages;
    }
    let own_display = crate::backend::settings::get_active_account()
        .and_then(|account| account.display_name)
        .map(|name| name.to_lowercase())
        .unwrap_or_else(|| own_login.clone());

    let channel_lower = channel.to_lowercase();

    let mut candidates: Vec<(Arc<str>, i64, String)> = Vec::new();
    let mut existing_real_ids: HashSet<Arc<str>> = HashSet::new();
    match CHAT_MESSAGES.get(&channel_lower) {
        Some(buffer) => {
            for message in buffer.iter() {
                if message.id.as_ref().starts_with("optimistic-") {
                    candidates.push((
                        message.id.clone(),
                        message.timestamp,
                        message.content.trim().to_string(),
                    ));
                } else {
                    existing_real_ids.insert(message.id.clone());
                }
            }
        }
        None => return messages,
    }
    if candidates.is_empty() {
        return messages;
    }

    let mut consumed = vec![false; candidates.len()];
    let mut upgrades: Vec<(Arc<str>, ChatMessage)> = Vec::new();
    let mut paired_real_ids: HashSet<Arc<str>> = HashSet::new();
    messages.retain(|incoming| {
        if !history_message_is_local_author(incoming, &own_login, &own_display) {
            return true;
        }
        if existing_real_ids.contains(&incoming.id) || paired_real_ids.contains(&incoming.id) {
            return true;
        }
        let incoming_content = incoming.content.trim();
        let mut best: Option<(usize, i64)> = None;
        for (idx, (_, ts, content)) in candidates.iter().enumerate() {
            if consumed[idx] || content != incoming_content {
                continue;
            }
            let delta = (incoming.timestamp - ts).abs();
            if delta > OPTIMISTIC_HISTORY_RECONCILE_WINDOW_MS {
                continue;
            }
            if best.is_none_or(|(_, best_delta)| delta < best_delta) {
                best = Some((idx, delta));
            }
        }
        match best {
            Some((idx, _)) => {
                consumed[idx] = true;
                paired_real_ids.insert(incoming.id.clone());
                upgrades.push((candidates[idx].0.clone(), incoming.clone()));
                false
            }
            None => true,
        }
    });

    if upgrades.is_empty() {
        return messages;
    }

    let mut upgraded_any = false;
    if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel_lower) {
        for (optimistic_id, real_msg) in &upgrades {
            let real = real_msg.clone();
            let ok = buffer.update_by_id(optimistic_id.as_ref(), move |row| {
                row.id = real.id;
                row.user_id = real.user_id;
                row.timestamp = real.timestamp;
                if real.color.is_some() {
                    row.color = real.color;
                }
                if !real.badges.is_empty() {
                    row.badges = real.badges;
                }
                row.content = real.content;
                row.slots = real.slots;
                row.structure_hash = real.structure_hash;
                row.twitch_emotes_tag = real.twitch_emotes_tag;
                row.reply_emote_offset = real.reply_emote_offset;
                if row.reply.is_none() {
                    row.reply = real.reply;
                }
            });
            upgraded_any |= ok;
        }
    }
    if !upgraded_any {
        return messages;
    }

    for (optimistic_id, real_msg) in &upgrades {
        mark_message_seen(&channel_lower, real_msg.id.as_ref());
        crate::state::layout_eviction::queue_evicted_id(optimistic_id.clone());
        crate::log_irc!(
            debug,
            "Reconciled optimistic row {} with real id {} in #{} (history)",
            optimistic_id,
            real_msg.id,
            channel_lower
        );
    }

    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        &channel_lower,
        crate::backend::observers::ChannelEvent::MessagesRetokenized,
    );

    messages
}

async fn merge_visible_history_messages(channel: &str, messages: Vec<ChatMessage>) -> usize {
    if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        return 0;
    }
    let mut messages = messages;
    prepare_history_emote_scopes(&mut messages, channel).await;
    // Retokenize before taking the gate: every live PRIVMSG commit and the
    // UI's visible-row reads wait on it, and a large history batch takes
    // milliseconds to parse. A catalog that changed while parsing gets one
    // more pass under the gate so committed rows match the current catalog.
    let parsed_against = history_emote_versions(channel);
    for message in &mut messages {
        reparse_message_slots(message, channel, true);
    }
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        return 0;
    }
    if history_emote_versions(channel) != parsed_against {
        for message in &mut messages {
            reparse_message_slots(message, channel, true);
        }
    }
    let messages = crate::backend::history::apply_blocked_visibility(messages, channel);
    let mut messages = reconcile_optimistic_history_rows(channel, messages);
    if is_channel_closed(channel) {
        crate::log_irc!(
            debug,
            "Dropping late history rows for closed channel {}",
            channel
        );
        return 0;
    }
    let defer_visibility = crate::backend::blocked_users::initial_visibility_pending();
    let mut inserted_ids = Vec::new();
    if !messages.is_empty() {
        let mut buffer = CHAT_MESSAGES
            .entry(channel.to_string())
            .or_insert_with(MessageBuffer::with_default_capacity);
        let existing_ids: HashSet<Arc<str>> =
            buffer.iter().map(|message| message.id.clone()).collect();
        let mut seen_ids = HashSet::new();
        messages.retain(|message| {
            !existing_ids.contains(&message.id) && seen_ids.insert(message.id.clone())
        });
        if defer_visibility {
            let remaining =
                crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held();
            if remaining == 0 {
                pending_channel_row_capacity_available_commit_held();
                drop(buffer);
                drop(visibility_commit);
                return 0;
            }

            if messages.len() > remaining {
                let drop_count = messages.len() - remaining;
                messages.drain(..drop_count);
                log_pending_channel_overflow();
            }
            inserted_ids.extend(messages.iter().map(|message| message.id.clone()));
        }
        let admitted = messages.len();
        if !defer_visibility {
            for message in &messages {
                track_visible_history_chatter(channel, message);
            }
        }
        buffer.merge_history(messages);
        if defer_visibility {
            inserted_ids.retain(|id| buffer.contains_id(id.as_ref()));
        }
        drop(buffer);
        for message_id in inserted_ids {
            defer_buffered_row_visibility_commit_held(channel, message_id.as_ref());
        }
        drop(visibility_commit);
        return admitted;
    }
    drop(visibility_commit);
    0
}

fn history_emote_versions(channel: &str) -> (u32, u32) {
    (
        crate::backend::emotes::get_global_emote_version(),
        crate::backend::emotes::get_emote_cache_version(channel),
    )
}

async fn prepare_history_emote_scopes(messages: &mut [ChatMessage], fallback_channel: &str) {
    use futures_util::StreamExt;
    let _ = tokio::time::timeout(
        INITIAL_GLOBAL_EMOTE_READY_TIMEOUT,
        crate::backend::emotes::wait_for_initial_catalogs_ready(),
    )
    .await;
    let preparation = async {
        let mut unresolved_by_id: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, message) in messages.iter().enumerate() {
            if message.emote_lookup_channel.as_deref() == Some("")
                && let Some(source_id) = message.emote_lookup_source_id.as_deref()
            {
                unresolved_by_id
                    .entry(source_id.to_string())
                    .or_default()
                    .push(index);
            }
        }

        let mut resolutions = futures_util::stream::iter(unresolved_by_id)
            .map(|(source_id, indices)| async move {
                let source_login =
                    crate::backend::twitch_api::get_channel_login_by_id_cached(&source_id).await;
                (source_id, indices, source_login)
            })
            .buffer_unordered(4);
        while let Some((source_id, indices, source_login)) = resolutions.next().await {
            if let Some(source_login) = source_login {
                crate::backend::twitch_api::cache_channel_id(&source_login, &source_id);
                let source_login: Arc<str> = source_login.into();
                for index in indices {
                    messages[index].emote_lookup_channel = Some(Arc::clone(&source_login));
                    messages[index].emote_lookup_source_id = None;
                }
            }
        }

        let mut sources = HashSet::new();
        if !crate::backend::whispers::is_whispers_channel(fallback_channel) {
            sources.insert(fallback_channel.to_string());
        }
        for message in messages.iter() {
            if let Some(source) = message
                .emote_lookup_channel
                .as_deref()
                .filter(|source| !source.is_empty())
            {
                sources.insert(source.to_string());
            }
        }
        futures_util::stream::iter(sources)
            .map(|source| async move {
                ensure_channel_emotes_ready_for_history(&source, None).await
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
    };
    let _ = tokio::time::timeout(HISTORY_EMOTE_READY_TIMEOUT, preparation).await;
}

/// Publish a history snapshot under the same gate as membership replacement.
/// Retokenization stays outside the gate; this final prune/snapshot/observer
/// commit makes its result current without holding the gate during parsing.
fn notify_visible_history_loaded(channel: &str) -> bool {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let blocked_rows_changed = apply_blocked_user_filter_to_channel(channel);
    if crate::backend::blocked_users::initial_visibility_pending() {
        return false;
    }
    let messages_for_observers = snapshot_channel_messages_for_observers(channel);
    drop(visibility_commit);
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::HistoryLoaded(messages_for_observers),
    );
    if blocked_rows_changed {
        dispatch_blocked_channel_refreshes(HashSet::from([channel.to_lowercase()]));
    }
    true
}

/// Load channel history asynchronously (extracted for reuse)
/// Prevents duplicate loads from multiple splits showing the same channel
async fn load_channel_history_async(channel: &str) {
    let channel_lower = channel.to_lowercase();

    {
        let mut loaded = HISTORY_LOADED_CHANNELS.lock();

        if loaded.contains(&channel_lower) {
            crate::log_irc!(
                debug,
                "History already loaded for {}, skipping duplicate load",
                channel
            );
            return;
        }

        loaded.insert(channel_lower.clone());
    }

    crate::backend::emote_candidates::mark_channel_loading(&channel_lower);

    // The disk merge can wait a long time on emote readiness (shared-chat rows
    // resolve their source channels over the network), so the network fetch
    // runs alongside it instead of after it. Join-time fetches skip while this
    // pass is in flight, so it must not be held up by the disk merge.
    let cached = async {
        if !crate::backend::settings::get_persist_history_enabled() {
            return;
        }
        let local = crate::backend::history_cache::load_channel_history(channel);
        if local.is_empty() {
            return;
        }
        let n = merge_visible_history_messages(channel, local).await;
        if n > 0 && notify_visible_history_loaded(channel) {
            bump_chat_version();
            crate::startup_metrics::record("cached_history_committed");
        }

        crate::log_irc!(
            info,
            "Eager-loaded {} cached history messages for {} (instant paint)",
            n,
            channel
        );
    };

    let network = async {
        match crate::backend::history::fetch_recent_messages_streaming(channel).await {
            Ok(result) => {
                if let Some(error) = result.cache_fallback_error.as_deref() {
                    add_history_error_message(
                        channel,
                        "Showing cached history because recent messages could not be loaded.",
                        error,
                    );
                }

                crate::log_irc!(
                    info,
                    "Loaded {} initial history messages for {} (immediate)",
                    result.messages.len(),
                    channel
                );

                merge_visible_history_messages(channel, result.messages).await;
                record_history_loaded_now(&channel_lower);

                if notify_visible_history_loaded(channel) {
                    bump_chat_version();
                }
            }
            Err(e) => {
                crate::log_irc!(
                    warn,
                    "Immediate history fetch failed for {}: {}",
                    channel,
                    e
                );
                add_history_error_message(channel, "Recent messages could not be loaded.", &e);

                {
                    let mut loaded = HISTORY_LOADED_CHANNELS.lock();
                    loaded.remove(&channel_lower);
                }
            }
        }
    };

    tokio::join!(cached, network);
}

/// True when the already-lowercased channel is in the desired set.
pub fn is_channel_desired(channel_lower: &str) -> bool {
    DESIRED_CHANNELS.read().contains(channel_lower)
}

/// Get all desired channels
pub fn get_desired_channels() -> Vec<String> {
    DESIRED_CHANNELS.read().iter().cloned().collect()
}

/// Run the read connection with automatic reconnection (never gives up)
/// This is the ONLY supervisor - manages ALL channels from DESIRED_CHANNELS
/// Only ONE instance should run at a time (enforced by SUPERVISOR_RUNNING flag)
async fn run_supervised_read_connection() {
    crate::log_irc!(info, "Supervisor started");

    crate::log_irc!(info, "Waiting for auth initialization...");
    while !crate::backend::auth::is_auth_initialized() {
        if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
            SUPERVISOR_RUNNING.store(false, Ordering::Release);
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    crate::log_irc!(
        info,
        "Auth initialization complete, proceeding with connection"
    );

    loop {
        if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
            crate::log_irc!(info, "Supervisor shutdown requested, exiting");
            SUPERVISOR_RUNNING.store(false, Ordering::SeqCst);
            break;
        }

        let channels: Vec<String> = get_desired_channels()
            .into_iter()
            .filter(|channel| twitch_irc::validate::validate_login(channel).is_ok())
            .collect();
        if channels.is_empty() {
            crate::log_irc!(info, "Supervisor: No joinable channels, waiting...");
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            continue;
        }

        {
            let mut connected = CONNECTED_CHANNELS.write();
            for ch in &channels {
                connected.remove(ch);
            }
        }

        crate::log_irc!(
            info,
            "Starting read connection for {} channel(s): {:?}",
            channels.len(),
            channels
        );

        let connection_generation = CONNECTION_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        let result = connect_and_listen_inner(channels.clone(), connection_generation).await;

        if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
            crate::log_irc!(info, "Read connection stopped for backend shutdown");
            set_connection_status(ConnectionStatus::Disconnected);
            break;
        }

        record_all_channel_disconnects();
        set_connection_status(ConnectionStatus::Reconnecting);

        let delay = get_and_increment_read_backoff();
        match &result {
            Ok(()) => {
                crate::log_irc!(
                    info,
                    "IRC read connection ended normally; reconnecting in {}ms",
                    delay
                );
            }
            Err(e) => {
                crate::log_irc!(
                    warn,
                    "IRC read connection lost ({}); reconnecting in {}ms",
                    e,
                    delay
                );
            }
        }
        for channel in &channels {
            add_connection_status_message(
                channel,
                &format!(
                    "Connection lost. Reconnecting in {:.1}s.",
                    delay as f64 / 1000.0
                ),
                "reconnecting",
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
    }
}

/// First self-JOIN for a connect-time channel: the server has joined it, so
/// announce the connection and run exactly one history pass. A channel with a
/// pending outage and a warm buffer is gap-filled; otherwise the recent
/// history is streamed as on first connect.
fn handle_session_channel_joined(channel: String, announce: bool) {
    if announce {
        add_connection_status_message(&channel, "Connected to chat.", "connected");
    }

    tokio::spawn(async move {
        let has_gap = CHANNEL_DISCONNECT_STATES.read().contains_key(&channel)
            && CHAT_MESSAGES
                .get(&channel)
                .is_some_and(|buffer| !buffer.is_empty());
        if has_gap {
            let _permit = RECONNECT_GAP_FILL_PERMITS.acquire().await;
            match fill_in_missing_messages(&channel).await {
                Ok(count) => {
                    if count > 0 {
                        crate::log_irc!(info, "Gap-filled {} messages for {}", count, channel);
                    }
                }
                Err(e) => {
                    crate::log_irc!(warn, "Gap-fill failed for {}: {}", channel, e);
                    add_history_error_message(
                        &channel,
                        "Recent messages could not be recovered after reconnect.",
                        &e,
                    );
                }
            }
            return;
        }
        take_channel_disconnect_state(&channel);

        if history_recently_loaded(&channel) {
            crate::log_irc!(
                debug,
                "Skipping streaming history for {}: prefetched moments ago",
                channel
            );
            return;
        }
        if history_load_in_flight(&channel) {
            crate::log_irc!(
                debug,
                "Skipping streaming history for {}: startup prefetch still running",
                channel
            );
            return;
        }

        match crate::backend::history::fetch_recent_messages_streaming(&channel).await {
            Ok(result) => {
                if let Some(error) = result.cache_fallback_error.as_deref() {
                    add_history_error_message(
                        &channel,
                        "Showing cached history because recent messages could not be loaded.",
                        error,
                    );
                }

                crate::log_irc!(
                    info,
                    "Loaded {} initial history messages for {} (streaming)",
                    result.messages.len(),
                    channel
                );

                merge_visible_history_messages(&channel, result.messages).await;
                record_history_loaded_now(&channel);

                if notify_visible_history_loaded(&channel) {
                    bump_chat_version();
                }
            }
            Err(e) => {
                crate::log_irc!(warn, "Failed to fetch history for {}: {}", channel, e);
                add_history_error_message(&channel, "Recent messages could not be loaded.", &e);
            }
        }
    });
}

/// Fill in missing messages for a channel after reconnection
/// Uses per-channel disconnect state to:
/// 1. Calculate how many messages to fetch based on disconnect duration
/// 2. Filter messages to only those after the last received message
/// 3. Insert at correct chronological position
///
/// PERF: Uses DashMap for per-channel access and MessageBuffer for O(N) merge
async fn fill_in_missing_messages(channel: &str) -> Result<usize, String> {
    let channel_lower = channel.to_lowercase();

    let disconnect_state = match take_channel_disconnect_state(&channel_lower) {
        Some(state) => state,
        None => {
            crate::log_irc!(
                debug,
                "No disconnect state for {}, skipping gap-fill",
                channel_lower
            );
            return Ok(0);
        }
    };

    let now_ms = crate::utils::unix_now_ms();
    let disconnect_secs = (now_ms - disconnect_state.disconnect_time_ms) / 1000;
    let limit = crate::config::calculate_adaptive_history_limit(disconnect_secs);

    crate::log_irc!(
        info,
        "Gap-fill {}: disconnect was {}s ago, fetching up to {} messages (last_msg_ts: {})",
        channel_lower,
        disconnect_secs,
        limit,
        disconnect_state.last_message_time_ms
    );

    let fetched_msgs = match crate::backend::history::fetch_recent_messages_with_limit(
        &channel_lower,
        limit,
    )
    .await
    {
        Ok(msgs) => msgs,
        Err(error) => {
            restore_channel_disconnect_state(&channel_lower, disconnect_state);
            return Err(error);
        }
    };

    if fetched_msgs.is_empty() {
        return Ok(0);
    }

    let existing_ids: HashSet<Arc<str>> = CHAT_MESSAGES
        .get(&channel_lower)
        .map(|buffer| buffer.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default();

    let disconnect_ts = disconnect_state.last_message_time_ms;
    let gap_messages: Vec<ChatMessage> = fetched_msgs
        .into_iter()
        .filter(|msg| msg.timestamp > disconnect_ts && !existing_ids.contains(&msg.id))
        .collect();

    if gap_messages.is_empty() {
        crate::log_irc!(
            debug,
            "No new gap messages for {} (filtered by timestamp/existing)",
            channel_lower
        );
        return Ok(0);
    }

    let gap_messages: Vec<ChatMessage> = gap_messages
        .into_iter()
        .filter(|msg| mark_message_seen(&channel_lower, &msg.id))
        .collect();

    if gap_messages.is_empty() {
        crate::log_irc!(
            debug,
            "No new gap messages for {} (all duplicates in dedup cache)",
            channel_lower
        );
        return Ok(0);
    }

    let gap_count = merge_visible_history_messages(&channel_lower, gap_messages).await;
    if gap_count == 0 {
        crate::log_irc!(
            debug,
            "No visible gap messages remained for {} after final blocked-user recheck",
            channel_lower
        );
        return Ok(0);
    }

    if notify_visible_history_loaded(&channel_lower) {
        bump_chat_version();
    }

    crate::log_irc!(
        info,
        "Gap-fill: Inserted {} gap messages for channel {}",
        gap_count,
        channel_lower
    );

    Ok(gap_count)
}

/// Internal connection function that runs until disconnection
/// Returns Ok(()) on normal exit, Err on error
async fn connect_and_listen_inner(
    mut channels: Vec<String>,
    connection_generation: u64,
) -> Result<(), String> {
    if channels.is_empty() {
        return Err("No channels to connect".to_string());
    }

    join_scheduler::prioritize_channels(&mut channels);
    let first_channel = channels[0].clone();
    crate::log_irc!(debug, "Connecting to IRC for channels: {:?}", channels);

    set_connection_status(ConnectionStatus::Reconnecting);

    let use_anon = is_anonymous_mode();
    let anonymous_connection = use_anon || get_token().is_none() || get_username().is_none();

    clear_self_channel_states();
    reset_self_joined_channels();
    SYSTEM_SUSPENDED.store(false, Ordering::Release);
    *SESSION_PENDING_JOIN_CHANNELS.write() = channels
        .iter()
        .map(|channel| channel.trim_start_matches('#').to_lowercase())
        .collect();
    *SESSION_PENDING_ANNOUNCE_CHANNELS.write() = SESSION_PENDING_JOIN_CHANNELS.read().clone();

    let mut credential_fence = None;
    let config = if !use_anon {
        if let (Some(token), Some(username)) = (get_token(), get_username()) {
            let clean_token = token.strip_prefix("oauth:").unwrap_or(&token).to_string();
            credential_fence = crate::backend::auth::capture_irc_credential_fence(&clean_token);
            crate::log_irc!(
                info,
                "Using authenticated IRC (user: {}, token_len: {})",
                username,
                clean_token.len()
            );
            ClientConfig::new_simple(StaticLoginCredentials::new(username, Some(clean_token)))
        } else {
            crate::log_irc!(info, "Using anonymous IRC (no credentials)");
            ClientConfig::default()
        }
    } else {
        crate::log_irc!(
            info,
            "Using anonymous IRC (auth previously failed or forced)"
        );
        ClientConfig::default()
    };

    let self_login = config.login_credentials.credentials.login.to_lowercase();
    let (mut incoming_messages, client) =
        TwitchIRCClient::<EchoIrcTransport, StaticLoginCredentials>::new(config);

    IRC_SENDER.store(Arc::new(Some(client.clone())));

    for channel in &channels {
        let channel_norm = channel.to_lowercase();

        CONNECTED_CHANNELS.write().insert(channel_norm.clone());

        if let Err(e) = client.join(channel.clone()) {
            crate::log_irc!(error, "Failed to join channel {}: {}", channel, e);
            CONNECTED_CHANNELS.write().remove(&channel_norm);
            continue;
        }

        crate::log_irc!(info, "Queued channel JOIN: {}", channel);
        if anonymous_connection {
            cache_self_state(channel, None, Vec::new());
        }

        let channel_clone = channel.clone();
        BADGE_LOADED_CHANNELS.lock().insert(channel.to_lowercase());

        tokio::spawn(async move {
            match TWITCH_BADGE_CACHE
                .load_channel_badges_by_login(&channel_clone)
                .await
            {
                Ok(()) => {}
                Err(e) if e == crate::backend::constants::API_PROXY_NOT_CONFIGURED => {
                    crate::log_cache!(debug, "Skipped badges for {}: {}", channel_clone, e);
                }
                Err(e) => {
                    crate::log_cache!(warn, "Failed to load badges for {}: {}", channel_clone, e);
                }
            }
        });

        let emote_channel = channel.clone();
        tokio::spawn(async move {
            ensure_channel_emotes_loaded(&emote_channel, None).await;
        });
    }

    MESSAGES_SINCE_CONNECT.store(0, Ordering::Relaxed);

    tokio::spawn(async move {
        tokio::time::sleep(SESSION_JOIN_FALLBACK_DELAY).await;
        let stragglers = {
            let mut pending = SESSION_PENDING_JOIN_CHANNELS.write();
            if CONNECTION_GENERATION.load(Ordering::Acquire) != connection_generation
                || MESSAGES_SINCE_CONNECT.load(Ordering::Relaxed) == 0
            {
                return;
            }
            std::mem::take(&mut *pending)
        };
        for channel in stragglers {
            crate::log_irc!(
                info,
                "JOIN still pending for {}; loading its history independently",
                channel
            );
            handle_session_channel_joined(channel, false);
        }
    });

    let client_for_sync = client.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            if CONNECTION_GENERATION.load(Ordering::Acquire) != connection_generation {
                crate::log_irc!(debug, "Channel-sync exiting for stale IRC connection");
                break;
            }

            tokio::select! {
                _ = CHANNEL_JOIN_NOTIFY.notified() => {
                    crate::log_irc!(debug, "Channel-sync woken by notification - immediate join");
                }
                _ = interval.tick() => {
                }
            }

            if CONNECTION_GENERATION.load(Ordering::Acquire) != connection_generation {
                crate::log_irc!(debug, "Channel-sync exiting after IRC reconnect request");
                break;
            }

            let desired = get_desired_channels();
            let desired_set: HashSet<String> = desired
                .iter()
                .map(|channel| channel.to_lowercase())
                .collect();
            let connected: HashSet<String> = CONNECTED_CHANNELS.read().clone();

            for channel in connected
                .iter()
                .filter(|channel| !desired_set.contains(*channel))
            {
                crate::log_irc!(info, "Channel-sync: Parting removed channel: {}", channel);
                clear_channel_runtime_state(channel);
                client_for_sync.part(channel.clone());
            }

            for channel in desired {
                let channel_norm = channel.to_lowercase();
                if !connected.contains(&channel_norm) {
                    crate::log_irc!(
                        info,
                        "Channel-sync: Joining newly added channel: {}",
                        channel
                    );

                    CONNECTED_CHANNELS.write().insert(channel_norm.clone());

                    if let Err(e) = client_for_sync.join(channel.clone()) {
                        crate::log_irc!(error, "Channel-sync: Failed to join {}: {}", channel, e);
                        CONNECTED_CHANNELS.write().remove(&channel_norm);
                        continue;
                    }
                    if anonymous_connection {
                        cache_self_state(&channel, None, Vec::new());
                    }

                    let emote_channel = channel.clone();
                    tokio::spawn(async move {
                        ensure_channel_emotes_loaded(&emote_channel, None).await;
                    });

                    let channel_clone2 = channel.clone();
                    tokio::spawn(async move {
                        load_channel_history_async(&channel_clone2).await;
                    });
                }
            }
        }
    });

    crate::log_irc!(info, "Starting message loop for channels: {:?}", channels);

    let connect_deadline = tokio::time::Instant::now() + CONNECT_RESPONSE_DEADLINE;
    loop {
        let message = tokio::select! {
            _ = CONNECTION_RESTART_NOTIFY.notified() => {
                if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                    break;
                }
                if !restart_request_applies(
                    connection_generation,
                    RESTART_TARGET_GENERATION.load(Ordering::Acquire),
                    CONNECTION_GENERATION.load(Ordering::Acquire),
                ) {
                    crate::log_irc!(debug, "Ignoring restart request aimed at a superseded IRC connection");
                    continue;
                }
                crate::log_irc!(info, "IRC reconnect requested for updated account credentials");
                return Err("IRC reconnect requested after account change".to_string());
            }
            _ = tokio::time::sleep_until(connect_deadline),
                if MESSAGES_SINCE_CONNECT.load(Ordering::Relaxed) == 0 =>
            {
                crate::log_irc!(
                    warn,
                    "No IRC server response within {}s of connecting",
                    CONNECT_RESPONSE_DEADLINE.as_secs()
                );
                IRC_SENDER.store(Arc::new(None));
                return Err("IRC server did not respond after connecting".to_string());
            }
            maybe_message = incoming_messages.recv() => {
                match maybe_message {
                    Some(message) => message,
                    None => break,
                }
            }
        };

        if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
            break;
        }

        let stale_connection = CONNECTION_GENERATION.load(Ordering::Acquire)
            != connection_generation
            || (!anonymous_connection
                && credential_fence
                    .as_ref()
                    .is_none_or(|fence| !fence.generation_is_current()));
        if stale_connection {
            return Err("Discarding queued event from a superseded IRC connection".to_string());
        }

        if MESSAGES_SINCE_CONNECT.fetch_add(1, Ordering::Relaxed) == 0 {
            reset_read_backoff();
        }
        record_activity(connection_generation);
        crate::managers::frame_rate_controller::record_irc_message();

        if let ServerMessage::Notice(n) = &message
            && n.message_text.contains("Login authentication failed")
        {
            return recover_from_irc_auth_failure(credential_fence.as_ref()).await;
        }

        match message {
            ServerMessage::Privmsg(msg) => {
                process_privmsg_message(msg, &first_channel).await;
            }
            ServerMessage::Whisper(msg) => {
                process_whisper_message(msg).await;
            }
            ServerMessage::UserState(msg) => {
                process_userstate_message(msg, &self_login);
            }
            ServerMessage::ClearChat(msg) => {
                process_clearchat_message(msg);
            }
            ServerMessage::ClearMsg(msg) => {
                process_clearmsg_message(msg);
            }
            ServerMessage::UserNotice(msg) => {
                process_usernotice_message(msg);
            }
            ServerMessage::RoomState(msg) => {
                let channel = msg.channel_login.to_lowercase();
                if SELF_JOINED_CHANNELS.read().contains(&channel) {
                    let announce = SESSION_PENDING_ANNOUNCE_CHANNELS.write().remove(&channel);
                    if SESSION_PENDING_JOIN_CHANNELS.write().remove(&channel) {
                        handle_session_channel_joined(channel, announce);
                    } else if announce {
                        add_connection_status_message(&channel, "Connected to chat.", "connected");
                    }
                }
                crate::backend::emotes::seventv_eventapi::subscribe_channel_entitlements(
                    &msg.channel_login,
                    &msg.channel_id,
                );
                crate::backend::irc::room_state::process_room_state(msg);
            }
            ServerMessage::Notice(msg) => {
                if let Some(channel) = msg.channel_login.as_deref() {
                    note_irc_send_rejection(
                        &self_login,
                        channel,
                        msg.message_id.as_deref().unwrap_or_default(),
                        &msg.message_text,
                    );
                }
                if let Some(channel) = msg.channel_login.as_deref() {
                    let channel = channel.to_lowercase();
                    if msg.message_id.as_deref() == Some("msg_channel_suspended") {
                        crate::backend::twitch_api::note_channel_suspended(&channel);
                    }
                }
                let text = msg.message_text.trim();
                if !text.is_empty()
                    && let Some(channel) = msg.channel_login.as_deref()
                {
                    let message_id = msg
                        .message_id
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let is_error = message_id.starts_with("msg_")
                        || message_id.starts_with("bad_")
                        || message_id == "no_permission";
                    add_system_message(
                        channel,
                        text,
                        if is_error {
                            SystemMessageType::Error {
                                message: text.to_string(),
                            }
                        } else {
                            SystemMessageType::Info
                        },
                    );
                }
            }
            ServerMessage::Join(msg) => {
                if msg.user_login.eq_ignore_ascii_case(&self_login) {
                    handle_self_join(&msg.channel_login);
                }
                if crate::backend::settings::get_show_join_part_messages() {
                    let text = format!("{} joined the channel", msg.user_login);
                    add_system_message(&msg.channel_login, &text, SystemMessageType::Info);
                }
            }
            ServerMessage::Part(msg) if crate::backend::settings::get_show_join_part_messages() => {
                let text = format!("{} left the channel", msg.user_login);
                add_system_message(&msg.channel_login, &text, SystemMessageType::Info);
            }
            _ => {}
        }
    }

    if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        crate::log_irc!(info, "Message loop stopped for backend shutdown");
        return Ok(());
    }

    // The supervisor logs this "down" transition with its reconnect delay.
    Err("Connection lost".to_string())
}

pub(crate) fn normalize_message_text(raw: &str) -> (String, bool) {
    if raw.starts_with("\x01ACTION ") && raw.ends_with('\x01') && raw.len() >= 9 {
        return (raw[8..raw.len() - 1].to_string(), true);
    }
    if let Some(stripped) = raw.strip_prefix("/me ") {
        return (stripped.to_string(), true);
    }
    (raw.to_string(), false)
}

/// Preload emotes and embeds from message slots immediately on receipt
/// This ensures content is downloaded/fetched BEFORE entering viewport, eliminating blank spaces during scrolling.
fn preload_message_content(slots: &[MessageSlot], channel: &str, msg_id: &str) {
    use crate::backend::slots::resolver::{ResolvedSlot, SlotResolver};

    let trace_preload = crate::log::emote_trace_enabled();
    if trace_preload {
        crate::log_emotes!(
            debug,
            "Preload: Processing message {} in channel {} with {} slots",
            msg_id,
            channel,
            slots.len()
        );
    }

    let mut emote_urls: Vec<String> = Vec::new();
    let resolver = SlotResolver::new(channel);

    for slot in slots {
        if trace_preload {
            crate::log_emotes!(debug, "Preload: Slot type: {:?}", slot);
        }
        match slot {
            MessageSlot::Emote(emote) => {
                let url = crate::backend::emotes::chat_display_url(emote);
                emote_urls.push(url.to_string());
            }

            MessageSlot::TwitchEmote { .. } => {
                if let ResolvedSlot::Emote(emote) = resolver.resolve(slot) {
                    let url = crate::backend::emotes::chat_display_url(&emote);
                    emote_urls.push(url.to_string());
                }
            }

            MessageSlot::Url { .. } => {}

            _ => {}
        }
    }

    if !emote_urls.is_empty() {
        if trace_preload {
            crate::log_emotes!(
                debug,
                "Preload: Triggering lazy preload for {} emote URLs in message {}",
                emote_urls.len(),
                msg_id
            );
        }
        crate::cache::loader::trigger_lazy_preload_with_source(
            emote_urls,
            crate::cache::loader::PreloadSource::IrcMessage,
        );
    } else if trace_preload {
        crate::log_emotes!(
            debug,
            "Preload: No emotes to preload in message {} ({} slots total)",
            msg_id,
            slots.len()
        );
    }
}

fn schedule_staggered_channel_emote_load(
    channel_id: String,
    channel_login: String,
    cached_count: usize,
    reason: &'static str,
) {
    let channel_key = channel_login.to_lowercase();
    if DEFERRED_CHANNEL_EMOTE_REFRESHES
        .insert(channel_key.clone(), ())
        .is_some()
    {
        crate::backend::metrics::record_emote_event("irc.channel_emotes.staggered.coalesced");
        crate::log_emotes!(
            debug,
            "Staggered channel emote load already queued for {}",
            channel_key
        );
        return;
    }

    let slot = DEFERRED_CHANNEL_EMOTE_REFRESH_SLOT.fetch_add(1, Ordering::Relaxed);
    let force_refresh = cached_count > 0;
    let (base_delay, stagger) = if force_refresh {
        (
            CACHED_CHANNEL_EMOTE_REFRESH_BASE_DELAY_MS,
            CACHED_CHANNEL_EMOTE_REFRESH_STAGGER_MS,
        )
    } else {
        (0, CACHED_CHANNEL_EMOTE_INITIAL_STAGGER_MS)
    };
    let delay_ms = base_delay
        .saturating_add(slot.saturating_mul(stagger))
        .min(CACHED_CHANNEL_EMOTE_REFRESH_MAX_DELAY_MS);
    let scheduled_stage = if force_refresh {
        "irc.channel_emotes.deferred_cached.scheduled"
    } else {
        "irc.channel_emotes.staggered_initial.scheduled"
    };
    crate::backend::metrics::record_emote_stage(
        scheduled_stage,
        std::time::Duration::from_millis(delay_ms),
        cached_count,
    );
    if force_refresh {
        crate::log_emotes!(
            debug,
            "Using cached emotes for {} immediately ({} emotes); deferred refresh in {}ms ({})",
            channel_key,
            cached_count,
            delay_ms,
            reason
        );
    } else {
        crate::log_emotes!(
            debug,
            "Scheduling initial channel emote load for {} in {}ms ({})",
            channel_key,
            delay_ms,
            reason
        );
    }

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let started_at = std::time::Instant::now();
        let outcome = EMOTE_CACHE
            .load_channel_emotes_reporting(&channel_id, &channel_key, force_refresh)
            .await;

        match outcome {
            Ok(crate::backend::emotes::LoadOutcome::Coalesced) => {
                crate::backend::metrics::record_emote_event(
                    "irc.channel_emotes.staggered.coalesced_retry",
                );
                let retry_channel_id = channel_id.clone();
                let retry_channel_key = channel_key.clone();
                crate::backend::bridge::RUNTIME.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        COALESCED_CHANNEL_EMOTE_RETRY_DELAY_MS,
                    ))
                    .await;
                    if !crate::backend::emotes::channel_has_lookup_entry(&retry_channel_key) {
                        crate::log_emotes!(
                            debug,
                            "Coalesced staggered load left {} unresolved; forcing one retry",
                            retry_channel_key
                        );
                        let _ = EMOTE_CACHE
                            .load_channel_emotes_reporting(
                                &retry_channel_id,
                                &retry_channel_key,
                                true,
                            )
                            .await;
                    }
                    DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&retry_channel_key);
                });
            }
            Ok(_) => {
                DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&channel_key);
                let count = EMOTE_CACHE.channel_emote_count(&channel_key);
                let completed_stage = if force_refresh {
                    "irc.channel_emotes.deferred_cached.completed"
                } else {
                    "irc.channel_emotes.staggered_initial.completed"
                };
                crate::backend::metrics::record_emote_stage(
                    completed_stage,
                    started_at.elapsed(),
                    count,
                );
                crate::log_emotes!(
                    debug,
                    "Staggered channel emote load complete for {} ({} emotes)",
                    channel_key,
                    count
                );
            }
            Err(e) => {
                DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&channel_key);
                let failed_stage = if force_refresh {
                    "irc.channel_emotes.deferred_cached.failed"
                } else {
                    "irc.channel_emotes.staggered_initial.failed"
                };
                crate::backend::metrics::record_emote_event(failed_stage);
                crate::backend::metrics::record_emote_stage(failed_stage, started_at.elapsed(), 0);
                crate::log_emotes!(
                    debug,
                    "Staggered channel emote load failed for {}: {}",
                    channel_key,
                    e
                );
            }
        }
    });
}

async fn load_or_defer_channel_emotes(
    channel_id: String,
    channel_login: String,
    reason: &'static str,
) {
    let cached_count = EMOTE_CACHE.channel_emote_count(&channel_login);
    schedule_staggered_channel_emote_load(channel_id, channel_login, cached_count, reason);
}

async fn ensure_channel_emotes_loaded(channel_login: &str, channel_id_hint: Option<&str>) {
    let channel_key = channel_login.to_lowercase();
    {
        let mut failures = EMOTE_CHANNEL_ID_FAILURES.lock();
        if let Some(failed_at) = failures.get(&channel_key) {
            if failed_at.elapsed() < EMOTE_CHANNEL_ID_RETRY {
                return;
            }
            failures.remove(&channel_key);
        }
    }
    let should_load_emotes = {
        let mut loaded = EMOTE_LOADED_CHANNELS.lock();
        if !loaded.contains(&channel_key) {
            loaded.insert(channel_key.clone());
            true
        } else {
            false
        }
    };

    if !should_load_emotes {
        return;
    }

    let resolved_channel_id = if let Some(id) = channel_id_hint {
        id.to_string()
    } else if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(channel_login) {
        id
    } else {
        match crate::backend::twitch_api::get_user_id_cached(channel_login).await {
            Ok(id) => id,
            Err(err) => {
                if err == crate::backend::constants::API_PROXY_NOT_CONFIGURED {
                    crate::log_emotes!(
                        debug,
                        "Skipped channel id lookup for {} while loading emotes: {}",
                        channel_login,
                        err
                    );
                } else {
                    crate::log_emotes!(
                        warn,
                        "Failed to resolve channel id for {} while loading emotes: {}",
                        channel_login,
                        err
                    );
                }
                EMOTE_LOADED_CHANNELS.lock().remove(&channel_key);
                let mut failures = EMOTE_CHANNEL_ID_FAILURES.lock();
                if failures.len() >= 256 {
                    failures.retain(|_, failed_at| failed_at.elapsed() < EMOTE_CHANNEL_ID_RETRY);
                }
                failures.insert(channel_key, std::time::Instant::now());
                return;
            }
        }
    };

    crate::backend::twitch_api::cache_channel_id(channel_login, &resolved_channel_id);

    let load_login = channel_login.to_string();
    tokio::spawn(async move {
        load_or_defer_channel_emotes(resolved_channel_id, load_login, "irc.ensure").await;
    });
}

const INITIAL_GLOBAL_EMOTE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const HISTORY_EMOTE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

struct HistoryEmoteReadinessGuard {
    channel: String,
    committed: bool,
}

impl HistoryEmoteReadinessGuard {
    fn new(channel: &str) -> Self {
        publish_history_emote_readiness(channel, false);
        Self {
            channel: channel.to_string(),
            committed: false,
        }
    }

    fn commit(&mut self) {
        publish_history_emote_readiness(&self.channel, true);
        self.committed = true;
    }
}

impl Drop for HistoryEmoteReadinessGuard {
    fn drop(&mut self) {
        if !self.committed {
            publish_history_emote_readiness(&self.channel, false);
        }
    }
}

async fn ensure_channel_emotes_ready_for_history(
    channel_login: &str,
    channel_id_hint: Option<&str>,
) -> bool {
    let channel_key = channel_login.to_lowercase();
    let mut readiness = HistoryEmoteReadinessGuard::new(&channel_key);
    if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
        readiness.commit();
        return true;
    }

    if EMOTE_CACHE.channel_emote_count(&channel_key) > 0 {
        crate::backend::emotes::rebuild_emote_indices(&channel_key).await;
        if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
            readiness.commit();
            return true;
        }
    }

    let channel_id = if let Some(id) = channel_id_hint {
        id.to_string()
    } else if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(&channel_key) {
        id
    } else {
        match crate::backend::twitch_api::get_user_id_cached(&channel_key).await {
            Ok(id) => id,
            Err(error) => {
                crate::log_emotes!(
                    debug,
                    "History emote readiness could not resolve {}: {}",
                    channel_key,
                    error
                );
                return false;
            }
        }
    };
    crate::backend::twitch_api::cache_channel_id(&channel_key, &channel_id);

    let ready = tokio::time::timeout(HISTORY_EMOTE_READY_TIMEOUT, async {
        loop {
            if is_channel_closed(&channel_key) {
                return false;
            }
            if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
                return true;
            }
            if crate::backend::emotes::channel_initial_catalog_pending(&channel_key) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
            let load_id = channel_id.clone();
            let load_key = channel_key.clone();
            let outcome = crate::backend::bridge::RUNTIME
                .spawn(async move {
                    EMOTE_CACHE
                        .load_channel_emotes_reporting(&load_id, &load_key, false)
                        .await
                })
                .await
                .unwrap_or_else(|error| Err(format!("channel emote task failed: {error}")));
            match outcome {
                Ok(crate::backend::emotes::LoadOutcome::Loaded) => {
                    if !crate::backend::emotes::channel_has_lookup_entry(&channel_key) {
                        crate::backend::emotes::rebuild_emote_indices(&channel_key).await;
                    }
                    if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
                        return true;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Ok(crate::backend::emotes::LoadOutcome::Coalesced) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => {
                    crate::log_emotes!(
                        debug,
                        "History emote readiness failed for {}: {}",
                        channel_key,
                        error
                    );
                    return false;
                }
            }
        }
    })
    .await
    .unwrap_or(false);

    if !ready {
        crate::log_emotes!(
            debug,
            "History emote readiness timed out for {} after {}s",
            channel_key,
            HISTORY_EMOTE_READY_TIMEOUT.as_secs()
        );
    } else {
        readiness.commit();
    }
    ready
}

fn publish_history_emote_readiness(channel: &str, ready: bool) {
    let mut loaded = EMOTE_LOADED_CHANNELS.lock();
    if ready {
        if is_channel_closed(channel) {
            return;
        }
        loaded.insert(channel.to_string());
    } else {
        loaded.remove(channel);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SharedChatEmoteLookupSelection {
    pub(crate) channel_key: String,
    pub(crate) unresolved_source_room_id: Option<String>,
}

/// Select the channel catalog used to tokenize one shared-chat row.
///
/// A foreign source whose login is not yet known must not inherit the host
/// channel's emotes. Its empty key intentionally resolves only user/global
/// emotes; the caller can resolve `unresolved_source_room_id` asynchronously
/// for future rows without changing the current row.
pub(crate) fn select_shared_chat_emote_lookup(
    host_channel_login: &str,
    host_channel_id: &str,
    source_broadcaster_login: Option<&str>,
    source_room_id: Option<&str>,
    cached_source_login: impl FnOnce(&str) -> Option<String>,
) -> SharedChatEmoteLookupSelection {
    if let Some(source_login) = source_broadcaster_login {
        return SharedChatEmoteLookupSelection {
            channel_key: source_login.to_lowercase(),
            unresolved_source_room_id: None,
        };
    }

    let Some(source_id) = source_room_id.filter(|source_id| *source_id != host_channel_id) else {
        return SharedChatEmoteLookupSelection {
            channel_key: host_channel_login.to_lowercase(),
            unresolved_source_room_id: None,
        };
    };

    if let Some(source_login) = cached_source_login(source_id) {
        SharedChatEmoteLookupSelection {
            channel_key: source_login.to_lowercase(),
            unresolved_source_room_id: None,
        }
    } else {
        SharedChatEmoteLookupSelection {
            channel_key: String::new(),
            unresolved_source_room_id: Some(source_id.to_string()),
        }
    }
}

/// Process a PRIVMSG (chat message)
async fn process_privmsg_message(msg: twitch_irc::message::PrivmsgMessage, _default_channel: &str) {
    let channel_lower = msg.channel_login.to_lowercase();

    if !DESIRED_CHANNELS.read().contains(&channel_lower) {
        crate::log_irc!(
            debug,
            "Dropping late PRIVMSG for removed channel {}",
            channel_lower
        );
        return;
    }

    if should_hide_blocked_channel_message(
        Some(&msg.sender.id),
        &msg.sender.name,
        &msg.channel_login,
    ) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding message from {} in #{}",
            msg.sender.name,
            msg.channel_login
        );
        return;
    }

    if !admit_received_chat_message(&channel_lower, &msg.message_id) {
        crate::log_irc!(
            debug,
            "Skipping duplicate message {} in {}",
            msg.message_id,
            msg.channel_login
        );
        return;
    }

    {
        let channel_key = channel_lower.clone();
        let should_load_badges = {
            let mut loaded = BADGE_LOADED_CHANNELS.lock();
            if !loaded.contains(&channel_key) {
                loaded.insert(channel_key.clone());
                true
            } else {
                false
            }
        };

        crate::backend::twitch_api::cache_channel_id(&msg.channel_login, &msg.channel_id);

        let should_load_emotes = {
            let mut loaded = EMOTE_LOADED_CHANNELS.lock();
            if !loaded.contains(&channel_key) {
                loaded.insert(channel_key.clone());
                true
            } else {
                false
            }
        };

        if should_load_badges || should_load_emotes {
            let channel_id = msg.channel_id.clone();
            let channel_login = msg.channel_login.clone();
            let load_badges = should_load_badges;
            let load_emotes = should_load_emotes;
            crate::backend::metrics::record_emote_stage(
                "irc.channel_assets.spawn",
                std::time::Duration::ZERO,
                1,
            );
            crate::log_perf!(
                debug,
                stage = "irc.channel_assets.spawn",
                channel = %channel_login,
                load_badges,
                load_emotes,
                "Spawning channel asset load"
            );

            tokio::spawn(async move {
                if load_badges
                    && let Err(e) = TWITCH_BADGE_CACHE
                        .load_channel_badges(&channel_id, &channel_login)
                        .await
                {
                    if e == crate::backend::constants::API_PROXY_NOT_CONFIGURED {
                        crate::log_cache!(
                            debug,
                            "Skipped channel badges for {}: {}",
                            channel_login,
                            e
                        );
                    } else {
                        crate::log_cache!(
                            warn,
                            "Failed to load channel badges for {}: {}",
                            channel_login,
                            e
                        );
                    }
                }
                if load_emotes {
                    load_or_defer_channel_emotes(channel_id, channel_login, "irc.first_message")
                        .await;
                }
            });
        }
    }

    let (mut clean_text, _is_action) = normalize_message_text(&msg.message_text);

    let pending_optimistic_id =
        take_optimistic_echo(&channel_lower, &msg.sender.login, &clean_text);

    let reply_parent_user_login_early = msg.source.tags.0.get("reply-parent-user-login").cloned();
    let reply_parent_msg_id_early = msg.source.tags.0.get("reply-parent-msg-id").cloned();

    let reply_mention_strip_len = if reply_parent_user_login_early.is_some() {
        if clean_text.starts_with('@') {
            if let Some(space_idx) = clean_text[1..].find(' ') {
                let mention_end = 1 + space_idx;
                let prefix_len = mention_end + 1;

                if clean_text.len() > prefix_len {
                    let strip_chars = clean_text[..prefix_len].chars().count();
                    clean_text = clean_text[prefix_len..].to_string();
                    strip_chars
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        }
    } else {
        0
    };

    if let Some(optimistic_id) = pending_optimistic_id {
        if reconcile_live_optimistic_echo(
            &channel_lower,
            optimistic_id.as_ref(),
            &msg,
            &clean_text,
            reply_mention_strip_len,
        ) {
            return;
        }
        crate::log_irc!(
            debug,
            "Optimistic echo matched in #{} but no row to upgrade; inserting real message",
            msg.channel_login
        );
    }

    let username = get_username_lowercase();
    let contains_account_name = contains_account_mention(&clean_text, &username);
    let is_reply_to_current_user = !username.is_empty()
        && reply_parent_user_login_early
            .as_deref()
            .map(|parent_login| parent_login.eq_ignore_ascii_case(&username))
            .unwrap_or(false);
    let is_own_sender =
        !username.is_empty() && msg.sender.login.as_str().eq_ignore_ascii_case(&username);
    let is_reply_to_participated_thread = !is_own_sender
        && !is_reply_to_current_user
        && reply_parent_msg_id_early
            .as_deref()
            .map(|parent_id| {
                reply_thread_has_current_user_participation(&channel_lower, parent_id, &username)
            })
            .unwrap_or(false);
    let highlight_blacklisted_sender =
        crate::backend::settings::is_highlight_blacklisted_user(&msg.sender.login);
    // Your own messages never mention you.
    let mentions_current_user =
        !is_own_sender && (contains_account_name || is_reply_to_current_user);
    let highlighted = is_mention_highlight(highlight_blacklisted_sender, mentions_current_user);

    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .filter_map(|e| {
            if e.char_range.end <= reply_mention_strip_len {
                return None;
            }
            let new_start = e.char_range.start.saturating_sub(reply_mention_strip_len);
            let new_end = e.char_range.end.saturating_sub(reply_mention_strip_len);
            Some((new_start, (e.id.clone(), new_end)))
        })
        .collect();

    let source_room_id = msg.source.tags.0.get("source-room-id").cloned();
    let source_broadcaster_login = msg
        .source
        .tags
        .0
        .get("source-broadcaster-user-login")
        .cloned()
        .map(|s| s.to_lowercase());
    let lookup_selection = select_shared_chat_emote_lookup(
        &msg.channel_login,
        &msg.channel_id,
        source_broadcaster_login.as_deref(),
        source_room_id.as_deref(),
        crate::backend::twitch_api::get_cached_channel_login_by_id,
    );
    if source_broadcaster_login.is_some()
        && let Some(source_id) = source_room_id.as_deref()
    {
        crate::backend::twitch_api::cache_channel_id(&lookup_selection.channel_key, source_id);
    }
    let unresolved_source_room_id = lookup_selection.unresolved_source_room_id.clone();
    if let Some(source_id_owned) = lookup_selection.unresolved_source_room_id {
        crate::log_emotes!(
            debug,
            "[SHARED] Deferring source-room-id {} resolution for host channel {}",
            source_id_owned,
            msg.channel_login
        );
        if begin_source_room_lookup(&source_id_owned, &msg.channel_login, &msg.message_id) {
            tokio::spawn(async move {
                let resolved =
                    crate::backend::twitch_api::get_channel_login_by_id_cached(&source_id_owned)
                        .await;
                let waiters = finish_source_room_lookup(&source_id_owned, resolved.is_some());
                if let Some(resolved) = resolved {
                    crate::backend::twitch_api::cache_channel_id(&resolved, &source_id_owned);
                    for (host_channel, message_id) in waiters {
                        publish_resolved_message_emote_scope(&host_channel, &message_id, &resolved);
                    }
                    ensure_channel_emotes_loaded(&resolved, Some(&source_id_owned)).await;
                }
            });
        }
    }
    let emote_lookup_channel = lookup_selection.channel_key;

    if !emote_lookup_channel.is_empty() && emote_lookup_channel != msg.channel_login {
        let source_emote_channel = emote_lookup_channel.clone();
        let source_room_id = source_room_id.clone();
        tokio::spawn(async move {
            ensure_channel_emotes_loaded(&source_emote_channel, source_room_id.as_deref()).await;
        });
    }

    let (clean_text, twitch_emotes, replaced_emote_tag) =
        match crate::backend::filters_replace::apply_ignore_replacements(
            &clean_text,
            &twitch_emotes,
        ) {
            Some((new_text, new_map)) => {
                let tag = crate::backend::filters_replace::serialize_emote_map(&new_map);
                (new_text, new_map, Some(tag))
            }
            None => (clean_text, twitch_emotes, None),
        };
    let replacement_applied = replaced_emote_tag.is_some();

    let twitch_gifs_tag: Option<Arc<str>> = (!replacement_applied)
        .then(|| {
            msg.source
                .tags
                .0
                .get("gifs")
                .map(|tag| Arc::from(tag.as_str()))
        })
        .flatten();
    let slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        &clean_text,
        &emote_lookup_channel,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
        twitch_gifs_tag.as_deref(),
        reply_mention_strip_len,
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let is_action = msg.message_text.starts_with('\x01') && msg.message_text.contains("ACTION");
    let content = clean_text;

    let sender_login = msg.sender.login.clone();
    let display_name = msg.sender.name.clone();

    let color: Option<Arc<str>> = msg.name_color.map(|c| format_color_rgb(c.r, c.g, c.b));

    let is_first_msg = msg
        .source
        .tags
        .0
        .get("first-msg")
        .map(|v| v.as_str() == "1")
        .unwrap_or(false);
    let is_returning_chatter = msg
        .source
        .tags
        .0
        .get("returning-chatter")
        .map(|v| v.as_str() == "1")
        .unwrap_or(false);
    let is_elevated_message = msg
        .source
        .tags
        .0
        .get("elevated-msg")
        .or_else(|| msg.source.tags.0.get("elevated-message"))
        .or_else(|| msg.source.tags.0.get("elevated"))
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE"))
        .unwrap_or(false);
    let is_hype_chat = msg
        .source
        .tags
        .0
        .keys()
        .any(|key| key.starts_with("pinned-chat-paid-"));

    let is_shared_chat = msg
        .source
        .tags
        .0
        .get("source-room-id")
        .cloned()
        .map(|source_id| source_id != msg.channel_id)
        .unwrap_or(false);

    let reply_parent_msg_id = msg.source.tags.0.get("reply-parent-msg-id").cloned();
    let reply_parent_user_login = msg.source.tags.0.get("reply-parent-user-login").cloned();
    let reply_parent_display_name = msg.source.tags.0.get("reply-parent-display-name").cloned();
    let reply_parent_user_id = msg.source.tags.0.get("reply-parent-user-id").cloned();
    let reply_parent_msg_body = msg.source.tags.0.get("reply-parent-msg-body").cloned();
    let is_reply = reply_parent_msg_id.is_some();

    let sub_length: Option<u32> = msg
        .badge_info
        .iter()
        .find(|b| b.name == "subscriber")
        .and_then(|b| b.version.parse().ok());
    let bits_amount = msg
        .source
        .tags
        .0
        .get("bits")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let is_cheer = bits_amount > 0;
    let badge_keys_for_highlights: Vec<String> = msg
        .badges
        .iter()
        .map(|badge| format_badge(&badge.name, &badge.version).to_string())
        .collect();
    let reward_id = msg
        .source
        .tags
        .0
        .get("custom-reward-id")
        .cloned()
        .unwrap_or_default();
    let is_reward_message = !reward_id.is_empty();

    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    let similarity_settings = get_similarity_settings();
    let is_similar = CHAT_MESSAGES
        .get(&msg.channel_login)
        .and_then(|buffer| {
            detect_similarity(
                &similarity_settings,
                &content,
                &sender_login,
                timestamp,
                buffer
                    .iter_recent(similarity_settings.max_messages_to_check as usize)
                    .map(|msg| SimilarityCandidate {
                        content: &msg.content,
                        sender: &msg.sender,
                        timestamp: msg.timestamp,
                    }),
            )
        })
        .is_some();

    let filter_ctx = TestContext {
        content: content.clone(),
        author: sender_login.clone(),
        author_login: sender_login.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.as_ref().map(|c| c.to_string()),
        badges: badge_names_for_filter(msg.badges.iter().map(|b| b.name.as_str())),
        channel_name: msg.channel_login.clone(),
        bits_amount,
        reward_id: reward_id.clone(),
        sub_length,
        is_sub: msg.badges.iter().any(|b| b.name == "subscriber"),
        is_first_msg,
        is_returning_chatter,
        is_action,
        is_reply,
        is_whisper: false,
        is_announcement: false,
        is_system: false,
        is_highlighted: highlighted,
        is_cheer,
        is_shared_chat,
        is_similar,
        is_points_redeemed: is_reward_message,
        is_reward_message,
        is_elevated_message,
        is_hype_chat,
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(&msg.channel_login),
        ..Default::default()
    };

    let exclude_self_from_filters =
        crate::backend::settings::get_exclude_user_messages_from_filter()
            && sender_login.eq_ignore_ascii_case(&get_username_lowercase());
    let filter_results = if exclude_self_from_filters {
        Vec::new()
    } else {
        FILTER_STORE.test_message(&filter_ctx)
    };

    let PrivmsgFilterOutcome {
        hide: should_hide,
        highlighted: filter_highlighted,
        matched_text: filter_highlight_match,
    } = fold_privmsg_filter_results(&filter_results);

    if !should_hide && is_similar && similarity_settings.hide_similar {
        crate::log_irc!(debug, "Similarity: Hiding message from {}", sender_login);
        return;
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding message from {}", sender_login);
        return;
    }

    let matched_configured_highlight = if highlight_blacklisted_sender {
        None
    } else {
        crate::backend::settings::match_highlight_rule_for_targets(
            &content,
            &sender_login,
            &badge_keys_for_highlights,
            &[
                crate::backend::settings::HighlightRuleTarget::Phrase,
                crate::backend::settings::HighlightRuleTarget::User,
            ],
        )
    };
    let matched_configured_badge_highlight = if highlight_blacklisted_sender {
        None
    } else {
        crate::backend::settings::match_highlight_rule_for_targets(
            &content,
            &sender_login,
            &badge_keys_for_highlights,
            &[crate::backend::settings::HighlightRuleTarget::Badge],
        )
    };

    let mut highlight_decision = decide_privmsg_highlight(
        PrivmsgHighlightSignals {
            blacklisted_sender: highlight_blacklisted_sender,
            account_name_mention: mentions_current_user,
            own_sender: is_own_sender,
            participated_thread_username: is_reply_to_participated_thread
                .then_some(username.as_str()),
            filter_matched_text: filter_highlight_match,
            filter_highlighted,
            cheer: is_cheer,
            first_message: is_first_msg,
            returning_chatter: is_returning_chatter,
            elevated: is_elevated_message || is_hype_chat,
            similar_dim_candidate: is_similar && similarity_settings.color_similar,
        },
        matched_configured_highlight.as_ref(),
        matched_configured_badge_highlight.as_ref(),
        || {
            crate::backend::settings::match_highlight_rule_effects_for_targets(
                &content,
                &sender_login,
                &badge_keys_for_highlights,
                &[
                    crate::backend::settings::HighlightRuleTarget::Phrase,
                    crate::backend::settings::HighlightRuleTarget::User,
                    crate::backend::settings::HighlightRuleTarget::Badge,
                ],
            )
        },
    );
    let highlight_type = highlight_decision.highlight_type.take();
    let is_similar_dim = highlight_decision.is_similar_dim;

    let reply = match (reply_parent_msg_id, reply_parent_user_login) {
        (Some(parent_id), Some(parent_user)) => Some(ReplyInfo {
            parent_msg_id: Arc::from(parent_id),
            parent_user_login: Arc::from(parent_user.clone()),
            parent_display_name: Arc::from(reply_parent_display_name.unwrap_or(parent_user)),
            parent_user_id: reply_parent_user_id.map(Arc::from),
            parent_msg_body: reply_parent_msg_body.map(Arc::from),
        }),
        _ => None,
    };

    let twitch_emotes_tag = msg.source.tags.0.get("emotes").cloned();

    let mut chat_msg = ChatMessage {
        id: Arc::from(msg.message_id.as_str()),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: (emote_lookup_channel != msg.channel_login)
            .then(|| Arc::from(emote_lookup_channel.as_str())),
        emote_lookup_source_id: unresolved_source_room_id.map(Arc::from),
        sender: Arc::from(sender_login.as_str()),
        display_name: Arc::from(display_name.as_str()),
        content: content.into(),
        color: color.clone(),
        badges: msg
            .badges
            .iter()
            .map(|b| format_badge(&b.name, &b.version))
            .collect(),
        timestamp,
        slots,
        structure_hash,
        reply,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type,
            is_system: false,
            is_usernotice: false,
            is_first_message: is_first_msg,
            system_type: None,
            is_shared_chat,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message,
            is_hype_chat,
            is_automod: false,
            is_automod_pending: false,
            is_suspicious_user: false,
            is_restricted: false,
            is_monitored: false,
            is_similar: is_similar_dim,
            is_alternate_row: false,
        },
        twitch_gifs_tag,
        twitch_emotes_tag: match replaced_emote_tag {
            Some(tag) => tag.map(|t| Arc::from(t.as_str())),
            None => twitch_emotes_tag.map(|t| Arc::from(t.as_str())),
        },
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: if replacement_applied {
            0
        } else {
            reply_mention_strip_len
        },
    };

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if !DESIRED_CHANNELS.read().contains(&channel_lower) {
        crate::log_irc!(
            debug,
            "Dropping late PRIVMSG after final visibility fence for removed channel {}",
            channel_lower
        );
        return;
    }
    let blocked_visibility = blocked_channel_visibility_snapshot(&msg.channel_login);
    if blocked_visibility.should_hide_sender(Some(&msg.sender.id), &sender_login) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding message from {} after final visibility recheck in #{}",
            sender_login,
            msg.channel_login
        );
        return;
    }
    if chat_msg.reply.as_ref().is_some_and(|reply| {
        blocked_visibility.should_redact_reply_parent(reply.parent_user_login.as_ref())
    }) {
        chat_msg.reply = None;
    }

    let initial_visibility_pending = crate::backend::blocked_users::initial_visibility_pending();
    if !initial_visibility_pending {
        crate::log_irc!(debug, "Received: {}: {}", msg.sender.name, msg.message_text);
    } else if !pending_channel_row_capacity_available_commit_held() {
        drop(visibility_commit);
        return;
    }

    let replaced_confirmed_local_row = {
        let mut buffer = CHAT_MESSAGES
            .entry(msg.channel_login.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        store_received_chat_row(&mut buffer, &mut chat_msg)
    };
    apply_pending_message_emote_scope(&msg.channel_login, chat_msg.id.as_ref());
    if replaced_confirmed_local_row {
        let affected_ids = std::iter::once(chat_msg.id.clone()).collect::<HashSet<_>>();
        crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
            &affected_ids,
        );
        bump_chat_version();
        crate::backend::history_cache::record_live_chat_message(&msg.channel_login, &chat_msg);
        crate::backend::observers::get_observer_registry().notify(
            &msg.channel_login,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
        drop(visibility_commit);
        return;
    }
    if initial_visibility_pending {
        defer_buffered_row_visibility_commit_held(&msg.channel_login, chat_msg.id.as_ref());
        drop(visibility_commit);
        return;
    }

    crate::benchmark_profile::note_live_inbound_privmsg(&msg.channel_login);

    if let Some(ref color_str) = color {
        cache_user_color(&sender_login, color_str);
    }
    track_chatter(
        &msg.channel_login,
        &sender_login,
        &display_name,
        color.as_ref().map(|color| color.to_string()),
    );

    bump_chat_version();
    crate::backend::history_cache::record_live_chat_message(&msg.channel_login, &chat_msg);

    let is_mention_activity = privmsg_shows_in_mentions(
        highlighted,
        chat_msg.flags.highlight_type.as_ref(),
        matched_configured_highlight.as_ref(),
        matched_configured_badge_highlight.as_ref(),
    );

    let mut tab_activity = TAB_ACTIVITY_MESSAGE;
    if is_mention_activity {
        tab_activity |= TAB_ACTIVITY_MENTION;
    }
    mark_channel_tab_activity(&msg.channel_login, tab_activity);

    if is_mention_activity {
        let evicted = {
            let mut mentions = GLOBAL_MENTIONS.write();
            let evicted = (mentions.len() >= MAX_ACTIVITY_MENTIONS)
                .then(|| mentions.pop_front())
                .flatten();
            mentions.push_back((msg.channel_login.clone(), chat_msg.clone()));
            evicted
        };
        if let Some((evicted_channel, evicted_message)) = evicted {
            remove_global_mention_unread(&evicted_channel, evicted_message.id.as_ref());
        }
        mark_global_mention_unread(&msg.channel_login, chat_msg.id.as_ref());
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }

    let slots_for_preload = Arc::clone(&chat_msg.slots);
    let msg_id_for_preload = Arc::clone(&chat_msg.id);
    let preload_emote_channel = emote_lookup_channel.clone();

    crate::backend::observers::get_observer_registry().notify(
        &msg.channel_login,
        crate::backend::observers::ChannelEvent::NewMessage(chat_msg.clone()),
    );
    drop(visibility_commit);

    match highlight_decision.sound() {
        Some(PrivmsgHighlightSound::Event(sound)) => {
            SOUND_STORE.play_sound_for_channel(sound, &msg.channel_login);
        }
        Some(PrivmsgHighlightSound::RuleFile(sound_path)) => {
            SOUND_STORE.play_custom_sound_for_channel(
                sound_path,
                SoundEventType::Phrase,
                &msg.channel_login,
            );
        }
        None => {}
    }
    match highlight_decision.alert() {
        PrivmsgHighlightAlert::Mention => {
            if crate::backend::settings::get_flash_on_mention() {
                crate::backend::notifications::flash_taskbar_for_attention();
            }
            notify_mention(MentionNotification {
                channel: msg.channel_login.clone(),
                author: chat_msg.display_name.to_string(),
                message: chat_msg.content.to_string(),
            });
        }
        PrivmsgHighlightAlert::Rule => {
            crate::backend::notifications::flash_taskbar_for_attention();
            notify_highlight(
                HighlightNotification {
                    channel: msg.channel_login.clone(),
                    author: chat_msg.display_name.to_string(),
                    message: chat_msg.content.to_string(),
                },
                SoundEventType::Phrase,
            );
        }
        PrivmsgHighlightAlert::None => {}
    }

    preload_message_content(
        &slots_for_preload,
        &preload_emote_channel,
        msg_id_for_preload.as_ref(),
    );
}

/// Process a whisper message
async fn process_whisper_message(msg: twitch_irc::message::WhisperMessage) {
    if crate::backend::blocked_users::should_hide_whisper(Some(&msg.sender.id), &msg.sender.name) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding whisper from {}",
            msg.sender.name
        );
        return;
    }

    let (clean_text, is_action) = normalize_message_text(&msg.message_text);
    let color = msg.name_color.map(|c| format_color_rgb(c.r, c.g, c.b));

    let filter_ctx = TestContext {
        content: clean_text.clone(),
        author: msg.sender.name.clone(),
        author_login: msg.sender.name.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.as_ref().map(|c| c.to_string()),
        badges: badge_names_for_filter(msg.badges.iter().map(|b| b.name.as_str())),
        is_action,
        is_whisper: true,
        is_highlighted: true,
        channel_watching: true,
        ..Default::default()
    };

    let mut should_hide = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Allow => {
                should_hide = false;
                break;
            }
            FilterAction::Highlight => {}
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding whisper from {}", msg.sender.name);
        return;
    }

    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .map(|e| (e.char_range.start, (e.id.clone(), e.char_range.end)))
        .collect();

    let (clean_text, twitch_emotes, replaced_emote_tag) =
        match crate::backend::filters_replace::apply_ignore_replacements(
            &clean_text,
            &twitch_emotes,
        ) {
            Some((new_text, new_map)) => {
                let tag = crate::backend::filters_replace::serialize_emote_map(&new_map);
                (new_text, new_map, Some(tag))
            }
            None => (clean_text, twitch_emotes, None),
        };

    let slots: Vec<_> = parse_to_slots(
        &clean_text,
        crate::backend::whispers::WHISPERS_CHANNEL,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let twitch_emotes_tag = msg.source.tags.0.get("emotes").cloned();

    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(|| Utc::now().timestamp_millis());

    let chat_msg = ChatMessage {
        id: Uuid::new_v4().to_string().into(),
        channel: Arc::from(crate::backend::whispers::WHISPERS_CHANNEL),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.name.as_str()),
        display_name: Arc::from(msg.sender.name.as_str()),
        content: clean_text.into(),
        color,
        badges: msg
            .badges
            .iter()
            .map(|b| format_badge(&b.name, &b.version))
            .collect(),
        timestamp,
        slots,
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type: Some(HighlightType::Whisper),
            is_system: false,
            is_usernotice: false,
            is_first_message: false,
            system_type: None,
            is_shared_chat: false,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message: false,
            is_hype_chat: false,
            is_automod: false,
            is_automod_pending: false,
            is_suspicious_user: false,
            is_restricted: false,
            is_monitored: false,
            is_similar: false,
            is_alternate_row: false,
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: match replaced_emote_tag {
            Some(tag) => tag.map(|t| Arc::from(t.as_str())),
            None => twitch_emotes_tag.map(|t| Arc::from(t.as_str())),
        },
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: 0,
    };

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::initial_visibility_pending() {
        let dropped_oldest = crate::backend::blocked_users::defer_whisper_visibility_commit_held(
            &msg.sender.id,
            &msg.sender.name,
            chat_msg.clone(),
        );
        crate::log_irc!(
            debug,
            "Blocked users: Deferred an incoming whisper until initial visibility becomes authoritative"
        );
        if dropped_oldest {
            crate::log_irc!(
                warn,
                "Blocked users: Pending whisper queue reached its bounded capacity; dropped the oldest deferred whisper"
            );
        }
        return;
    }
    if crate::backend::blocked_users::should_hide_whisper(Some(&msg.sender.id), &msg.sender.name) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding whisper from {} after final visibility recheck",
            msg.sender.name
        );
        return;
    }
    crate::log_irc!(
        debug,
        "Whisper from {}: {}",
        msg.sender.name,
        msg.message_text
    );
    let publication =
        crate::backend::whispers::record_received_visibility_commit_held(chat_msg.clone());
    drop(visibility_commit);
    crate::backend::whispers::dispatch_publication(publication);
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
    SOUND_STORE.play_sound(SoundEventType::Whisper);
    notify_whisper(&chat_msg.display_name, &chat_msg.content);
}

fn note_irc_send_rejection(session_login: &str, channel: &str, message_id: &str, text: &str) {
    if !matches!(message_id, "msg_slowmode" | "msg_banned" | "msg_timedout") {
        return;
    }
    if !crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(session_login) {
        return;
    }
    let Some(auth) = crate::backend::auth::capture_authenticated_user_resource() else {
        return;
    };
    let account_id = auth.user_id.clone();
    let session_login = session_login.to_string();
    let channel = channel.to_string();
    let message_id = message_id.to_ascii_lowercase();
    let text = text.to_string();
    let _ = auth.commit_if_current(|| {
        if !crate::backend::settings::get_username_lowercase().eq_ignore_ascii_case(&session_login)
        {
            return;
        }
        crate::backend::irc::room_state::note_send_rejection(
            &account_id,
            &channel,
            &message_id,
            &text,
        );
    });
}

/// Process a USERSTATE message
fn process_userstate_message(msg: twitch_irc::message::UserStateMessage, session_login: &str) {
    let color = msg
        .name_color
        .map(|c| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b));
    let badges: Vec<String> = msg
        .badges
        .iter()
        .map(|b| format!("{}:{}", b.name, b.version))
        .collect();
    crate::log_irc!(
        debug,
        "USERSTATE for #{}: color={:?}, badges={:?}",
        msg.channel_login,
        color,
        badges
    );
    let slow_mode_exempt = msg
        .source
        .tags
        .0
        .get("mod")
        .is_some_and(|mod_flag| mod_flag == "1")
        || session_login.eq_ignore_ascii_case(&msg.channel_login)
        || badges.iter().any(|badge| {
            crate::backend::badges::is_moderator_badge_key(badge)
                || crate::backend::badges::badge_key_matches(badge, "broadcaster")
                || crate::backend::badges::badge_key_matches(badge, "vip")
        });
    crate::backend::irc::room_state::note_self_roles(
        session_login,
        &msg.channel_login,
        slow_mode_exempt,
    );
    if cache_self_state(&msg.channel_login, color, badges) {
        crate::backend::eventsub::refresh_channel_authorization(&msg.channel_login);
    }
}

pub(crate) fn badge_names_for_filter<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut badges: Vec<String> = Vec::new();
    let mut has_moderator = false;
    let mut has_lead_moderator = false;

    for name in names {
        if name.is_empty() {
            continue;
        }
        if crate::backend::badges::badge_name_matches(name, "moderator") {
            has_moderator = true;
        }
        if crate::backend::badges::is_lead_moderator_badge_name(name) {
            has_lead_moderator = true;
        }
        if !badges
            .iter()
            .any(|badge| crate::backend::badges::badge_name_matches(badge.as_str(), name))
        {
            badges.push(name.to_string());
        }
    }

    if has_lead_moderator && !has_moderator {
        badges.push("moderator".to_string());
    }

    badges
}

/// Process a CLEARCHAT message (ban/timeout/clear)
fn process_clearchat_message(msg: twitch_irc::message::ClearChatMessage) {
    use twitch_irc::message::ClearChatAction;
    let (text, streamer_text, target_user, deleted_sender) = match &msg.action {
        ClearChatAction::UserBanned { user_login, .. } => (
            format!("{} was permanently banned", user_login),
            "A user was permanently banned".to_string(),
            user_login.clone(),
            Some(user_login.as_str()),
        ),
        ClearChatAction::UserTimedOut {
            user_login,
            timeout_length,
            ..
        } => (
            format!(
                "{} timed out for {} seconds",
                user_login,
                timeout_length.as_secs()
            ),
            "A user was timed out".to_string(),
            user_login.clone(),
            Some(user_login.as_str()),
        ),
        ClearChatAction::ChatCleared => (
            "Chat was cleared by a moderator".to_string(),
            "Chat was cleared".to_string(),
            "moderator".to_string(),
            None,
        ),
    };
    let display_text = moderation_system_message_text(&text, &streamer_text);

    crate::log_irc!(info, "Mod: {}: {}", msg.channel_login, text);
    if let ClearChatAction::UserTimedOut {
        user_login,
        timeout_length,
        ..
    } = &msg.action
    {
        add_timeout_system_message(
            &msg.channel_login,
            user_login,
            timeout_length.as_secs(),
            &streamer_text,
        );
    } else {
        add_moderation_system_message(
            &msg.channel_login,
            &display_text,
            &target_user,
            moderation_action_filter_label(&msg.action),
            "",
            "",
        );
    }

    if let Some(target_user_id) = msg.source.tags.0.get("target-user-id") {
        match &msg.action {
            ClearChatAction::UserBanned { .. } => {
                crate::backend::irc::room_state::note_self_moderation(
                    target_user_id,
                    &msg.channel_login,
                    None,
                );
            }
            ClearChatAction::UserTimedOut { timeout_length, .. } => {
                let sent_at = msg
                    .source
                    .tags
                    .0
                    .get("tmi-sent-ts")
                    .and_then(|timestamp| timestamp.parse::<i64>().ok())
                    .unwrap_or_else(|| Utc::now().timestamp_millis());
                let elapsed_ms =
                    Utc::now().timestamp_millis().saturating_sub(sent_at).max(0) as u64;
                let remaining =
                    timeout_length.saturating_sub(std::time::Duration::from_millis(elapsed_ms));
                crate::backend::irc::room_state::note_self_moderation(
                    target_user_id,
                    &msg.channel_login,
                    Some(remaining),
                );
            }
            ClearChatAction::ChatCleared => {}
        }
    }

    let msg_id = Uuid::new_v4().to_string();
    let timestamp = Utc::now().timestamp_millis();
    if let Some(action) = parse_mod_action(
        &msg_id,
        &display_text,
        &target_user,
        &msg.channel_login,
        timestamp,
    ) {
        add_mod_action(action);
    }

    apply_deleted_message_display_for_sender(&msg.channel_login, deleted_sender);
}

/// Feed a raw CLEARMSG or CLEARCHAT line through the live moderation path.
/// Only the chat replay fixture calls this, so a run without an account can
/// exercise deletions, timeouts, bans and chat clears. Returns whether the
/// line was one of those commands.
pub(crate) fn replay_raw_moderation_line(raw: &str) -> bool {
    let Ok(irc) = twitch_irc::message::IRCMessage::parse(raw) else {
        return false;
    };
    match ServerMessage::try_from(irc) {
        Ok(ServerMessage::ClearMsg(msg)) => {
            process_clearmsg_message(msg);
            true
        }
        Ok(ServerMessage::ClearChat(msg)) => {
            process_clearchat_message(msg);
            true
        }
        _ => false,
    }
}

/// Process a CLEARMSG message (single message deletion)
fn process_clearmsg_message(msg: twitch_irc::message::ClearMsgMessage) {
    let original_message = find_buffered_message_by_id(&msg.channel_login, &msg.message_id);
    let text = format!("Message from {} was deleted", msg.sender_login);
    // The notice carries the removed text so it stays readable
    // in a split that hides deleted rows. The log line never includes it.
    let notice = match original_message
        .as_ref()
        .map(|original| deleted_text_preview(&original.content))
    {
        Some(preview) if !preview.is_empty() => format!("{text}: {preview}"),
        _ => text.clone(),
    };
    let display_text = moderation_system_message_text(&notice, "A message was deleted");
    crate::log_irc!(
        info,
        "Mod: {}: {} (id: {})",
        msg.channel_login,
        text,
        msg.message_id
    );
    add_moderation_system_message(
        &msg.channel_login,
        &display_text,
        &msg.sender_login,
        "delete",
        "",
        "",
    );

    let timestamp = Utc::now().timestamp_millis();
    if let Some(action) = parse_mod_action(
        &msg.message_id,
        &display_text,
        &msg.sender_login,
        &msg.channel_login,
        timestamp,
    ) {
        add_mod_action(action);
    }

    crate::backend::history_cache::record_readable_deleted_message(
        &msg.channel_login,
        &msg.message_id,
        &msg.sender_login,
        original_message.as_ref(),
    );

    apply_deleted_message_display(&msg.channel_login, &msg.message_id);
}

/// Longest removed-text preview a deletion notice carries.
const DELETED_PREVIEW_LIMIT: usize = 100;

fn deleted_text_preview(content: &str) -> String {
    let mut chars = content.trim().chars();
    let mut preview: String = chars.by_ref().take(DELETED_PREVIEW_LIMIT).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

fn find_buffered_message_by_id(channel: &str, message_id: &str) -> Option<ChatMessage> {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() || message_id.trim().is_empty() {
        return None;
    }

    CHAT_MESSAGES.get(&channel_key).and_then(|buffer| {
        buffer
            .iter()
            .find(|msg| msg.id.as_ref() == message_id)
            .cloned()
    })
}

fn moderation_system_message_text(text: &str, streamer_text: &str) -> String {
    if crate::backend::streamer_mode::should_hide_mod_actions() {
        streamer_text.to_string()
    } else {
        text.to_string()
    }
}

fn moderation_action_filter_label(action: &twitch_irc::message::ClearChatAction) -> &'static str {
    match action {
        twitch_irc::message::ClearChatAction::UserBanned { .. } => "ban",
        twitch_irc::message::ClearChatAction::UserTimedOut { .. } => "timeout",
        twitch_irc::message::ClearChatAction::ChatCleared => "clear",
    }
}

/// Compact duration: "10m", "1d", "1h 30m".
fn format_timeout_duration(total_seconds: u64) -> String {
    const UNITS: [(u64, &str); 4] = [(86_400, "d"), (3_600, "h"), (60, "m"), (1, "s")];
    let mut remaining = total_seconds;
    let mut parts = Vec::new();
    for (unit_seconds, suffix) in UNITS {
        let count = remaining / unit_seconds;
        if count > 0 {
            parts.push(format!("{count}{suffix}"));
            remaining %= unit_seconds;
        }
    }
    if parts.is_empty() {
        "0s".to_string()
    } else {
        parts.join(" ")
    }
}

fn timeout_row_text(user_login: &str, seconds: u64, streamer_text: &str, count: u32) -> String {
    let text = format!(
        "{} timed out for {}",
        user_login,
        format_timeout_duration(seconds)
    );
    let text = moderation_system_message_text(&text, streamer_text);
    if count > 1 {
        format!("{text} ({count} times)")
    } else {
        text
    }
}

/// Repeated timeouts of one user within this window and row distance merge
/// into one row.
const TIMEOUT_STACK_WINDOW_MS: i64 = 5_000;
const TIMEOUT_STACK_RECENT_ROWS: usize = 20;

struct RecentTimeoutRow {
    row_id: String,
    count: u32,
    at_ms: i64,
}

static RECENT_TIMEOUT_ROWS: LazyLock<Mutex<HashMap<(String, String), RecentTimeoutRow>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn add_timeout_system_message(channel: &str, user_login: &str, seconds: u64, streamer_text: &str) {
    let channel_key = channel.to_lowercase();
    let now_ms = Utc::now().timestamp_millis();
    let key = (channel_key.clone(), user_login.to_lowercase());

    let mut recent = RECENT_TIMEOUT_ROWS.lock();
    recent.retain(|_, row| now_ms.saturating_sub(row.at_ms) <= TIMEOUT_STACK_WINDOW_MS);
    if let Some(row) = recent.get_mut(&key) {
        let count = row.count.saturating_add(1);
        let text = timeout_row_text(user_login, seconds, streamer_text, count);
        let verdict = moderation_row_filter_verdict(channel, &text, user_login, "timeout", "", "");
        let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
        let updated = CHAT_MESSAGES
            .get_mut(&channel_key)
            .is_some_and(|mut buffer| {
                if !buffer
                    .iter_recent(TIMEOUT_STACK_RECENT_ROWS)
                    .any(|message| message.id.as_ref() == row.row_id)
                {
                    return false;
                }
                match &verdict {
                    None => buffer.remove_by_id(&row.row_id),
                    Some(highlight_type) => buffer.update_by_id(&row.row_id, |message| {
                        let slots = vec![MessageSlot::text(text.as_str())];
                        message.structure_hash = compute_structure_hash(&slots);
                        message.slots = slots.into();
                        message.content = Arc::from(text.as_str());
                        message.flags.highlight_type = highlight_type.clone();
                    }),
                }
            });
        drop(visibility_commit);
        if updated {
            row.count = count;
            row.at_ms = now_ms;
            let row_id: Arc<str> = Arc::from(row.row_id.as_str());
            if verdict.is_none() {
                recent.remove(&key);
            }
            drop(recent);
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
                &HashSet::from([row_id]),
            );
            crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
            bump_chat_version();
            crate::backend::observers::get_observer_registry().notify(
                &channel_key,
                crate::backend::observers::ChannelEvent::MessagesRetokenized,
            );
            return;
        }
    }

    let row_id = Uuid::new_v4().to_string();
    recent.insert(
        key,
        RecentTimeoutRow {
            row_id: row_id.clone(),
            count: 1,
            at_ms: now_ms,
        },
    );
    drop(recent);
    add_moderation_system_message_with_id(
        channel,
        &timeout_row_text(user_login, seconds, streamer_text, 1),
        user_login,
        "timeout",
        "",
        "",
        Some(row_id),
    );
}

fn add_moderation_system_message(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
) {
    add_moderation_system_message_with_id(
        channel,
        text,
        author_login,
        event_action,
        moderator_login,
        event_reason,
        None,
    );
}

fn add_moderation_system_message_with_id(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
    row_id: Option<String>,
) {
    let Some(highlight_type) = moderation_row_filter_verdict(
        channel,
        text,
        author_login,
        event_action,
        moderator_login,
        event_reason,
    ) else {
        crate::log_irc!(debug, "Filter: Hiding moderation system row in {}", channel);
        return;
    };
    add_system_message_with_highlight_flags_id(
        channel,
        text,
        SystemMessageType::Info,
        highlight_type,
        SystemMessageFlagOverrides::default(),
        row_id,
    );
}

/// Run the channel filters over a moderation row: `None` hides it, otherwise
/// the highlight it should carry.
fn moderation_row_filter_verdict(
    channel: &str,
    text: &str,
    author_login: &str,
    event_action: &str,
    moderator_login: &str,
    event_reason: &str,
) -> Option<Option<HighlightType>> {
    let filter_ctx = TestContext {
        content: text.to_string(),
        author: author_login.to_string(),
        author_login: author_login.to_lowercase(),
        moderator_name: moderator_login.to_string(),
        moderator_login: moderator_login.to_lowercase(),
        target_name: author_login.to_string(),
        target_login: author_login.to_lowercase(),
        channel_name: channel.to_string(),
        event_action: event_action.to_string(),
        event_reason: event_reason.to_string(),
        event_source: "irc".to_string(),
        is_system: true,
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(channel),
        ..Default::default()
    };

    let mut should_hide = false;
    let mut filter_highlight_match: Option<String> = None;
    let mut filter_highlighted = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Highlight => {
                filter_highlighted = true;
                if filter_highlight_match.is_none() {
                    filter_highlight_match =
                        result.matched_text.filter(|text| !text.trim().is_empty());
                }
            }
            FilterAction::Allow => {
                should_hide = false;
                filter_highlighted = false;
                filter_highlight_match = None;
                break;
            }
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        return None;
    }

    Some(
        filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage)),
    )
}

/// Mark one row deleted. The row keeps its original text: whether it fades,
/// collapses or disappears is each split's choice, so a split can reveal it later.
fn apply_deleted_message_display(channel: &str, message_id: &str) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let affected_ids: HashSet<String> = std::iter::once(message_id.to_string()).collect();

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let changed = CHAT_MESSAGES
        .get_mut(&channel_key)
        .map(|mut buffer| {
            buffer.update_by_id(message_id, |msg| {
                msg.flags.is_deleted = true;
            })
        })
        .unwrap_or(false);
    drop(visibility_commit);

    if changed {
        crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
            &affected_ids,
        );
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            &channel_key,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}

/// Mark every ordinary row authored by `sender` deleted. A sender-less
/// CLEARCHAT is Twitch's full chat-clear event and therefore targets every
/// ordinary chat row while preserving the moderation notice and other system
/// rows. Rows keep their text; each split decides how to present them.
fn apply_deleted_message_display_for_sender(channel: &str, sender: Option<&str>) {
    use crate::state::MessageReconcileAction;

    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let mut mutated_ids = HashSet::new();
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let changed = CHAT_MESSAGES
        .get_mut(&channel_key)
        .map(|mut buffer| {
            let result = buffer.reconcile(|message| {
                let is_target = !message.flags.is_system
                    && sender.is_none_or(|login| message.sender.eq_ignore_ascii_case(login));
                if !is_target || message.flags.is_deleted {
                    return MessageReconcileAction::Keep;
                }

                message.flags.is_deleted = true;
                if sender.is_some() && !message.id.is_empty() {
                    mutated_ids.insert(message.id.to_string());
                }
                MessageReconcileAction::Mutated
            });
            result.removed != 0 || result.mutated != 0
        })
        .unwrap_or(false);
    drop(visibility_commit);

    if changed {
        if sender.is_none() {
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_channel(
                &channel_key,
            );
        } else {
            crate::components::chat::canvas_renderer::global_layout_cache::clear_layouts_for_messages(
                &mutated_ids,
            );
        }
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            &channel_key,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}

/// Process a USERNOTICE message (sub/raid/etc)
fn process_usernotice_message(msg: twitch_irc::message::UserNoticeMessage) {
    if should_hide_blocked_channel_message(
        Some(&msg.sender.id),
        &msg.sender.login,
        &msg.channel_login,
    ) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding USERNOTICE from {} in #{}",
            msg.sender.login,
            msg.channel_login
        );
        return;
    }

    let msg_id = msg.source.tags.0.get("msg-id").cloned().unwrap_or_default();

    let tag_value = |key: &str| msg.source.tags.0.get(key).cloned();
    let tags = UsernoticeTags {
        msg_id: msg_id.clone(),
        ritual_name: tag_value("ritual-name"),
        watch_streak_months: tag_value("watch-streak-months").and_then(|v| v.parse().ok()),
        watch_streak: tag_value("watch-streak"),
        milestone_category: tag_value("msg-param-category"),
        milestone_value: tag_value("msg-param-value").and_then(|value| value.parse().ok()),
        modiversary_months: tag_value("msg-param-months").and_then(|value| value.parse().ok()),
    };
    let mut highlight_type = usernotice_core::highlight_kind(&tags).map(usernotice_highlight_type);
    if matches!(highlight_type, Some(HighlightType::Announcement { .. })) {
        let color = tag_value("msg-param-color").filter(|value| !value.trim().is_empty());
        highlight_type = Some(HighlightType::Announcement { color });
    }
    let event_type = usernotice_core::event_type_label(&msg_id);
    let authored_text = msg
        .message_text
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    let system_text = if msg_id == "announcement" && msg.system_message.trim().is_empty() {
        Some("Announcement")
    } else {
        (!msg.system_message.trim().is_empty()).then_some(msg.system_message.as_str())
    };
    if authored_text.is_none() && system_text.is_none() {
        return;
    }

    let filter_text = match (system_text, authored_text) {
        (Some(system), Some(content)) => format!("{system} {content}"),
        (Some(system), None) => system.to_string(),
        (None, Some(content)) => content.to_string(),
        (None, None) => unreachable!("empty USERNOTICE returned above"),
    };
    let sub_length = msg
        .badge_info
        .iter()
        .find(|badge| badge.name == "subscriber")
        .and_then(|badge| badge.version.parse().ok());
    let bits_amount = tag_value("msg-param-threshold")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let color = msg
        .name_color
        .map(|color| format_color_rgb(color.r, color.g, color.b).to_string());
    let filter_ctx = TestContext {
        content: filter_text.clone(),
        author: msg.sender.login.clone(),
        author_login: msg.sender.login.clone(),
        author_user_id: msg.sender.id.clone(),
        author_color: color.clone(),
        badges: badge_names_for_filter(msg.badges.iter().map(|badge| badge.name.as_str())),
        channel_name: msg.channel_login.clone(),
        bits_amount,
        sub_length,
        is_sub: msg.badges.iter().any(|badge| badge.name == "subscriber"),
        is_announcement: matches!(
            highlight_type.as_ref(),
            Some(HighlightType::Announcement { .. })
        ),
        is_system: authored_text.is_none(),
        is_highlighted: highlight_type.is_some(),
        is_cheer: matches!(highlight_type.as_ref(), Some(HighlightType::BitsBadge)),
        is_sub_message: matches!(highlight_type.as_ref(), Some(HighlightType::Subscription)),
        is_watch_streak: matches!(highlight_type.as_ref(), Some(HighlightType::WatchStreak)),
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(&msg.channel_login),
        ..Default::default()
    };

    let mut should_hide = false;
    let mut filter_highlight_match: Option<String> = None;
    let mut filter_highlighted = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }
        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Highlight => {
                filter_highlighted = true;
                if filter_highlight_match.is_none() {
                    filter_highlight_match =
                        result.matched_text.filter(|text| !text.trim().is_empty());
                }
            }
            FilterAction::Allow => {
                should_hide = false;
                filter_highlighted = false;
                filter_highlight_match = None;
                break;
            }
            FilterAction::Replace => {}
        }
    }
    if should_hide {
        crate::log_irc!(
            debug,
            "Filter: Hiding {} USERNOTICE in {}",
            event_type,
            msg.channel_login
        );
        return;
    }
    if highlight_type.is_none() {
        highlight_type = filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage));
    }

    let display_name = msg
        .source
        .tags
        .0
        .get("display-name")
        .cloned()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| msg.sender.name.clone());
    let base_id = if msg.message_id.trim().is_empty() {
        Uuid::new_v4().to_string()
    } else {
        msg.message_id.clone()
    };
    let timestamp = msg
        .source
        .tags
        .0
        .get("tmi-sent-ts")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| msg.server_timestamp.timestamp_millis());

    let mut rows = Vec::with_capacity(2);
    if let Some(content) = authored_text {
        rows.push(build_usernotice_content_row(
            &msg,
            content,
            &display_name,
            &format!("{base_id}:content"),
            timestamp,
            highlight_type.clone(),
        ));
    }
    if let Some(system) = system_text {
        let system_id = base_id.clone();
        rows.push(build_usernotice_system_row(
            &msg,
            system,
            &display_name,
            system_id,
            timestamp,
            highlight_type.clone(),
        ));
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let blocked_visibility = blocked_channel_visibility_snapshot(&msg.channel_login);
    if blocked_visibility.should_hide_sender(Some(&msg.sender.id), &msg.sender.login) {
        crate::log_irc!(
            debug,
            "Blocked users: Hiding USERNOTICE from {} after final visibility recheck in #{}",
            msg.sender.login,
            msg.channel_login
        );
        return;
    }
    let initial_visibility_pending = crate::backend::blocked_users::initial_visibility_pending();
    let inserted = {
        let mut buffer = CHAT_MESSAGES
            .entry(msg.channel_login.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        rows.retain(|row| !buffer.contains_id(row.id.as_ref()));
        if initial_visibility_pending
            && crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held()
                < rows.len()
        {
            pending_channel_row_capacity_available_commit_held();
            return;
        }
        for row in &mut rows {
            buffer.push_stamped(row);
        }
        rows
    };
    if inserted.is_empty() {
        return;
    }

    if initial_visibility_pending {
        for row in &inserted {
            defer_buffered_row_visibility_commit_held(&msg.channel_login, row.id.as_ref());
        }
        drop(visibility_commit);
        return;
    }

    crate::log_irc!(
        info,
        "Event: {} in {}: {}",
        event_type,
        msg.channel_login,
        filter_text
    );
    if let Some(sound) = sound_for_highlight_type(highlight_type.as_ref()) {
        SOUND_STORE.play_sound_for_channel(sound, &msg.channel_login);
    }
    if authored_text.is_some() {
        if let Some(color) = color.as_deref() {
            cache_user_color(&msg.sender.login, color);
        }
        track_chatter(&msg.channel_login, &msg.sender.login, &display_name, color);
    }
    for row in &inserted {
        crate::backend::history_cache::record_live_chat_message(&msg.channel_login, row);
    }
    bump_chat_version();
    for row in inserted {
        crate::backend::observers::get_observer_registry().notify(
            &msg.channel_login,
            crate::backend::observers::ChannelEvent::NewMessage(row),
        );
    }
    drop(visibility_commit);
}

fn build_usernotice_content_row(
    msg: &twitch_irc::message::UserNoticeMessage,
    raw_content: &str,
    display_name: &str,
    id: &str,
    timestamp: i64,
    highlight_type: Option<HighlightType>,
) -> ChatMessage {
    let (content, is_action) = normalize_message_text(raw_content);
    let action_prefix_chars = if is_action { 8 } else { 0 };
    let twitch_emotes: FxHashMap<usize, (String, usize)> = msg
        .emotes
        .iter()
        .filter_map(|emote| {
            if emote.char_range.end <= action_prefix_chars {
                return None;
            }
            Some((
                emote.char_range.start.saturating_sub(action_prefix_chars),
                (
                    emote.id.clone(),
                    emote.char_range.end.saturating_sub(action_prefix_chars),
                ),
            ))
        })
        .collect();
    let slots: Vec<_> = parse_to_slots(
        &content,
        &msg.channel_login,
        crate::backend::emotes::EmoteSender::new(Some(&msg.sender.id), &msg.sender.login),
        &twitch_emotes,
    );
    let structure_hash = compute_structure_hash(&slots);

    ChatMessage {
        id: Arc::from(id),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.login.as_str()),
        display_name: Arc::from(display_name),
        content: Arc::from(content),
        color: msg
            .name_color
            .map(|color| format_color_rgb(color.r, color.g, color.b)),
        badges: msg
            .badges
            .iter()
            .map(|badge| format_badge(&badge.name, &badge.version))
            .collect(),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type,
            is_system: false,
            is_usernotice: true,
            is_first_message: false,
            system_type: None,
            is_shared_chat: false,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message: false,
            is_hype_chat: false,
            is_automod: false,
            is_automod_pending: false,
            is_suspicious_user: false,
            is_restricted: false,
            is_monitored: false,
            is_similar: false,
            is_alternate_row: false,
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: msg
            .source
            .tags
            .0
            .get("emotes")
            .map(|tag| Arc::from(tag.as_str())),
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: action_prefix_chars,
    }
}

fn build_usernotice_system_row(
    msg: &twitch_irc::message::UserNoticeMessage,
    text: &str,
    display_name: &str,
    id: String,
    timestamp: i64,
    highlight_type: Option<HighlightType>,
) -> ChatMessage {
    let recipient_display_name = msg
        .source
        .tags
        .0
        .get("msg-param-recipient-display-name")
        .filter(|name| !name.is_empty())
        .or_else(|| {
            msg.source
                .tags
                .0
                .get("msg-param-recipient-user-name")
                .filter(|name| !name.is_empty())
        });
    let mut names = Vec::with_capacity(2);
    if !display_name.is_empty() {
        names.push(display_name);
    }
    if let Some(recipient_display_name) = recipient_display_name {
        names.push(recipient_display_name.as_str());
    }
    let slots = usernotice_core::usernotice_system_slots(text, &names);
    let structure_hash = compute_structure_hash(&slots);
    ChatMessage {
        id: Arc::from(id),
        channel: Arc::from(msg.channel_login.as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(msg.sender.login.as_str()),
        display_name: Arc::from(display_name),
        content: Arc::from(text),
        color: msg
            .name_color
            .map(|color| format_color_rgb(color.r, color.g, color.b)),
        badges: smallvec::SmallVec::new(),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action: false,
            highlight_type,
            is_system: true,
            is_usernotice: true,
            is_first_message: false,
            system_type: Some(SystemMessageType::Info),
            is_shared_chat: false,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message: false,
            is_hype_chat: false,
            is_automod: false,
            is_automod_pending: false,
            is_suspicious_user: false,
            is_restricted: false,
            is_monitored: false,
            is_similar: false,
            is_alternate_row: false,
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: Some(Arc::from(msg.sender.id.as_str())),
        reply_emote_offset: 0,
    }
}

fn usernotice_highlight_type(kind: UsernoticeHighlightKind) -> HighlightType {
    match kind {
        UsernoticeHighlightKind::Subscription => HighlightType::Subscription,
        UsernoticeHighlightKind::Raid => HighlightType::Raid,
        UsernoticeHighlightKind::Announcement => HighlightType::Announcement { color: None },
        UsernoticeHighlightKind::BitsBadge => HighlightType::BitsBadge,
        UsernoticeHighlightKind::WatchStreak => HighlightType::WatchStreak,
    }
}

/// The membership publisher already owns the visibility commit gate. Keeping
/// this non-locking entry point avoids self-deadlock while preserving the
/// global gate -> membership -> chat/whisper lock order.
pub(crate) fn apply_blocked_user_filter_to_buffers_visibility_commit_held() -> HashSet<String> {
    let Some(policy) = crate::backend::blocked_users::visibility_policy_snapshot() else {
        return HashSet::new();
    };
    if policy.is_empty() {
        return HashSet::new();
    }

    let channels: Vec<String> = CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    let expected_changed_channels: HashSet<String> = channels
        .iter()
        .filter(|channel| blocked_user_filter_would_change_channel(channel, &policy))
        .cloned()
        .collect();
    mark_blocked_channel_refresh_handoff(&expected_changed_channels);
    let mut changed_channels = HashSet::new();
    for channel in channels {
        if apply_blocked_user_filter_to_channel_with_policy(&channel, &policy) {
            changed_channels.insert(channel);
        }
    }

    crate::backend::whispers::remove_blocked_users_with_policy(&policy);
    let late_changes: HashSet<String> = changed_channels
        .difference(&expected_changed_channels)
        .cloned()
        .collect();
    mark_blocked_channel_refresh_handoff(&late_changes);
    changed_channels.extend(expected_changed_channels);
    changed_channels
}

fn mark_blocked_channel_refresh_handoff(channels: &HashSet<String>) {
    if channels.is_empty() {
        return;
    }
    let observers = crate::backend::observers::get_observer_registry();
    crate::backend::blocked_users::mark_chat_state_reconcile_pending(
        channels
            .iter()
            .filter(|channel| observers.has_live_observers(channel)),
    );
}

pub(crate) fn dispatch_blocked_channel_refreshes(channels: HashSet<String>) {
    if channels.is_empty() {
        return;
    }
    let observers = crate::backend::observers::get_observer_registry();
    bump_chat_version();
    for channel in channels {
        observers.notify(
            &channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
}

/// A new account has no authoritative block list yet. Rendering and direct
/// bridge reads consult the revoke flag immediately; observer wakes ensure all
/// retained ChatState snapshots reconcile on the next UI turn.
pub(crate) fn notify_blocked_resident_visibility_revoked() {
    let channels: HashSet<String> = CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    mark_blocked_channel_refresh_handoff(&channels);
    dispatch_blocked_channel_refreshes(channels);
}

fn remove_pending_blocked_visibility_rows(
    rows: Vec<crate::backend::blocked_users::PendingChannelRow>,
) {
    let mut rows_by_channel: HashMap<String, HashSet<String>> = HashMap::new();
    for row in rows {
        rows_by_channel
            .entry(row.channel)
            .or_default()
            .insert(row.message_id);
    }
    for (channel, message_ids) in rows_by_channel {
        if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel) {
            buffer.retain(|message| !message_ids.contains(message.id.as_ref()));
        }
    }
}

/// Account transitions discard backend-only rows that were never made
/// authoritative. The caller owns the visibility commit gate.
pub(crate) fn discard_pending_blocked_visibility_rows_commit_held(
    rows: Vec<crate::backend::blocked_users::PendingChannelRow>,
) {
    remove_pending_blocked_visibility_rows(rows);
}

fn pending_channel_row_capacity_available_commit_held() -> bool {
    if crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held() == 0 {
        log_pending_channel_overflow();
        false
    } else {
        true
    }
}

fn log_pending_channel_overflow() {
    if crate::backend::blocked_users::should_log_pending_channel_overflow() {
        crate::log_irc!(
            warn,
            "Blocked users: Pending channel-row queue reached its bounded capacity; dropping new backend-only rows until authority resolves"
        );
    }
}

fn defer_buffered_row_visibility_commit_held(channel: &str, message_id: &str) {
    if !crate::backend::blocked_users::defer_channel_row_visibility_commit_held(channel, message_id)
    {
        crate::log_irc!(
            error,
            "Blocked users: Pending channel-row admission raced despite the visibility commit gate"
        );
    }
}

#[derive(Default)]
pub(crate) struct BlockedVisibilityEffects {
    channels: HashSet<String>,
    whispers: Vec<(
        crate::backend::blocked_users::PendingWhisper,
        crate::backend::whispers::WhisperPublication,
    )>,
}

/// Complete the initial visibility transaction after either an authoritative
/// membership publication or the deliberate Failed/fail-open state. The
/// caller owns the visibility commit gate. External observer/sound work is
/// returned so it can run only after the gate is released.
pub(crate) fn finalize_pending_blocked_visibility_commit_held(
    auth_epoch: u64,
) -> BlockedVisibilityEffects {
    let mut channels = apply_blocked_user_filter_to_buffers_visibility_commit_held();
    let pending = crate::backend::blocked_users::take_pending_visibility_commit_held(auth_epoch);

    channels.extend(
        pending
            .channel_rows
            .into_iter()
            .map(|row| row.channel.to_lowercase()),
    );
    if pending.reveal_resident_rows {
        channels.extend(CHAT_MESSAGES.iter().map(|entry| entry.key().clone()));
    }
    mark_blocked_channel_refresh_handoff(&channels);
    if pending.reveal_resident_rows {
        crate::backend::blocked_users::release_resident_visibility_commit_held();
        for channel in &channels {
            seed_chatters_from_visible_buffer(channel);
        }
    }
    let whispers = pending
        .whispers
        .into_iter()
        .map(|whisper| {
            let publication = crate::backend::whispers::record_received_visibility_commit_held(
                whisper.message.clone(),
            );
            (whisper, publication)
        })
        .collect();

    BlockedVisibilityEffects { channels, whispers }
}

pub(crate) fn dispatch_blocked_visibility_effects(effects: BlockedVisibilityEffects) {
    crate::backend::activity_persistence::hydrate_current_account();
    for channel in &effects.channels {
        crate::backend::history_cache::schedule_visible_channel_history_persist(channel);
    }
    dispatch_blocked_channel_refreshes(effects.channels);
    if !effects.whispers.is_empty() {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
    for (whisper, publication) in effects.whispers {
        crate::backend::whispers::dispatch_publication(publication);
        crate::log_irc!(
            debug,
            "Whisper from {} released after blocked-user visibility became authoritative",
            whisper.sender_login
        );
        SOUND_STORE.play_sound(SoundEventType::Whisper);
        notify_whisper(&whisper.message.display_name, &whisper.message.content);
    }
}

/// Prune one channel against a stable immutable policy. The policy and channel
/// role are captured before any chat/UI lock, then buffers are reconciled in
/// place without cloning complete ChatMessage rows.
fn apply_blocked_user_filter_to_channel_with_policy(
    channel: &str,
    policy: &crate::backend::blocked_users::BlockedVisibilityPolicy,
) -> bool {
    if policy.is_empty() {
        return false;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }
    let hide_blocked_senders =
        !blocked_visibility_role_state_for(&channel, policy.visibility).unwrap_or(false);

    let result =
        CHAT_MESSAGES
            .get_mut(&channel)
            .map(|mut buffer| {
                buffer.reconcile(|message| {
                    if hide_blocked_senders
                        && is_user_attributed_message(message)
                        && policy.contains(message.user_id.as_deref(), message.sender.as_ref())
                    {
                        return MessageReconcileAction::Remove;
                    }
                    if message.reply.as_ref().is_some_and(|reply| {
                        policy.contains(None, reply.parent_user_login.as_ref())
                    }) {
                        message.reply = None;
                        return MessageReconcileAction::Mutated;
                    }
                    MessageReconcileAction::Keep
                })
            })
            .unwrap_or_default();

    if hide_blocked_senders && let Some(mut chatters) = RECENT_CHATTERS.get_mut(&channel) {
        let blocked: Vec<String> = chatters
            .iter()
            .filter(|(login, _)| policy.contains(None, login))
            .map(|(login, _)| login.clone())
            .collect();
        for login in blocked {
            chatters.pop(login.as_str());
        }
    }

    let mut mentions_changed = false;
    {
        let mut mentions = GLOBAL_MENTIONS.write();
        let before = mentions.len();
        mentions.retain_mut(|(mention_channel, message)| {
            if !mention_channel.eq_ignore_ascii_case(&channel) {
                return true;
            }
            if hide_blocked_senders
                && policy.contains(message.user_id.as_deref(), message.sender.as_ref())
            {
                return false;
            }
            if message
                .reply
                .as_ref()
                .is_some_and(|reply| policy.contains(None, reply.parent_user_login.as_ref()))
            {
                message.reply = None;
                mentions_changed = true;
            }
            true
        });
        mentions_changed |= mentions.len() != before;
    }

    if mentions_changed {
        reconcile_global_mention_unread();
    }

    if result.removed > 0 || result.mutated > 0 {
        crate::log_irc!(
            debug,
            "Blocked users: Pruned {} row(s) and redacted {} reply preview(s) from #{}",
            result.removed,
            result.mutated,
            channel
        );
    }
    if mentions_changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }

    result.removed > 0 || result.mutated > 0
}

/// Read-only precursor to the in-place reconciliation. The caller holds the
/// visibility commit gate, so the subsequent mutation observes the same
/// bounded message buffer and role policy.
fn blocked_user_filter_would_change_channel(
    channel: &str,
    policy: &crate::backend::blocked_users::BlockedVisibilityPolicy,
) -> bool {
    if policy.is_empty() {
        return false;
    }

    let channel = channel.trim_start_matches('#').to_lowercase();
    if channel.is_empty() {
        return false;
    }
    let hide_blocked_senders =
        !blocked_visibility_role_state_for(&channel, policy.visibility).unwrap_or(false);

    CHAT_MESSAGES.get(&channel).is_some_and(|buffer| {
        buffer.iter().any(|message| {
            (hide_blocked_senders
                && is_user_attributed_message(message)
                && policy.contains(message.user_id.as_deref(), message.sender.as_ref()))
                || message
                    .reply
                    .as_ref()
                    .is_some_and(|reply| policy.contains(None, reply.parent_user_login.as_ref()))
        })
    })
}

fn apply_blocked_user_filter_to_channel(channel: &str) -> bool {
    let Some(policy) = crate::backend::blocked_users::visibility_policy_snapshot() else {
        return false;
    };
    let expected_changed = blocked_user_filter_would_change_channel(channel, &policy);
    if expected_changed {
        mark_blocked_channel_refresh_handoff(&HashSet::from([channel.to_lowercase()]));
    }
    let changed = apply_blocked_user_filter_to_channel_with_policy(channel, &policy);
    if changed && !expected_changed {
        mark_blocked_channel_refresh_handoff(&HashSet::from([channel.to_lowercase()]));
    }
    changed || expected_changed
}

fn is_user_attributed_message(message: &ChatMessage) -> bool {
    !message.flags.is_system
        || message.user_id.is_some()
        || !message.sender.eq_ignore_ascii_case("system")
}

/// Reparse resident rows for one generation-fenced provider-visibility transaction.
/// Catalog publication itself never calls this: add/remove/rename events retain
/// parse-time bindings for already resident messages.
pub fn retokenize_all_channels_if_current(is_current: impl Fn() -> bool) {
    let channels: Vec<String> = CHAT_MESSAGES.iter().map(|r| r.key().clone()).collect();

    crate::log_emotes!(
        debug,
        "Provider visibility changed - re-tokenizing {} channels",
        channels.len()
    );
    for channel in channels {
        if !is_current() {
            return;
        }
        retokenize_channel_messages_if_current(&channel, &is_current);
    }
}

fn snapshot_channel_messages_for_observers(channel: &str) -> Arc<Vec<ChatMessage>> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Arc::new(Vec::new());
    }
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(channel);
    CHAT_MESSAGES
        .get_mut(channel)
        .map(|mut buffer| {
            if pending_ids.is_empty() {
                buffer.snapshot()
            } else {
                Arc::new(
                    buffer
                        .iter()
                        .filter(|message| !pending_ids.contains(message.id.as_ref()))
                        .cloned()
                        .collect(),
                )
            }
        })
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

/// Return the externally visible backend snapshot without exposing rows held
/// for the initial blocked-user authority decision.
pub(crate) fn visible_channel_messages(channel: &str) -> Vec<ChatMessage> {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel = channel.to_lowercase();
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(&channel);
    let messages = CHAT_MESSAGES
        .get(&channel)
        .map(|buffer| {
            buffer
                .iter()
                .filter(|message| !pending_ids.contains(message.id.as_ref()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    drop(visibility_commit);
    messages
}

/// Catalog scopes currently referenced by resident rows. Shared-chat source
/// channels may not be IRC-desired hosts but still need visibility rebuilds.
pub fn active_emote_lookup_channels() -> HashSet<String> {
    let mut channels = HashSet::new();
    for buffer in CHAT_MESSAGES.iter() {
        for message in buffer.value().iter() {
            match message.emote_lookup_channel.as_deref() {
                Some("") => {}
                Some(source) => {
                    channels.insert(source.to_string());
                }
                None if !message.channel.is_empty() => {
                    channels.insert(message.channel.to_string());
                }
                None => {}
            }
        }
    }
    channels
}

/// Return only the newest externally visible rows without cloning the entire
/// bounded channel buffer. This keeps small bridge reads cheap while the
/// initial blocked-user transaction still filters backend-only rows.
pub(crate) fn visible_channel_messages_tail(channel: &str, limit: usize) -> Vec<ChatMessage> {
    if limit == 0 {
        return Vec::new();
    }
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel = channel.to_lowercase();
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(&channel);
    let mut messages = CHAT_MESSAGES
        .get(&channel)
        .map(|buffer| {
            buffer
                .iter_recent(buffer.len())
                .filter(|message| !pending_ids.contains(message.id.as_ref()))
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    messages.reverse();
    drop(visibility_commit);
    messages
}

fn parse_twitch_emotes_for_message(msg: &ChatMessage) -> FxHashMap<usize, (String, usize)> {
    msg.twitch_emotes_tag
        .as_ref()
        .map(|tag| parse_emotes_tag_with_offset(tag, msg.reply_emote_offset))
        .unwrap_or_default()
}

fn message_emote_lookup_channel<'a>(msg: &'a ChatMessage, fallback_channel: &'a str) -> &'a str {
    msg.emote_lookup_channel
        .as_deref()
        .unwrap_or(fallback_channel)
}

fn publish_resolved_message_emote_scope(channel: &str, message_id: &str, source_channel: &str) {
    let channel = channel.to_lowercase();
    let message_id = message_id.to_string();
    let source_channel: Arc<str> = source_channel.to_lowercase().into();
    let mut pending = PENDING_MESSAGE_EMOTE_SCOPES.lock();
    let changed = update_buffered_message_emote_lookup_scope(
        &channel,
        &message_id,
        Arc::clone(&source_channel),
    );
    if changed {
        drop(pending);
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel);
        return;
    }
    if pending.len() >= MAX_PENDING_MESSAGE_EMOTE_SCOPES
        && let Some(stale_key) = pending.keys().next().cloned()
    {
        pending.remove(&stale_key);
    }
    pending.insert((channel, message_id), source_channel);
}

fn apply_pending_message_emote_scope(channel: &str, message_id: &str) {
    let channel = channel.to_lowercase();
    let message_id = message_id.to_string();
    let mut pending = PENDING_MESSAGE_EMOTE_SCOPES.lock();
    if let Some(source_channel) = pending.remove(&(channel.clone(), message_id.clone())) {
        update_buffered_message_emote_lookup_scope(&channel, &message_id, source_channel);
    }
}

fn update_buffered_message_emote_lookup_scope(
    channel: &str,
    message_id: &str,
    source_channel: Arc<str>,
) -> bool {
    CHAT_MESSAGES.get_mut(channel).is_some_and(|mut messages| {
        messages.update_by_id(message_id, |message| {
            message.emote_lookup_channel = Some(Arc::clone(&source_channel));
            message.emote_lookup_source_id = None;
        })
    })
}

fn reparse_message_slots(
    msg: &mut ChatMessage,
    fallback_channel: &str,
    force_install: bool,
) -> bool {
    let twitch_emotes = parse_twitch_emotes_for_message(msg);
    let emote_lookup_channel = message_emote_lookup_channel(msg, fallback_channel);
    let sender = if msg.flags.is_system {
        crate::backend::emotes::EmoteSender::NONE
    } else {
        crate::backend::emotes::EmoteSender::new(msg.user_id.as_deref(), &msg.sender)
    };
    let new_slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        &msg.content,
        emote_lookup_channel,
        sender,
        &twitch_emotes,
        msg.twitch_gifs_tag.as_deref(),
        msg.reply_emote_offset,
    );
    let new_hash = compute_structure_hash(&new_slots);
    let semantic_changed = !slots_semantically_equal(&msg.slots, &new_slots);
    if !force_install && !semantic_changed {
        return false;
    }
    msg.slots = new_slots.into();
    msg.structure_hash = new_hash;
    semantic_changed
}

/// Reparse the resident rows `user_id` sent in `channels` after that user's
/// 7TV personal emote set loaded or changed.
pub fn retokenize_user_messages(user_id: &str, channels: &[Arc<str>]) {
    let mut any_updated = false;
    for channel in channels {
        let channel = channel.to_lowercase();
        let candidates: Vec<ChatMessage> = match CHAT_MESSAGES.get(&channel) {
            Some(buffer) => buffer
                .iter()
                .filter(|msg| !msg.flags.is_system && msg.user_id.as_deref() == Some(user_id))
                .cloned()
                .collect(),
            None => continue,
        };
        let updated: Vec<ChatMessage> = candidates
            .into_iter()
            .filter_map(|mut msg| reparse_message_slots(&mut msg, &channel, false).then_some(msg))
            .collect();
        if updated.is_empty() {
            continue;
        }
        if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel) {
            for msg in updated {
                if let Some(index) = buffer.index_of(msg.id.as_ref()) {
                    buffer.update_at(index, msg);
                }
            }
        }
        any_updated = true;
        crate::backend::observers::get_observer_registry().notify(
            &channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
    if any_updated {
        bump_chat_version();
    }
}

fn retokenize_channel_messages_if_current(channel: &str, is_current: &impl Fn() -> bool) -> bool {
    let messages_to_process: Vec<(usize, ChatMessage)> = match CHAT_MESSAGES.get(channel) {
        Some(buffer) => buffer
            .iter()
            .enumerate()
            .filter(|(_, msg)| !msg.flags.is_system)
            .map(|(idx, msg)| (idx, msg.clone()))
            .collect(),
        None => {
            return false;
        }
    };

    let mut updated_messages: Vec<(usize, ChatMessage)> =
        Vec::with_capacity(messages_to_process.len() / 10);
    for (idx, mut msg) in messages_to_process {
        let old_hash = msg.structure_hash;
        let emote_lookup_channel = message_emote_lookup_channel(&msg, channel).to_string();
        if reparse_message_slots(&mut msg, channel, false) {
            if crate::log::emote_trace_enabled() {
                let preview = msg.content.chars().take(60).collect::<String>();
                crate::log_emotes!(
                    debug,
                    "[RETOKENIZE] Message '{}' structure changed (ch={}, emote_ch={}): old_hash={}, new_hash={}, slots: {:?}",
                    preview,
                    channel,
                    emote_lookup_channel,
                    old_hash,
                    msg.structure_hash,
                    msg.slots
                        .iter()
                        .map(|s| match s {
                            crate::backend::slots::MessageSlot::Text(t) => {
                                let text_preview = t.chars().take(15).collect::<String>();
                                format!("Text({})", text_preview)
                            }
                            crate::backend::slots::MessageSlot::Emote(e) =>
                                format!("Emote({})", e.name),
                            crate::backend::slots::MessageSlot::TwitchEmote { name, .. } =>
                                format!("TwitchEmote({})", name),
                            _ => "Other".to_string(),
                        })
                        .collect::<Vec<_>>()
                );
            }
            updated_messages.push((idx, msg));
        }
    }

    let updated_count = updated_messages.len();

    if !is_current() {
        return false;
    }

    if !updated_messages.is_empty()
        && is_current()
        && let Some(mut buffer) = CHAT_MESSAGES.get_mut(channel)
    {
        for (idx, updated_msg) in updated_messages {
            if !is_current() {
                return false;
            }
            if !buffer.update_at(idx, updated_msg.clone())
                && let Some(real_idx) = buffer.index_of(updated_msg.id.as_ref())
            {
                buffer.update_at(real_idx, updated_msg);
            }
        }
    }

    if !is_current() {
        return false;
    }

    if updated_count > 0 {
        crate::log_emotes!(
            info,
            "Re-tokenized {} messages for channel {}",
            updated_count,
            channel
        );

        crate::backend::observers::get_observer_registry().notify(
            channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }

    bump_chat_version();
    updated_count > 0
}

/// Add a system message to a channel
pub fn add_system_message(channel: &str, text: &str, msg_type: SystemMessageType) {
    let Some(highlight_type) = generic_system_message_filter_highlight(channel, text, &msg_type)
    else {
        return;
    };
    add_system_message_with_highlight(channel, text, msg_type, highlight_type);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMessageFlagOverrides {
    pub is_automod: bool,
    /// AutoMod hold awaiting a moderator decision (renders inline Allow/Deny).
    pub is_automod_pending: bool,
    pub is_suspicious_user: bool,
    pub is_restricted: bool,
    pub is_monitored: bool,
}

pub fn add_system_message_with_highlight(
    channel: &str,
    text: &str,
    msg_type: SystemMessageType,
    highlight_type: Option<HighlightType>,
) {
    add_system_message_with_highlight_and_flags(
        channel,
        text,
        msg_type,
        highlight_type,
        SystemMessageFlagOverrides::default(),
    );
}

pub fn add_system_message_with_highlight_and_flags(
    channel: &str,
    text: &str,
    msg_type: SystemMessageType,
    highlight_type: Option<HighlightType>,
    flag_overrides: SystemMessageFlagOverrides,
) {
    add_system_message_with_highlight_flags_id(
        channel,
        text,
        msg_type,
        highlight_type,
        flag_overrides,
        None,
    );
}

/// Like [`add_system_message_with_highlight_and_flags`] but lets the caller pin
/// the row id. AutoMod hold rows key on [`automod_row_id`] so their inline
/// Allow/Deny hitboxes and a later `automod.message.update` can target the row.
pub fn add_system_message_with_highlight_flags_id(
    channel: &str,
    text: &str,
    msg_type: SystemMessageType,
    highlight_type: Option<HighlightType>,
    flag_overrides: SystemMessageFlagOverrides,
    explicit_id: Option<String>,
) {
    add_system_message_with_highlight_flags_id_author(
        channel,
        text,
        msg_type,
        highlight_type,
        flag_overrides,
        explicit_id,
        false,
    );
}

fn add_system_message_with_highlight_flags_id_author(
    channel: &str,
    text: &str,
    msg_type: SystemMessageType,
    highlight_type: Option<HighlightType>,
    flag_overrides: SystemMessageFlagOverrides,
    explicit_id: Option<String>,
    defer_visibility: bool,
) {
    use crate::backend::slots::MessageSlot;

    let slots: Vec<MessageSlot> = if matches!(&msg_type, SystemMessageType::UploadSuccess { .. }) {
        parse_to_slots(
            text,
            channel,
            crate::backend::emotes::EmoteSender::NONE,
            &FxHashMap::default(),
        )
    } else {
        vec![MessageSlot::text(text)]
    };
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let pinned_id = explicit_id.filter(|id| !id.trim().is_empty());
    let id_is_pinned = pinned_id.is_some();
    let id: Arc<str> = pinned_id
        .map(Arc::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string().into());
    let sender = Arc::from("System");
    let display_name = Arc::from("System");
    let user_id = None;
    let timestamp = Utc::now().timestamp_millis();

    let mut msg = ChatMessage {
        id,
        channel: Arc::from(channel),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender,
        display_name,
        content: Arc::from(text),
        color: None,
        badges: smallvec::SmallVec::new(),
        timestamp,
        slots,
        structure_hash,
        reply: None,
        flags: crate::backend::models::MessageFlags {
            is_action: false,
            highlight_type,
            is_system: true,
            is_usernotice: false,
            is_first_message: false,
            system_type: Some(msg_type),
            is_shared_chat: false,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message: false,
            is_hype_chat: false,
            is_automod: flag_overrides.is_automod,
            is_automod_pending: flag_overrides.is_automod_pending,
            is_suspicious_user: flag_overrides.is_suspicious_user,
            is_restricted: flag_overrides.is_restricted,
            is_monitored: flag_overrides.is_monitored,
            is_similar: false,
            is_alternate_row: false,
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id,
        reply_emote_offset: 0,
    };

    if defer_visibility && !pending_channel_row_capacity_available_commit_held() {
        return;
    }
    if is_channel_closed(channel) {
        return;
    }

    {
        let mut buffer = CHAT_MESSAGES
            .entry(channel.to_string())
            .or_insert_with(MessageBuffer::with_default_capacity);
        if id_is_pinned && buffer.contains_id(&msg.id) {
            return;
        }
        buffer.push_stamped(&mut msg);
    }
    if defer_visibility {
        defer_buffered_row_visibility_commit_held(channel, msg.id.as_ref());
        return;
    }
    crate::backend::history_cache::record_live_chat_message(channel, &msg);

    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::NewMessage(msg),
    );
}

/// Prefix that keeps AutoMod hold rows out of Twitch's real chat-message id
/// namespace. Sharing the held id would make a CLEARMSG/mod-delete for that
/// message resolve to the AutoMod system row instead of the user's message,
/// and would let any later message carrying the id inherit the hold row's
/// cached layout.
pub const AUTOMOD_ROW_ID_PREFIX: &str = "automod_";

/// Row id for the inline AutoMod hold on `held_id`.
pub fn automod_row_id(held_id: &str) -> String {
    format!("{}{}", AUTOMOD_ROW_ID_PREFIX, held_id.trim())
}

/// Recover the bare Twitch message id from an AutoMod row id. The Helix
/// manage-held-message endpoint only accepts the un-namespaced id.
pub fn automod_held_message_id(row_id: &str) -> &str {
    row_id
        .strip_prefix(AUTOMOD_ROW_ID_PREFIX)
        .unwrap_or(row_id)
        .trim()
}

/// Resolve an inline AutoMod hold row in place when its `automod.message.update`
/// arrives: swap the row text for the resolved outcome and clear the pending
/// state so the renderer stops drawing the Allow/Deny actions, rather than
/// appending a second row. Returns true when a held row with `held_id` existed,
/// so the caller can skip inserting a duplicate resolved row.
pub fn resolve_automod_message(
    channel: &str,
    held_id: &str,
    text: &str,
    flag_overrides: SystemMessageFlagOverrides,
) -> bool {
    use crate::backend::slots::MessageSlot;

    let held_id = held_id.trim();
    if held_id.is_empty() {
        return false;
    }
    let row_id = automod_row_id(held_id);
    let resolved_text = text.to_string();

    let changed = CHAT_MESSAGES
        .get_mut(channel)
        .map(|mut buffer| {
            buffer.update_by_id(row_id.as_str(), |msg| {
                let slots: Vec<MessageSlot> = vec![MessageSlot::text(resolved_text.as_str())];
                msg.structure_hash = compute_structure_hash(&slots);
                msg.slots = slots.into();
                msg.content = Arc::from(resolved_text.as_str());
                msg.flags.is_automod = flag_overrides.is_automod;
                msg.flags.is_automod_pending = false;
                msg.flags.is_suspicious_user = flag_overrides.is_suspicious_user;
                msg.flags.is_restricted = flag_overrides.is_restricted;
                msg.flags.is_monitored = flag_overrides.is_monitored;
            })
        })
        .unwrap_or(false);

    if changed {
        bump_chat_version();
        crate::backend::observers::get_observer_registry().notify(
            channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
    changed
}

fn generic_system_message_filter_highlight(
    channel: &str,
    text: &str,
    msg_type: &SystemMessageType,
) -> Option<Option<HighlightType>> {
    let filter_ctx = TestContext {
        content: text.to_string(),
        author: "System".to_string(),
        author_login: "system".to_string(),
        channel_name: channel.trim_start_matches('#').to_string(),
        event_action: system_message_filter_label(msg_type).to_string(),
        event_source: "echo".to_string(),
        event_status: system_message_filter_status(msg_type).to_string(),
        event_detail: system_message_filter_detail(msg_type, text),
        is_system: true,
        channel_watching: true,
        channel_live: crate::backend::bridge::get_cached_channel_live_status(channel),
        ..Default::default()
    };

    let mut should_hide = false;
    let mut filter_highlight_match: Option<String> = None;
    let mut filter_highlighted = false;
    for result in FILTER_STORE.test_message(&filter_ctx) {
        if !result.matched {
            continue;
        }

        match result.action {
            FilterAction::Hide => {
                should_hide = true;
                break;
            }
            FilterAction::Highlight => {
                filter_highlighted = true;
                if filter_highlight_match.is_none() {
                    filter_highlight_match =
                        result.matched_text.filter(|text| !text.trim().is_empty());
                }
            }
            FilterAction::Allow => {
                should_hide = false;
                filter_highlighted = false;
                filter_highlight_match = None;
                break;
            }
            FilterAction::Replace => {}
        }
    }

    if should_hide {
        crate::log_irc!(debug, "Filter: Hiding generic system row in {}", channel);
        return None;
    }

    Some(
        filter_highlight_match
            .map(|pattern| HighlightType::Phrase { pattern })
            .or_else(|| filter_highlighted.then_some(HighlightType::HighlightedMessage)),
    )
}

fn system_message_filter_label(msg_type: &SystemMessageType) -> &'static str {
    match msg_type {
        SystemMessageType::UploadStart => "upload_start",
        SystemMessageType::UploadSuccess { .. } => "upload_success",
        SystemMessageType::UploadError { .. } => "upload_error",
        SystemMessageType::Info => "info",
        SystemMessageType::Error { .. } => "error",
        SystemMessageType::ConnectionStatus { .. } => "connection",
    }
}

fn system_message_filter_status(msg_type: &SystemMessageType) -> &str {
    match msg_type {
        SystemMessageType::UploadStart => "start",
        SystemMessageType::UploadSuccess { .. } => "success",
        SystemMessageType::UploadError { .. } => "error",
        SystemMessageType::Info => "info",
        SystemMessageType::Error { .. } => "error",
        SystemMessageType::ConnectionStatus { status } => status.as_str(),
    }
}

fn system_message_filter_detail(msg_type: &SystemMessageType, text: &str) -> String {
    match msg_type {
        SystemMessageType::UploadSuccess { url, delete_url } => delete_url
            .as_ref()
            .map(|delete_url| format!("{url} {delete_url}"))
            .unwrap_or_else(|| url.clone()),
        SystemMessageType::UploadError { error } => error.clone(),
        SystemMessageType::Error { message } => message.clone(),
        _ => text.to_string(),
    }
}

fn add_connection_status_message(channel: &str, text: &str, status: &str) {
    add_system_message(
        channel,
        text,
        SystemMessageType::ConnectionStatus {
            status: status.to_string(),
        },
    );
}

fn add_history_error_message(channel: &str, prefix: &str, error: &str) {
    let detail = crate::backend::bridge::trim_error_detail(error, 180, "Unknown network error");
    let text = format!("{prefix} {detail}");
    add_system_message(
        channel,
        &text,
        SystemMessageType::Error {
            message: text.clone(),
        },
    );
}

/// Clear locally displayed messages for a channel without sending a Twitch
/// moderation command (`/clearmessages`).
pub fn clear_channel_messages(channel: &str) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel_key) {
        buffer.replace_all(Vec::new());
    }
    drop(visibility_commit);

    crate::backend::history_cache::schedule_visible_channel_history_persist(&channel_key);
    bump_chat_version();
    crate::backend::observers::get_observer_registry().notify(
        &channel_key,
        crate::backend::observers::ChannelEvent::HistoryLoaded(Arc::new(Vec::new())),
    );
}

/// Insert a locally constructed chat message and notify current observers.
pub fn insert_local_message(channel: &str, mut message: ChatMessage) {
    let channel_key = channel.trim_start_matches('#').to_lowercase();
    if channel_key.is_empty() {
        return;
    }

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    CHAT_MESSAGES
        .entry(channel_key.clone())
        .or_insert_with(MessageBuffer::with_default_capacity)
        .push_stamped(&mut message);

    bump_chat_version();
    // Notify under the gate, like received rows, so the UI appends rows in
    // buffer order and each keeps the zebra parity it was stored with.
    crate::backend::observers::get_observer_registry().notify(
        &channel_key,
        crate::backend::observers::ChannelEvent::NewMessage(message),
    );
    drop(visibility_commit);
}

/// Connect to a channel with automatic reconnection (supervised connection)
/// This is the main entry point - reconnects automatically
pub async fn connect_and_listen(channel: String) {
    crate::log_irc!(info, "Requesting connection for channel: {}", channel);

    init_connection_broadcast();

    add_desired_channel(&channel);

    let should_start_supervisor = SUPERVISOR_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok();

    if should_start_supervisor {
        crate::log_irc!(info, "Starting new supervisor for channel: {}", channel);
    } else {
        crate::log_irc!(
            info,
            "Supervisor already running, channel {} added to desired list",
            channel
        );
    }

    if should_start_supervisor {
        tokio::spawn(async move {
            struct SupervisorGuard;
            impl Drop for SupervisorGuard {
                fn drop(&mut self) {
                    SUPERVISOR_RUNNING.store(false, Ordering::SeqCst);
                    crate::log_irc!(info, "Supervisor guard dropped, flag reset for recovery");
                }
            }
            let _guard = SupervisorGuard;

            run_supervised_read_connection().await;
        });
    }
}

/// Stop new IRC message admission and wait for the supervised reader to exit.
pub fn shutdown_read_connection(timeout: std::time::Duration) -> bool {
    READ_SHUTDOWN_REQUESTED.store(true, Ordering::Release);
    CONNECTION_RESTART_NOTIFY.notify_waiters();
    CHANNEL_JOIN_NOTIFY.notify_waiters();

    let started = std::time::Instant::now();
    while SUPERVISOR_RUNNING.load(Ordering::Acquire) {
        if started.elapsed() >= timeout {
            crate::log_irc!(
                warn,
                "Timed out stopping IRC read connection during shutdown"
            );
            return false;
        }
        std::thread::yield_now();
    }
    true
}

/// Return the action body for a `/me <text>` submission.
///
/// Command dispatch is ASCII case-insensitive, so the wire path must accept
/// `/ME` the same way instead of rendering the optimistic row as ordinary text.
fn action_message_body(message: &str) -> Option<&str> {
    let prefix = message.get(..4)?;
    prefix.eq_ignore_ascii_case("/me ").then(|| &message[4..])
}

#[derive(Debug, Deserialize)]
struct HelixChatSendResponse {
    #[serde(default)]
    data: Vec<HelixChatSendResult>,
}

#[derive(Debug, Deserialize)]
struct HelixChatSendResult {
    #[serde(default)]
    message_id: String,
    is_sent: bool,
    #[serde(default)]
    drop_reason: Option<HelixChatDropReason>,
}

#[derive(Debug, Deserialize)]
struct HelixChatDropReason {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Strictly classify the authoritative Helix response. A local chat row is
/// allowed only for a positive `is_sent` result with Twitch's real message ID.
/// A failed chat send. `delivery_unknown` marks failures after which Twitch
/// may still have posted the message (network error, 5xx, an unreadable or
/// inconsistent send result, an account switch mid-request, IRC `/me`), so
/// the text must not go back into the draft where a second Enter repeats it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SendError {
    pub message: String,
    pub delivery_unknown: bool,
}

impl SendError {
    fn delivery_unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            delivery_unknown: true,
        }
    }
}

impl From<String> for SendError {
    fn from(message: String) -> Self {
        Self {
            message,
            delivery_unknown: false,
        }
    }
}

impl From<&str> for SendError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn accepted_helix_chat_message_id(response: HelixChatSendResponse) -> Result<String, SendError> {
    let [result] = response.data.as_slice() else {
        return Err(SendError::delivery_unknown(
            "Twitch returned a malformed chat-send result; the message was not shown locally.",
        ));
    };
    if result.is_sent && result.drop_reason.is_some() {
        return Err(SendError::delivery_unknown(
            "Twitch returned an inconsistent chat-send result; the message was not shown locally.",
        ));
    }
    if !result.is_sent {
        let reason = result
            .drop_reason
            .as_ref()
            .map(|reason| {
                crate::backend::ui_actions::composer_send::rejection_reason(
                    &reason.code,
                    &reason.message,
                )
            })
            .unwrap_or_else(|| "Twitch declined the message without a reason".to_string());
        return Err(reason.into());
    }
    let message_id = result.message_id.trim();
    if message_id.is_empty() {
        return Err(SendError::delivery_unknown(
            "Twitch accepted the request without a message ID; the message was not shown locally.",
        ));
    }
    Ok(message_id.to_string())
}

fn confirmed_send_lane(account_user_id: &str, channel: &str) -> Arc<ConfirmedSendLane> {
    if CONFIRMED_SEND_LANES.len() > DEDUP_MAX_CHANNELS {
        let connected = CONNECTED_CHANNELS.read();
        CONFIRMED_SEND_LANES.retain(|(lane_account, lane_channel), lane| {
            (lane_account == account_user_id && connected.contains(lane_channel))
                || Arc::strong_count(lane) > 1
        });
    }
    let key = (account_user_id.to_string(), channel.to_lowercase());
    CONFIRMED_SEND_LANES
        .entry(key)
        .or_insert_with(|| {
            Arc::new(ConfirmedSendLane {
                capacity: Arc::new(tokio::sync::Semaphore::new(
                    MAX_PENDING_CONFIRMED_SENDS_PER_LANE,
                )),
                serial: tokio::sync::Mutex::new(()),
            })
        })
        .clone()
}

async fn send_confirmed_helix_message(
    channel: &str,
    message: &str,
    reply_parent_message_id: Option<&str>,
    local_reply: Option<ReplyInfo>,
    mut auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    ensure_authenticated_send_ready()?;
    let channel_key = ensure_channel_send_ready(channel)?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }

    let lane = confirmed_send_lane(&auth.user_id, &channel_key);
    let _pending_permit = lane.capacity.clone().try_acquire_owned().map_err(|_| {
        format!(
            "Too many messages are waiting to be confirmed for #{channel_key}; wait for Twitch before sending more."
        )
    })?;
    let _serial = lane.serial.lock().await;
    ensure_authenticated_send_ready()?;
    ensure_channel_send_ready(&channel_key)?;

    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }
    let broadcaster_id = get_user_id_cached(&channel_key)
        .await
        .map_err(|error| format!("Could not resolve #{channel_key} for Twitch chat: {error}"))?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }

    let mut body = serde_json::json!({
        "broadcaster_id": broadcaster_id,
        "sender_id": auth.user_id.clone(),
        "message": message,
    });
    if let Some(parent_id) = reply_parent_message_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        body["reply_parent_message_id"] = serde_json::Value::String(parent_id.to_string());
    }

    let mut refreshed_after_unauthorized = false;
    let response = loop {
        let response = TWITCH_HTTP_CLIENT
            .post("https://api.twitch.tv/helix/chat/messages")
            .helix_auth(&auth.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!(
                    "Twitch could not confirm whether the message was sent: {error}"
                ))
            })?;
        if !auth.is_current() {
            return Err(SendError::delivery_unknown(
                "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
            ));
        }
        if response.status() != reqwest::StatusCode::UNAUTHORIZED || refreshed_after_unauthorized {
            break response;
        }
        refreshed_after_unauthorized = true;
        let detail = crate::backend::http::error_body(response).await;
        let refreshed =
            match crate::backend::auth::recover_helix_unauthorized(&auth.access_token, &detail)
                .await
            {
                Some(_) => crate::backend::auth::capture_authenticated_user_resource()
                    .filter(|refreshed| refreshed.user_id == auth.user_id),
                None => None,
            };
        let Some(refreshed) = refreshed else {
            return Err(format!(
                "Twitch rejected the message ({}): {}",
                reqwest::StatusCode::UNAUTHORIZED,
                crate::backend::bridge::trim_error_detail(&detail, 180, "Unknown network error")
            )
            .into());
        };
        auth = refreshed;
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = crate::backend::http::error_body(response).await;
        let payload = serde_json::from_str::<serde_json::Value>(&detail).ok();
        let reason = payload
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or(&detail)
            .to_string();
        if !status.is_server_error() {
            let account_id = auth.user_id.clone();
            let channel = channel_key.clone();
            let _ = auth.commit_if_current(|| {
                crate::backend::irc::room_state::note_send_rejection(
                    &account_id,
                    &channel,
                    "",
                    &reason,
                );
            });
        }
        return Err(SendError {
            message: match status {
                reqwest::StatusCode::TOO_MANY_REQUESTS => {
                    "Sending too quickly — try again shortly".to_string()
                }
                reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
                    "Message is too long for Twitch".to_string()
                }
                _ => crate::backend::ui_actions::composer_send::rejection_reason("", &reason),
            },
            delivery_unknown: status.is_server_error(),
        });
    }
    let response = response.json::<HelixChatSendResponse>().await.map_err(|error| {
        SendError::delivery_unknown(format!("Twitch returned an invalid chat-send result; the message was not shown locally: {error}"))
    })?;
    if !auth.is_current() {
        return Err(SendError::delivery_unknown(
            "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
        ));
    }
    if let Some(drop_reason) = response
        .data
        .iter()
        .find(|result| !result.is_sent)
        .and_then(|result| result.drop_reason.as_ref())
    {
        let account_id = auth.user_id.clone();
        let channel = channel_key.clone();
        let code = drop_reason.code.clone();
        let text = drop_reason.message.clone();
        let _ = auth.commit_if_current(|| {
            crate::backend::irc::room_state::note_send_rejection(
                &account_id,
                &channel,
                &code,
                &text,
            );
        });
    }
    let message_id = accepted_helix_chat_message_id(response)?;
    let authenticated_user_id = auth.user_id.clone();
    auth.commit_if_current(|| {
        publish_confirmed_message(
            &channel_key,
            message,
            &message_id,
            local_reply,
            &authenticated_user_id,
        );
        crate::backend::irc::room_state::record_confirmed_send(
            &authenticated_user_id,
            &channel_key,
        );
    })
    .ok_or_else(|| {
        SendError::delivery_unknown(
            "The active Twitch account changed while Twitch processed the message; it was not shown locally.",
        )
    })?;
    Ok(())
}

/// Send a message bound to the account that owned the composer submission.
/// The bridge captures this resource before spawning its asynchronous task.
pub(crate) async fn send_message_with_auth(
    channel: &str,
    message: &str,
    auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    crate::benchmark_profile::reject_live_outbound_user_action("chat message")?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the message was sent.".into());
    }
    if let Some(action_body) = action_message_body(message) {
        ensure_authenticated_send_ready()?;
        let channel_key = ensure_channel_send_ready(channel)?;
        let client = get_irc_client().ok_or("IRC client not connected")?;
        client
            .me(channel_key, action_body.to_string())
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!("Failed to send action message: {error}"))
            })?;
        add_system_message(
            channel.trim_start_matches('#'),
            "Action message submitted over IRC. Delivery is unconfirmed; no local sent row has been added.",
            SystemMessageType::Info,
        );
        return Ok(());
    }

    let channel_key = ensure_channel_send_ready(channel)?;
    send_confirmed_helix_message(&channel_key, message, None, None, auth).await
}

pub fn ensure_authenticated_send_ready() -> Result<(), String> {
    if is_anonymous_mode() {
        return Err(
            "Cannot send Twitch chat while Echo is connected anonymously. Turn off Anonymous in the account switcher, re-authenticate the active Twitch account, or wait for the authenticated reconnect to finish."
                .to_string(),
        );
    }

    if crate::backend::auth::get_needs_reauth() {
        return Err(
            "Cannot send Twitch chat until the active Twitch account is re-authenticated."
                .to_string(),
        );
    }

    let Some(username) = get_username().filter(|username| !username.trim().is_empty()) else {
        return Err("Cannot send Twitch chat without an active Twitch account.".to_string());
    };

    if get_token()
        .as_deref()
        .is_none_or(|token| token.trim().is_empty())
    {
        return Err(format!(
            "Cannot send Twitch chat as {username}; the active account has no chat token. Re-authenticate the account."
        ));
    }

    match get_connection_status() {
        ConnectionStatus::Connected => {}
        ConnectionStatus::Reconnecting => {
            return Err(
                "Cannot send Twitch chat while Echo is still connecting or reconnecting to Twitch IRC."
                    .to_string(),
            );
        }
        ConnectionStatus::Disconnected => {
            return Err(
                "Cannot send Twitch chat while Twitch IRC is disconnected. Wait for Echo to reconnect."
                    .to_string(),
            );
        }
    }

    if get_irc_client().is_none() {
        return Err(
            "Cannot send Twitch chat until the Twitch IRC connection is ready.".to_string(),
        );
    }

    Ok(())
}

pub fn ensure_channel_send_ready(channel: &str) -> Result<String, String> {
    let channel_key = channel.trim_start_matches('#').trim().to_lowercase();
    if channel_key.is_empty() {
        return Err("Cannot send Twitch chat without a channel.".to_string());
    }

    if !is_channel_join_confirmed(&channel_key) {
        return Err(format!("Joining #{channel_key} — try again in a moment"));
    }

    if let Some(status) = crate::backend::irc::room_state::current_send_eligibility(&channel_key) {
        return Err(status.block.text(&channel_key));
    }

    Ok(channel_key)
}

fn reply_info_for_confirmed_message(
    channel: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
    reply_to_display_name: &str,
) -> ReplyInfo {
    let parent_body = CHAT_MESSAGES
        .get(&channel.to_lowercase())
        .and_then(|buffer| {
            buffer
                .iter()
                .find(|m| m.id.as_ref() == reply_to_msg_id)
                .map(|m| m.content.to_string())
        });

    ReplyInfo {
        parent_msg_id: Arc::from(reply_to_msg_id),
        parent_user_login: Arc::from(reply_to_user.to_lowercase()),
        parent_display_name: Arc::from(reply_to_display_name),
        parent_user_id: None,
        parent_msg_body: parent_body.map(Arc::from),
    }
}

/// Build a local row only after Twitch has assigned its canonical ID.
fn build_confirmed_message(
    channel: &str,
    message: &str,
    message_id: &str,
    reply: Option<ReplyInfo>,
    authenticated_user_id: &str,
) -> Option<ChatMessage> {
    let username = get_username()?;

    let display_name = crate::backend::settings::get_active_account()
        .and_then(|a| a.display_name)
        .unwrap_or_else(|| username.clone());

    let (color, badges) = get_self_state(channel).unwrap_or((None, Vec::new()));

    let (clean_text, is_action) = if let Some(stripped) = action_message_body(message) {
        (stripped.to_string(), true)
    } else {
        (message.to_string(), false)
    };

    let slots: Vec<_> = parse_to_slots(
        &clean_text,
        channel,
        crate::backend::emotes::EmoteSender::new(Some(authenticated_user_id), &username),
        &FxHashMap::default(),
    );
    let structure_hash = compute_structure_hash(&slots);
    let slots: std::sync::Arc<[_]> = slots.into();

    let chat_msg = ChatMessage {
        id: Arc::from(message_id),
        channel: Arc::from(channel.to_lowercase().as_str()),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(username.to_lowercase().as_str()),
        display_name: display_name.into(),
        content: clean_text.clone().into(),
        color: color.map(|c| Arc::from(c.as_str())),
        badges: badges.into_iter().map(|b| Arc::from(b.as_str())).collect(),
        timestamp: Utc::now().timestamp_millis(),
        slots,
        structure_hash,
        reply,
        flags: crate::backend::models::MessageFlags {
            is_action,
            highlight_type: None,
            is_system: false,
            is_usernotice: false,
            is_first_message: false,
            system_type: None,
            is_shared_chat: false,
            is_deleted: false,
            deleted_presentation: Default::default(),
            is_elevated_message: false,
            is_hype_chat: false,
            is_automod: false,
            is_automod_pending: false,
            is_suspicious_user: false,
            is_restricted: false,
            is_monitored: false,
            is_similar: false,
            is_alternate_row: false,
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: Some(Arc::from(authenticated_user_id)),
        reply_emote_offset: 0,
    };
    Some(chat_msg)
}

fn publish_confirmed_message(
    channel: &str,
    message: &str,
    message_id: &str,
    reply: Option<ReplyInfo>,
    authenticated_user_id: &str,
) {
    let channel_key = channel.trim_start_matches('#').trim().to_lowercase();
    let message_id = message_id.trim();
    if channel_key.is_empty() || message_id.is_empty() {
        return;
    }
    if CHAT_MESSAGES
        .get(&channel_key)
        .is_some_and(|buffer| buffer.contains_id(message_id))
    {
        return;
    }
    let Some(mut chat_msg) = build_confirmed_message(
        &channel_key,
        message,
        message_id,
        reply,
        authenticated_user_id,
    ) else {
        return;
    };
    let mut receipts = CONFIRMED_LOCAL_ECHOES.lock();
    if !mark_message_seen(&channel_key, message_id) {
        return;
    }
    receipts.put((channel_key.clone(), message_id.to_string()), ());
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let inserted = {
        let mut buffer = CHAT_MESSAGES
            .entry(channel_key.clone())
            .or_insert_with(MessageBuffer::with_default_capacity);
        insert_confirmed_local_row(&mut buffer, &mut chat_msg)
    };
    if !inserted {
        drop(visibility_commit);
        receipts.pop(&(channel_key, message_id.to_string()));
        return;
    }
    crate::backend::history_cache::record_live_chat_message(channel, &chat_msg);
    bump_chat_version();
    // Notify under the gate, like received rows, so the UI appends rows in
    // buffer order and each keeps the zebra parity it was stored with.
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::NewMessage(chat_msg),
    );
    drop(visibility_commit);
    drop(receipts);
}

/// Send a reply using Helix's reply-parent field. The local row appears only
/// after Twitch returns the reply's real message ID.
pub async fn send_reply(
    channel: &str,
    message: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
) -> Result<(), String> {
    let auth = crate::backend::auth::capture_authenticated_user_resource().ok_or(
        "Cannot send Twitch chat until Echo has the active account identity. Re-authenticate the account.",
    )?;
    send_reply_with_auth(channel, message, reply_to_msg_id, reply_to_user, auth)
        .await
        .map_err(|error| error.message)
}

/// Send a reply bound to the account that owned the composer submission.
pub(crate) async fn send_reply_with_auth(
    channel: &str,
    message: &str,
    reply_to_msg_id: &str,
    reply_to_user: &str,
    auth: crate::backend::auth::AuthenticatedUserResource,
) -> Result<(), SendError> {
    crate::benchmark_profile::reject_live_outbound_user_action("chat reply")?;
    if !auth.is_current() {
        return Err("The active Twitch account changed before the reply was sent.".into());
    }
    let channel_key = ensure_channel_send_ready(channel)?;
    if let Some(action_body) = action_message_body(message) {
        ensure_authenticated_send_ready()?;
        let client = get_irc_client().ok_or("IRC client not connected")?;
        let reply_target = (channel_key.as_str(), reply_to_msg_id);
        client
            .me_in_reply_to(&reply_target, action_body.to_string())
            .await
            .map_err(|error| {
                SendError::delivery_unknown(format!("Failed to send action reply: {error}"))
            })?;
        add_system_message(
            &channel_key,
            "Action reply submitted over IRC. Delivery is unconfirmed; no local sent row has been added.",
            SystemMessageType::Info,
        );
        return Ok(());
    }

    let reply = reply_info_for_confirmed_message(
        &channel_key,
        reply_to_msg_id,
        reply_to_user,
        reply_to_user,
    );
    send_confirmed_helix_message(
        &channel_key,
        message,
        Some(reply_to_msg_id),
        Some(reply),
        auth,
    )
    .await
}

#[cfg(test)]
mod timeout_format_tests {
    use super::format_timeout_duration;
    use core::prelude::v1::test;

    #[test]
    fn timeout_durations_read_compactly() {
        assert_eq!(format_timeout_duration(0), "0s");
        assert_eq!(format_timeout_duration(1), "1s");
        assert_eq!(format_timeout_duration(600), "10m");
        assert_eq!(format_timeout_duration(86_400), "1d");
        assert_eq!(format_timeout_duration(5_400), "1h 30m");
        assert_eq!(format_timeout_duration(1_209_600), "14d");
    }
}

#[cfg(test)]
mod restart_generation_tests {
    use super::restart_request_applies;
    use core::prelude::v1::test;

    #[test]
    fn stale_permit_for_an_older_connection_is_ignored() {
        assert!(!restart_request_applies(7, 5, 7));
    }

    #[test]
    fn restart_aimed_at_the_running_connection_applies() {
        assert!(restart_request_applies(7, 7, 7));
        assert!(restart_request_applies(7, 7, 8));
    }

    #[test]
    fn back_to_back_restarts_sharing_one_permit_still_apply() {
        assert!(restart_request_applies(7, 8, 9));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatMessage, HelixChatDropReason, HelixChatSendResponse, HelixChatSendResult,
        HistoryEmoteReadinessGuard, MessageBuffer, SharedChatEmoteLookupSelection,
        accepted_helix_chat_message_id, active_emote_lookup_channels,
        apply_pending_message_emote_scope, insert_confirmed_local_row,
        message_emote_lookup_channel, publish_history_emote_readiness,
        publish_resolved_message_emote_scope, select_shared_chat_emote_lookup,
        store_received_chat_row, update_buffered_message_emote_lookup_scope,
    };
    use core::prelude::v1::test;

    #[test]
    fn account_mentions_match_unicode_names_at_word_boundaries() {
        let matcher = super::AccountMentionMatcher::new("revulate", "Луна");
        assert!(matcher.is_match("Hello @лУнА!"));
        assert!(matcher.is_match("REVULATE and Луна"));
        assert!(!matcher.is_match("revulated луна123 prefixЛуна"));
        assert!(!matcher.is_match("revulate_name"));
        assert!(!matcher.is_match("revulate\u{301}"));
        assert!(super::AccountMentionMatcher::new("user", "Élodie").is_match("@ÉLODIE"));
        assert!(super::AccountMentionMatcher::new("user", "Σ").is_match("ς"));
        assert!(super::AccountMentionMatcher::new("user", "配信者").is_match("@配信者!"));
        assert!(!super::AccountMentionMatcher::new("", "").is_match("anything"));
    }

    #[test]
    fn confirmed_send_requires_one_positive_result_with_a_real_message_id() {
        let accepted = accepted_helix_chat_message_id(HelixChatSendResponse {
            data: vec![HelixChatSendResult {
                message_id: "real-twitch-id".to_string(),
                is_sent: true,
                drop_reason: None,
            }],
        });
        assert_eq!(accepted.as_deref(), Ok("real-twitch-id"));

        let dropped = accepted_helix_chat_message_id(HelixChatSendResponse {
            data: vec![HelixChatSendResult {
                message_id: String::new(),
                is_sent: false,
                drop_reason: Some(HelixChatDropReason {
                    code: "msg_rejected".to_string(),
                    message: "Message was not sent.".to_string(),
                }),
            }],
        });
        assert!(dropped.is_err_and(|error| !error.delivery_unknown));

        let malformed = accepted_helix_chat_message_id(HelixChatSendResponse { data: Vec::new() });
        assert!(malformed.is_err_and(|error| error.delivery_unknown));
    }

    #[test]
    fn canonical_twitch_id_prevents_duplicates_in_both_echo_race_orders() {
        let mut local = ChatMessage {
            id: "real-id".into(),
            content: "local rendering".into(),
            ..Default::default()
        };
        let mut canonical = ChatMessage {
            id: "real-id".into(),
            content: "Twitch rendering".into(),
            ..Default::default()
        };
        let mut api_first = MessageBuffer::with_default_capacity();
        assert!(insert_confirmed_local_row(&mut api_first, &mut local));
        assert!(store_received_chat_row(&mut api_first, &mut canonical));
        assert!(!insert_confirmed_local_row(&mut api_first, &mut local));
        assert_eq!(api_first.iter().count(), 1);
        assert_eq!(
            api_first.iter().next().unwrap().content.as_ref(),
            "Twitch rendering"
        );

        let mut irc_first = MessageBuffer::with_default_capacity();
        assert!(!store_received_chat_row(&mut irc_first, &mut canonical));
        assert!(!insert_confirmed_local_row(&mut irc_first, &mut local));
        assert_eq!(irc_first.iter().count(), 1);
        assert_eq!(
            irc_first.iter().next().unwrap().content.as_ref(),
            "Twitch rendering"
        );
    }

    #[test]
    fn shared_chat_emote_lookup_uses_source_or_global_scope_never_foreign_host() {
        let known = select_shared_chat_emote_lookup(
            "host_channel",
            "100",
            Some("Source_Channel"),
            Some("200"),
            |_| panic!("known source login must not consult the id cache"),
        );
        assert_eq!(
            known,
            SharedChatEmoteLookupSelection {
                channel_key: "source_channel".to_string(),
                unresolved_source_room_id: None,
            }
        );

        let cached = select_shared_chat_emote_lookup(
            "host_channel",
            "100",
            None,
            Some("200"),
            |source_id| {
                assert_eq!(source_id, "200");
                Some("Cached_Source".to_string())
            },
        );
        assert_eq!(
            cached,
            SharedChatEmoteLookupSelection {
                channel_key: "cached_source".to_string(),
                unresolved_source_room_id: None,
            }
        );

        let unresolved =
            select_shared_chat_emote_lookup("host_channel", "100", None, Some("200"), |_| None);
        assert_eq!(
            unresolved,
            SharedChatEmoteLookupSelection {
                channel_key: String::new(),
                unresolved_source_room_id: Some("200".to_string()),
            }
        );
        assert_ne!(unresolved.channel_key, "host_channel");

        let source_scoped = ChatMessage {
            emote_lookup_channel: Some("source_channel".into()),
            ..Default::default()
        };
        let global_user_only = ChatMessage {
            emote_lookup_channel: Some("".into()),
            ..Default::default()
        };
        let legacy_host = ChatMessage::default();
        assert_eq!(
            message_emote_lookup_channel(&source_scoped, "host_channel"),
            "source_channel"
        );
        assert_eq!(
            message_emote_lookup_channel(&global_user_only, "host_channel"),
            ""
        );
        assert_eq!(
            message_emote_lookup_channel(&legacy_host, "host_channel"),
            "host_channel"
        );
    }

    #[test]
    fn resolved_shared_source_updates_scope_without_retokenizing() {
        let channel = format!("shared-scope-{}", std::process::id());
        let message_id = "shared-message";
        let slots = vec![crate::backend::slots::MessageSlot::text("plain")];
        let structure_hash = crate::backend::slots::compute_structure_hash(&slots);
        let mut buffer = echo_core::message_buffer::MessageBuffer::with_default_capacity();
        buffer.push(ChatMessage {
            id: message_id.into(),
            channel: channel.clone().into(),
            emote_lookup_channel: Some("".into()),
            emote_lookup_source_id: Some("source-id".into()),
            content: "plain".into(),
            slots: slots.into(),
            structure_hash,
            ..Default::default()
        });
        super::CHAT_MESSAGES.insert(channel.clone(), buffer);

        assert!(update_buffered_message_emote_lookup_scope(
            &channel,
            message_id,
            "resolved_source".into()
        ));
        let message = super::CHAT_MESSAGES
            .get(&channel)
            .and_then(|messages| messages.get(0).cloned())
            .expect("shared row remains buffered");
        assert_eq!(
            message.emote_lookup_channel.as_deref(),
            Some("resolved_source")
        );
        assert_eq!(message.emote_lookup_source_id, None);
        assert!(matches!(
            message.slots.as_ref(),
            [crate::backend::slots::MessageSlot::Text(text)] if text.as_ref() == "plain"
        ));
        let active_scopes = active_emote_lookup_channels();
        assert!(active_scopes.contains("resolved_source"));
        drop(message);

        super::CHAT_MESSAGES.remove(&channel);

        let pending_id = "scope-before-insert";
        publish_resolved_message_emote_scope(&channel, pending_id, "Pending_Source");
        let mut pending_buffer = echo_core::message_buffer::MessageBuffer::with_default_capacity();
        pending_buffer.push(ChatMessage {
            id: pending_id.into(),
            channel: channel.clone().into(),
            emote_lookup_channel: Some("".into()),
            emote_lookup_source_id: Some("pending-id".into()),
            ..Default::default()
        });
        super::CHAT_MESSAGES.insert(channel.clone(), pending_buffer);
        apply_pending_message_emote_scope(&channel, pending_id);
        let pending_message = super::CHAT_MESSAGES
            .get(&channel)
            .and_then(|messages| messages.get(0).cloned())
            .expect("pending shared row remains buffered");
        assert_eq!(
            pending_message.emote_lookup_channel.as_deref(),
            Some("pending_source")
        );
        assert_eq!(pending_message.emote_lookup_source_id, None);
        drop(pending_message);
        super::CHAT_MESSAGES.remove(&channel);
    }

    #[test]
    fn failed_history_emote_readiness_releases_loaded_marker() {
        let channel = format!("history-readiness-{}", std::process::id());
        publish_history_emote_readiness(&channel, true);
        assert!(super::EMOTE_LOADED_CHANNELS.lock().contains(&channel));
        {
            let _cancelled = HistoryEmoteReadinessGuard::new(&channel);
            assert!(!super::EMOTE_LOADED_CHANNELS.lock().contains(&channel));
        }
        assert!(!super::EMOTE_LOADED_CHANNELS.lock().contains(&channel));
        {
            let mut completed = HistoryEmoteReadinessGuard::new(&channel);
            completed.commit();
        }
        assert!(super::EMOTE_LOADED_CHANNELS.lock().contains(&channel));
        publish_history_emote_readiness(&channel, false);
    }
}
