//! The Twitch IRC read connection: supervisor, watchdog and reconnect backoff,
//! channel membership, and the dispatch of incoming messages to the ingest
//! submodules.

use super::format::{
    CTCP_ACTION_PREFIX_CHARS, format_badge, format_color_rgb, normalize_message_text,
};
use super::user_colors::cache_user_color;
use crate::backend::badges::TWITCH_BADGE_CACHE;
use crate::backend::components::mod_log::{ModAction, ModActionType, add_mod_action};
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
use twitch_irc::login::{CredentialsPair, LoginCredentials};
use twitch_irc::message::ServerMessage;
use uuid::Uuid;

use super::join_scheduler;

mod activity;
mod anonymous;
mod blocked_visibility;
mod chatters;
mod emote_loading;
mod history_load;
mod moderation;
mod privmsg;
mod retokenize;
mod self_state;
mod send;
mod system_rows;
mod usernotice;
mod whisper;

pub use activity::*;
pub use anonymous::*;
pub(crate) use blocked_visibility::*;
pub use chatters::*;
pub(crate) use emote_loading::*;
pub use history_load::*;
pub(crate) use moderation::*;
pub(crate) use privmsg::*;
pub use retokenize::*;
pub use self_state::*;
pub use send::*;
pub use system_rows::*;
use usernotice::*;
use whisper::*;

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
pub(super) static SELF_JOINED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
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

/// Record per-channel disconnect state for gap-filling
/// Called when the read connection is lost to capture the last message timestamp
///
/// The earliest unconsumed state wins: a gap that began at system sleep, or
/// before a failed gap-fill, must not shrink when the dead socket is noticed.
pub(super) fn record_channel_disconnect(channel: &str) {
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

pub static CONNECTED_CHANNELS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

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

/// Login for the read connection. twitch-irc asks for credentials each time
/// its pool opens a socket, which it also does on its own (a server RECONNECT,
/// a ping timeout, a full connection) long after the client was built, so the
/// token is read at that moment rather than captured once.
pub struct EchoIrcCredentials {
    login: String,
    /// The token the client was built with; `None` logs in anonymously.
    token: Option<String>,
    fence: Option<Arc<crate::backend::auth::IrcCredentialFence>>,
}

impl EchoIrcCredentials {
    fn anonymous() -> Self {
        Self {
            login: "justinfan12345".to_string(),
            token: None,
            fence: None,
        }
    }
}

impl std::fmt::Debug for EchoIrcCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EchoIrcCredentials")
            .field("login", &self.login)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

#[async_trait::async_trait]
impl LoginCredentials for EchoIrcCredentials {
    type Error = std::convert::Infallible;

    async fn get_credentials(&self) -> Result<CredentialsPair, Self::Error> {
        let token = self
            .fence
            .as_ref()
            .and_then(|fence| fence.current_account_token())
            .or_else(|| self.token.clone());
        Ok(CredentialsPair {
            login: self.login.clone(),
            token,
        })
    }
}

pub static IRC_SENDER: LazyLock<
    arc_swap::ArcSwap<Option<TwitchIRCClient<EchoIrcTransport, EchoIrcCredentials>>>,
> = LazyLock::new(|| arc_swap::ArcSwap::from_pointee(None));

/// Get the current IRC client, if connected.
/// Clones the client out of the ArcSwap guard so callers get an owned value.
pub fn get_irc_client() -> Option<TwitchIRCClient<EchoIrcTransport, EchoIrcCredentials>> {
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
pub(super) static CONNECTION_GENERATION: AtomicU64 = AtomicU64::new(0);

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
            credential_fence =
                crate::backend::auth::capture_irc_credential_fence(&clean_token).map(Arc::new);
            crate::log_irc!(
                info,
                "Using authenticated IRC (user: {}, token_len: {})",
                username,
                clean_token.len()
            );
            ClientConfig::new_simple(EchoIrcCredentials {
                login: username,
                token: Some(clean_token),
                fence: credential_fence.clone(),
            })
        } else {
            crate::log_irc!(info, "Using anonymous IRC (no credentials)");
            ClientConfig::new_simple(EchoIrcCredentials::anonymous())
        }
    } else {
        crate::log_irc!(
            info,
            "Using anonymous IRC (auth previously failed or forced)"
        );
        ClientConfig::new_simple(EchoIrcCredentials::anonymous())
    };

    let self_login = config.login_credentials.login.to_lowercase();
    let (mut incoming_messages, client) =
        TwitchIRCClient::<EchoIrcTransport, EchoIrcCredentials>::new(config);

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
                Err(e) if e.is_proxy_not_configured() => {
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
                    .is_none_or(|fence| !fence.account_is_current()));
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
            return recover_from_irc_auth_failure(credential_fence.as_deref()).await;
        }

        match message {
            ServerMessage::Privmsg(msg) => {
                process_privmsg_message(msg).await;
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
