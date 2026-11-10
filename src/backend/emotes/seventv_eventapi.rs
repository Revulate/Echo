//! 7TV EventAPI v3 WebSocket client for real-time emote set updates.
//!
//! Protocol: wss://events.7tv.io/v3
//! Subscribes to `emote_set.update` events for each channel's emote set,
//! updates the emote cache on changes, and posts system messages to chat.
//! Channel-scoped `entitlement.*` events feed other chatters' personal emote
//! sets and trigger bounded cosmetics refetches for paint/badge changes.
//!
//! Dev replay harness:
//! - `ECHO_7TV_EVENTAPI_REPLAY=<path>` replays a JSON file or directory of JSON files.
//! - `ECHO_7TV_EVENTAPI_REPLAY_SET_ID=<set>` and
//!   `ECHO_7TV_EVENTAPI_REPLAY_CHANNEL=<channel|global>` map fixture updates.
//! - `ECHO_7TV_EVENTAPI_REPLAY_ONLY=1` skips the live websocket.
//! - `ECHO_7TV_EVENTAPI_REPLAY_DELAY_MS=1500` delays replay until caches load.
//!
//! Opcode reference:
//!   0  = DISPATCH   (server → client: event fired)
//!   1  = HELLO      (server → client: connection established, heartbeat interval)
//!   2  = HEARTBEAT  (server → client: keepalive)
//!   4  = RECONNECT  (server → client: server asks us to reconnect)
//!   5  = ACK        (server → client: command accepted)
//!   6  = ERROR      (server → client: command/session error)
//!   7  = END_STREAM (server → client: stream closing)
//!   35 = SUBSCRIBE  (client → server: subscribe to an event)
//!   36 = UNSUBSCRIBE (client → server)
//!   34 = RESUME     (client → server: reconnect with session)

use crate::backend::lock_utils::MutexExt;
use dashmap::DashMap;
use futures_util::{Sink, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

const EVENTAPI_URL: &str = "wss://events.7tv.io/v3";
const RECONNECT_BASE_MS: u64 = 1_000;
const RECONNECT_MAX_MS: u64 = 60_000;
const RECONNECT_SLOW_BASE_MS: u64 = 10_000;
const RECONNECT_BACKOFF: f64 = 2.0;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 25_000;
const HEARTBEAT_TIMEOUT_MULTIPLIER: u32 = 3;
const DISPATCH_REFRESH_DEBOUNCE_MS: u64 = 250;
const COMMAND_ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// Sentinel channel name for global emote set events
const GLOBAL_SET_CHANNEL: &str = "__global__";

mod op {
    pub const DISPATCH: u32 = 0;
    pub const HELLO: u32 = 1;
    pub const HEARTBEAT: u32 = 2;
    pub const RECONNECT: u32 = 4;
    pub const ACK: u32 = 5;
    pub const ERROR: u32 = 6;
    pub const END_OF_STREAM: u32 = 7;
    pub const RESUME: u32 = 34;
    pub const SUBSCRIBE: u32 = 35;
    pub const UNSUBSCRIBE: u32 = 36;
}

#[derive(Debug, Serialize, Deserialize)]
struct EventApiMessage {
    op: u32,
    #[serde(default)]
    d: serde_json::Value,
}

#[derive(Clone, Debug)]
pub(crate) struct SevenTvChannelSubscription {
    pub user_id: String,
    pub emote_set_id: String,
    pub connection_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SubscriptionKind {
    EmoteSetUpdate,
    UserUpdate,
    /// Channel-scoped: `object_id` is the Twitch channel id.
    EntitlementCreate,
    /// Channel-scoped: `object_id` is the Twitch channel id.
    EntitlementDelete,
}

impl SubscriptionKind {
    fn event_type(&self) -> &'static str {
        match self {
            Self::EmoteSetUpdate => "emote_set.update",
            Self::UserUpdate => "user.update",
            Self::EntitlementCreate => "entitlement.create",
            Self::EntitlementDelete => "entitlement.delete",
        }
    }

    fn from_event_type(event_type: &str) -> Option<Self> {
        Some(match event_type {
            "emote_set.update" => Self::EmoteSetUpdate,
            "user.update" => Self::UserUpdate,
            "entitlement.create" => Self::EntitlementCreate,
            "entitlement.delete" => Self::EntitlementDelete,
            _ => return None,
        })
    }

    fn is_channel_scoped(&self) -> bool {
        matches!(self, Self::EntitlementCreate | Self::EntitlementDelete)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SubscriptionKey {
    kind: SubscriptionKind,
    object_id: String,
}

impl SubscriptionKey {
    fn emote_set(set_id: impl Into<String>) -> Self {
        Self {
            kind: SubscriptionKind::EmoteSetUpdate,
            object_id: set_id.into(),
        }
    }

    fn user(user_id: impl Into<String>) -> Self {
        Self {
            kind: SubscriptionKind::UserUpdate,
            object_id: user_id.into(),
        }
    }

    fn channel_entitlements(twitch_channel_id: &str) -> [Self; 2] {
        [
            SubscriptionKind::EntitlementCreate,
            SubscriptionKind::EntitlementDelete,
        ]
        .map(|kind| Self {
            kind,
            object_id: twitch_channel_id.to_string(),
        })
    }

    fn event_type(&self) -> &'static str {
        self.kind.event_type()
    }

    fn condition(&self) -> serde_json::Value {
        if self.kind.is_channel_scoped() {
            serde_json::json!({
                "ctx": "channel",
                "platform": "TWITCH",
                "id": self.object_id,
            })
        } else {
            serde_json::json!({ "object_id": self.object_id })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SubscriptionCommand {
    Subscribe(SubscriptionKey),
    Unsubscribe(SubscriptionKey),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EventApiDiagnostics {
    pub connected: bool,
    pub desired_subscriptions: usize,
    pub active_subscriptions: usize,
    pub in_flight_subscription_commands: usize,
    pub reconnects: u64,
    pub last_heartbeat_ms_ago: Option<u64>,
    pub last_error: Option<String>,
    pub queued_subscription_commands: usize,
    pub pending_refreshes: usize,
    pub active_set_refreshes: usize,
    pub owned_channels: usize,
    pub owned_sets: usize,
    pub owned_users: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct EventApiChannelOwnership {
    pub channel: String,
    pub emote_set_id: String,
    pub user_id: Option<String>,
    pub connection_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct EventApiSetOwnership {
    pub emote_set_id: String,
    pub channels: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EventApiUserOwnership {
    pub user_id: String,
    pub channels: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EventApiOwnershipSnapshot {
    pub channels: Vec<EventApiChannelOwnership>,
    pub sets: Vec<EventApiSetOwnership>,
    pub users: Vec<EventApiUserOwnership>,
    pub active_set_refreshes: Vec<(String, String)>,
}

/// Whether the EventAPI task is already running
static RUNNING: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static RESTART_QUEUED: AtomicBool = AtomicBool::new(false);

/// Whether the dev replay worker has already been started.
static REPLAY_RUNNING: AtomicBool = AtomicBool::new(false);

/// `ECHO_7TV_EVENTAPI_REPLAY_ONLY`: replay fixtures without the live websocket.
fn replay_only() -> bool {
    echo_core::env::flag("ECHO_7TV_EVENTAPI_REPLAY_ONLY")
}

/// emote_set_id -> all channel owners. Multiple active channels can share one
/// 7TV set, and every owner needs dispatch refreshes.
static SET_TO_CHANNELS: LazyLock<DashMap<String, HashSet<String>>> = LazyLock::new(DashMap::new);

/// Channel_login -> emote_set_id (reverse map)
static CHANNEL_TO_SET: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);

/// Current global sentinel owner. Keep this separate from CHANNEL_TO_SET so
/// diagnostics continue to report only real channel ownership.
static GLOBAL_SET_OWNER: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Channel_login -> 7TV user/update subscription state
static CHANNEL_DETAILS: LazyLock<DashMap<String, SevenTvChannelSubscription>> =
    LazyLock::new(DashMap::new);

/// Channel login -> Twitch channel id, for channel-scoped entitlement
/// subscriptions. 7TV pushes `entitlement.create` for chatters present in the
/// channel, which is how other chatters' personal emote sets are learned
/// before their profile is fetched.
static CHANNEL_ENTITLEMENT_IDS: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);

/// 7TV user id -> all channel owners for user.update active-set changes.
static USER_TO_CHANNELS: LazyLock<DashMap<String, HashSet<String>>> = LazyLock::new(DashMap::new);

/// Queue of subscription changes to apply on the live connection (drained at
/// connect and by the connection's 250ms tick). A std Mutex so any thread or
/// runtime can push synchronously: an async lock would need a task on the
/// caller's runtime, and a short-lived fetch runtime can shut down before
/// that task runs, dropping the command.
static PENDING_SUBSCRIPTIONS: LazyLock<std::sync::Mutex<Vec<SubscriptionCommand>>> =
    LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Last known session_id from the 7TV EventAPI HELLO message (for RESUME on reconnect)
static LAST_SESSION_ID: LazyLock<tokio::sync::Mutex<Option<String>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

/// The subscriptions the server last ACKed. These are only carried into a
/// resumed session; a fresh session starts with no active subscriptions.
static RESUME_ACKED_SUBSCRIPTIONS: LazyLock<Mutex<HashSet<SubscriptionKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// A rejected resume or terminal stream means the next fresh connection must
/// reconcile every owned set because events may have been lost while offline.
static AUTHORITATIVE_RECONCILE_REQUIRED: AtomicBool = AtomicBool::new(false);

/// A command that was written but not ACKed has an unknowable server-side
/// outcome. Start fresh rather than risk a resumed duplicate subscription.
static RESUME_REQUIRES_FRESH_SESSION: AtomicBool = AtomicBool::new(false);

/// Error code 4007 is 7TV's slower reconnect class. Preserve Echo's existing
/// bounded supervisor while avoiding a tight one-second reconnect loop.
static SLOW_RECONNECT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set when the current connection received HELLO, so its failure restarts the backoff.
static CONNECTION_GREETED: AtomicBool = AtomicBool::new(false);
/// A connection that stayed up this long was healthy; its failure restarts the backoff.
const HEALTHY_CONNECTION_LIFETIME: Duration = Duration::from_secs(60);
/// A greeted connection must also survive this long before its failure
/// restarts the backoff, so a server that says HELLO and then errors backs off.
const GREETED_CONNECTION_MIN_LIFETIME: Duration = Duration::from_secs(15);

/// Coalesced refreshes keyed by emote set id. EventAPI can emit add/remove/update
/// bursts for one UI edit; Echo refreshes once per burst and then retokenizes once.
static PENDING_REFRESHES: LazyLock<Mutex<HashMap<String, PendingRefresh>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a coalescing refresh worker is already sleeping/draining.
static REFRESH_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);

static DIAGNOSTICS: LazyLock<Mutex<EventApiDiagnostics>> =
    LazyLock::new(|| Mutex::new(EventApiDiagnostics::default()));

static LAST_HEARTBEAT_AT_MS: AtomicU64 = AtomicU64::new(0);

/// channel_login -> latest requested active emote set. Used to drop stale
/// active-set refresh workers when 7TV emits rapid user.update changes.
static ACTIVE_SET_REFRESH_TARGETS: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);

/// Serialize authoritative live-set cache mutations. Fetches stay concurrent;
/// only the short apply/clear phase is ordered so an older worker cannot land
/// after a newer active-set transition.
static LIVE_SET_APPLY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Per-connection subscription cap announced by HELLO `subscription_limit`;
/// 0 means the server did not announce one.
static SUBSCRIPTION_LIMIT: AtomicU64 = AtomicU64::new(0);

/// Keys whose server-side subscription a manual reload asked to re-establish.
/// The live connection unsubscribes each active key once; the desired-state
/// flush then subscribes it again, or the server's "not subscribed" error
/// forces a fresh authoritative session.
static FORCED_RESUBSCRIBES: LazyLock<Mutex<HashSet<SubscriptionKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Last warn per drop site/object, so silent-drop diagnostics reach the
/// default WARN log without flooding it during a burst.
static DROP_WARN_LAST_AT: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const DROP_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Backoff before each retry of a failed authoritative set fetch.
const SET_FETCH_RETRY_DELAYS_SECS: [u64; 3] = [2, 10, 30];

#[derive(Clone, Debug)]
struct PendingRefresh {
    set_id: String,
    channel: String,
    is_global: bool,
    added: usize,
    removed: usize,
    updated: usize,
    requires_full_refresh: bool,
    patch: super::SevenTvEmoteSetPatch,
    notifications: Vec<EmoteSetNotification>,
    /// Authoritative fetch retries already spent on this refresh.
    fetch_attempt: usize,
}

#[derive(Clone, Default, Debug)]
struct DispatchSummary {
    added: usize,
    removed: usize,
    updated: usize,
}

#[derive(Default, Debug)]
struct DispatchChanges {
    summary: DispatchSummary,
    patch: super::SevenTvEmoteSetPatch,
    requires_full_refresh: bool,
    notifications: Vec<EmoteSetNotification>,
}

/// A single user-visible emote-set action. Keep this separate from cache
/// patches: rows are emitted only after the corresponding state is known good.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EmoteSetNotification {
    Added {
        actor: Option<String>,
        name: String,
    },
    Removed {
        actor: Option<String>,
        name: String,
    },
    Renamed {
        actor: Option<String>,
        old_name: String,
        new_name: String,
    },
}

impl EmoteSetNotification {
    fn format(&self) -> String {
        match self {
            Self::Added { actor, name } => {
                format_emote_set_notification(actor.as_deref(), "added", name)
            }
            Self::Removed { actor, name } => {
                format_emote_set_notification(actor.as_deref(), "removed", name)
            }
            Self::Renamed {
                actor,
                old_name,
                new_name,
            } => {
                let prefix = actor
                    .as_deref()
                    .map_or_else(|| "Renamed".to_string(), |actor| format!("{actor} renamed"));
                format!("{prefix} 7TV emote {old_name} to {new_name}.")
            }
        }
    }
}

fn format_emote_set_notification(actor: Option<&str>, action: &str, name: &str) -> String {
    let prefix = actor.map_or_else(
        || {
            let mut chars = action.chars();
            chars
                .next()
                .map(|first| format!("{}{}", first.to_ascii_uppercase(), chars.as_str()))
                .unwrap_or_default()
        },
        |actor| format!("{actor} {action}"),
    );
    format!("{prefix} 7TV emote {name}.")
}

#[derive(Debug)]
struct ConnectionSubscriptions {
    hello_received: bool,
    resume_session_id: Option<String>,
    resume_sent: bool,
    resume_pending: bool,
    resume_sent_at: Option<Instant>,
    active: HashSet<SubscriptionKey>,
    pending: Vec<PendingSubscriptionCommand>,
}

#[derive(Clone, Debug)]
struct PendingSubscriptionCommand {
    command: SubscriptionCommand,
    sent_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandGate {
    AwaitHello,
    AwaitResumeAck,
    Ready,
}

impl ConnectionSubscriptions {
    fn new(resume_session_id: Option<String>) -> Self {
        let active = if resume_session_id.is_some() {
            RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clone()
        } else {
            RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clear();
            HashSet::new()
        };
        Self {
            hello_received: false,
            resume_session_id,
            resume_sent: false,
            resume_pending: false,
            resume_sent_at: None,
            active,
            pending: Vec::new(),
        }
    }

    fn command_gate(&self) -> CommandGate {
        command_gate(self.hello_received, self.resume_pending)
    }

    fn active_count(&self) -> usize {
        self.active.len()
    }

    fn acknowledge(&mut self, command: &SubscriptionCommand) -> bool {
        if !acknowledge_subscription_command(&mut self.active, &mut self.pending, command) {
            return false;
        }
        *RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover() = self.active.clone();
        true
    }

    fn acknowledge_resume(&mut self) -> bool {
        if !self.resume_pending {
            return false;
        }
        self.resume_pending = false;
        self.resume_sent_at = None;
        *RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover() = self.active.clone();
        true
    }

    fn in_flight_count(&self) -> usize {
        in_flight_command_count(self.resume_pending, self.pending.len())
    }

    fn has_expired_ack_at(&self, now: Instant) -> bool {
        command_ack_expired_at(self.resume_pending, self.resume_sent_at, &self.pending, now)
    }

    fn require_fresh_session(&mut self) {
        RESUME_REQUIRES_FRESH_SESSION.store(true, Ordering::SeqCst);
        AUTHORITATIVE_RECONCILE_REQUIRED.store(true, Ordering::SeqCst);
        RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clear();
    }
}

fn in_flight_command_count(resume_pending: bool, pending_count: usize) -> usize {
    pending_count + usize::from(resume_pending)
}

fn command_ack_expired_at(
    resume_pending: bool,
    resume_sent_at: Option<Instant>,
    pending: &[PendingSubscriptionCommand],
    now: Instant,
) -> bool {
    (resume_pending
        && resume_sent_at.is_some_and(|sent_at| now.duration_since(sent_at) >= COMMAND_ACK_TIMEOUT))
        || pending
            .iter()
            .any(|pending| now.duration_since(pending.sent_at) >= COMMAND_ACK_TIMEOUT)
}

fn connection_drop_requires_fresh(resume_pending: bool, pending_count: usize) -> bool {
    resume_pending || pending_count > 0
}

fn connection_drop_requeues_unacked(shutdown_requested: bool) -> bool {
    !shutdown_requested
}

fn command_error_requires_fresh(in_flight_count: usize) -> bool {
    in_flight_count > 0
}

fn command_gate(hello_received: bool, resume_pending: bool) -> CommandGate {
    if !hello_received {
        CommandGate::AwaitHello
    } else if resume_pending {
        CommandGate::AwaitResumeAck
    } else {
        CommandGate::Ready
    }
}

fn acknowledge_subscription_command(
    active: &mut HashSet<SubscriptionKey>,
    pending: &mut Vec<PendingSubscriptionCommand>,
    command: &SubscriptionCommand,
) -> bool {
    let Some(index) = pending
        .iter()
        .position(|pending| pending.command == *command)
    else {
        return false;
    };
    pending.remove(index);
    match command {
        SubscriptionCommand::Subscribe(key) => {
            active.insert(key.clone());
        }
        SubscriptionCommand::Unsubscribe(key) => {
            active.remove(key);
        }
    }
    true
}

impl Drop for ConnectionSubscriptions {
    fn drop(&mut self) {
        let shutdown_requested = SHUTDOWN_REQUESTED.load(Ordering::SeqCst);
        if connection_drop_requires_fresh(self.resume_pending, self.pending.len()) {
            self.require_fresh_session();
        }
        if connection_drop_requeues_unacked(shutdown_requested) {
            for pending in self.pending.drain(..) {
                queue_subscription_command(pending.command);
            }
        }
        update_diagnostics(|diag| {
            diag.active_subscriptions = 0;
            diag.in_flight_subscription_commands = 0;
        });
    }
}

#[derive(Debug)]
struct ConnectionHealth {
    heartbeat_interval: Duration,
    last_heartbeat: Instant,
}

impl ConnectionHealth {
    fn new() -> Self {
        Self {
            heartbeat_interval: Duration::from_millis(DEFAULT_HEARTBEAT_INTERVAL_MS),
            last_heartbeat: Instant::now(),
        }
    }

    fn update_interval_ms(&mut self, interval_ms: u64) {
        self.heartbeat_interval = Duration::from_millis(interval_ms.max(1_000));
        self.last_heartbeat = Instant::now();
    }

    fn mark_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    fn timeout_after(&self) -> Duration {
        self.heartbeat_interval
            .saturating_mul(HEARTBEAT_TIMEOUT_MULTIPLIER)
    }

    fn timeout_remaining(&self) -> Duration {
        self.timeout_after()
            .saturating_sub(self.last_heartbeat.elapsed())
            .max(Duration::from_millis(100))
    }

    fn is_timed_out(&self) -> bool {
        self.last_heartbeat.elapsed() >= self.timeout_after()
    }
}

/// Start the 7TV EventAPI background task.
/// Safe to call multiple times - only one task runs.
pub fn start_seventv_event_api() {
    if crate::backend::reliability::is_safe_mode() {
        crate::log_emotes!(
            info,
            "7TV EventAPI startup skipped because safe mode is active"
        );
        return;
    }

    let replay_only = start_eventapi_replay_from_env();
    if replay_only {
        crate::log_emotes!(
            info,
            "7TV EventAPI: replay-only mode enabled, skipping websocket"
        );
        return;
    }

    if RUNNING.swap(true, Ordering::SeqCst) {
        crate::log_emotes!(debug, "7TV EventAPI already running");
        return;
    }
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);

    crate::log_emotes!(info, "Starting 7TV EventAPI WebSocket client");

    let known_subscriptions = desired_subscriptions();

    if !known_subscriptions.is_empty() {
        crate::log_emotes!(
            info,
            "7TV EventAPI: queuing {} pre-known subscriptions",
            known_subscriptions.len()
        );
        PENDING_SUBSCRIPTIONS.lock_or_recover().extend(
            known_subscriptions
                .into_iter()
                .map(SubscriptionCommand::Subscribe),
        );
    }

    super::spawn_runtime_thread("7tv-eventapi", &RUNNING, run_supervisor);
}

pub fn shutdown() {
    let start = Instant::now();
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    crate::log_emotes!(
        info,
        elapsed_ms = start.elapsed().as_millis(),
        "7TV EventAPI shutdown requested"
    );
}

/// Restart the live EventAPI socket so proxy setting changes apply to new connections.
pub fn restart_for_proxy_change() {
    restart("proxy settings change");
}

/// Replace the EventAPI socket, which cannot outlive a system sleep.
///
/// Unlike a proxy change this never starts a stopped or skipped EventAPI.
pub fn restart_after_system_wake() {
    if RUNNING.load(Ordering::SeqCst) {
        restart("system wake");
    }
}

fn restart(reason: &'static str) {
    if crate::backend::reliability::is_safe_mode() {
        crate::log_emotes!(
            info,
            reason,
            "7TV EventAPI restart skipped because safe mode is active"
        );
        return;
    }

    if replay_only() {
        crate::log_emotes!(
            debug,
            reason,
            "7TV EventAPI restart skipped because replay-only mode is active"
        );
        return;
    }

    if !RUNNING.load(Ordering::SeqCst) {
        start_seventv_event_api();
        return;
    }

    if RESTART_QUEUED.swap(true, Ordering::SeqCst) {
        return;
    }

    shutdown();
    let spawn_result = std::thread::Builder::new()
        .name("7tv-eventapi-restart".into())
        .spawn(move || {
            for _ in 0..40 {
                if crate::backend::is_shutdown_started() {
                    RESTART_QUEUED.store(false, Ordering::SeqCst);
                    return;
                }
                if !RUNNING.load(Ordering::SeqCst) {
                    RESTART_QUEUED.store(false, Ordering::SeqCst);
                    start_seventv_event_api();
                    crate::log_emotes!(info, reason, "7TV EventAPI restarted");
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }

            RESTART_QUEUED.store(false, Ordering::SeqCst);
            crate::log_emotes!(
                warn,
                reason,
                "7TV EventAPI restart timed out waiting for existing socket to stop"
            );
        });

    if let Err(error) = spawn_result {
        RESTART_QUEUED.store(false, Ordering::SeqCst);
        crate::log_emotes!(
            warn,
            "Failed to spawn 7TV EventAPI restart worker: {}",
            error
        );
    }
}

/// Register a channel's 7TV emote set ID for real-time updates.
/// Called from emotes/mod.rs when a channel's emote set ID is discovered.
pub fn subscribe_channel_set(channel_login: &str, set_id: &str) {
    if set_id.is_empty() {
        return;
    }
    if crate::backend::irc::is_channel_closed(channel_login) {
        crate::log_emotes!(
            debug,
            "7TV EventAPI: not registering set {} for closed channel {}",
            set_id,
            channel_login
        );
        return;
    }

    let channel = super::normalize_channel_key(channel_login);
    let set = set_id.to_string();

    if let Some(old_set) = CHANNEL_TO_SET.insert(channel.clone(), set.clone())
        && old_set != set
    {
        remove_set_channel_mapping(&old_set, &channel);
        crate::log_emotes!(
            info,
            "7TV EventAPI: channel '{}' set changed {} -> {}",
            channel,
            old_set,
            set
        );
        if !set_is_desired(&old_set) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(old_set),
            ));
        }
    }
    add_set_channel_mapping(&set, &channel);

    crate::log_emotes!(
        debug,
        "7TV EventAPI: registered set {} for channel subscription",
        set_id
    );

    queue_subscription_command(SubscriptionCommand::Subscribe(SubscriptionKey::emote_set(
        set,
    )));
}

/// Manual-reload heal: ask the live connection to re-establish the channel's
/// emote-set and user subscriptions even when Echo believes they are active,
/// and restart a stopped EventAPI client.
pub(crate) fn resubscribe_channel(channel_login: &str) {
    let channel = super::normalize_channel_key(channel_login);
    let set_id = CHANNEL_TO_SET
        .get(&channel)
        .map(|set_id| set_id.clone())
        .filter(|set_id| !set_id.is_empty());
    let mut keys = Vec::new();
    if let Some(set_id) = &set_id {
        keys.push(SubscriptionKey::emote_set(set_id.clone()));
    }
    if let Some(user_id) = CHANNEL_DETAILS
        .get(&channel)
        .map(|details| details.user_id.clone())
        .filter(|user_id| !user_id.is_empty())
    {
        keys.push(SubscriptionKey::user(user_id));
    }
    crate::log_emotes!(
        info,
        "7TV EventAPI: manual reload re-establishing {} subscription(s) for #{} (set {:?}, connected={})",
        keys.len(),
        channel,
        set_id,
        DIAGNOSTICS.lock_or_recover().connected
    );
    FORCED_RESUBSCRIBES.lock_or_recover().extend(keys);
    if !RUNNING.load(Ordering::SeqCst) && !replay_only() {
        start_seventv_event_api();
    }
}

pub(crate) fn clear_channel_set_subscription(channel_login: &str) {
    let channel = super::normalize_channel_key(channel_login);
    if let Some((_, set_id)) = CHANNEL_TO_SET.remove(&channel) {
        remove_set_channel_mapping(&set_id, &channel);
        if !set_is_desired(&set_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(set_id),
            ));
        }
    }
}

fn remove_channel_set_owner_if_matches(channel_login: &str, set_id: &str) -> bool {
    let channel = super::normalize_channel_key(channel_login);
    let removed = CHANNEL_TO_SET
        .remove_if(&channel, |_, current| current == set_id)
        .is_some();
    if removed {
        remove_set_channel_mapping(set_id, &channel);
        if !set_is_desired(set_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(set_id.to_string()),
            ));
        }
    }
    removed
}

fn remove_global_set_owner_if_matches(set_id: &str) -> bool {
    let removed = {
        let mut current = GLOBAL_SET_OWNER.lock_or_recover();
        if current.as_deref() == Some(set_id) {
            current.take();
            true
        } else {
            false
        }
    };
    if removed {
        remove_set_channel_mapping(set_id, GLOBAL_SET_CHANNEL);
        if !set_is_desired(set_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(set_id.to_string()),
            ));
        }
    }
    removed
}

fn retain_channel_user_subscription_without_set(
    channel_login: &str,
    mut details: SevenTvChannelSubscription,
    allowed_set_ids: &[&str],
) -> bool {
    let channel = super::normalize_channel_key(channel_login);
    if let Some(existing) = CHANNEL_DETAILS.get(&channel) {
        let identity_matches = existing.user_id == details.user_id
            && existing.connection_index == details.connection_index;
        let set_matches = existing.emote_set_id.is_empty()
            || allowed_set_ids
                .iter()
                .any(|set_id| existing.emote_set_id == *set_id);
        if !identity_matches || !set_matches {
            return false;
        }
    }

    details.emote_set_id.clear();
    CHANNEL_DETAILS.insert(channel.clone(), details.clone());
    add_user_channel_mapping(&details.user_id, &channel);
    queue_subscription_command(SubscriptionCommand::Subscribe(SubscriptionKey::user(
        details.user_id,
    )));
    true
}

/// Apply a terminal exact-set result from the ordinary channel fetch path.
/// An unowned channel is safe to clear; a different current owner makes this
/// response stale. User identity remains registered with an empty active set so
/// a later `user.update` can recover the channel without polling.
pub(crate) fn retire_terminal_channel_fetch(
    channel_login: &str,
    set_id: &str,
    details: Option<SevenTvChannelSubscription>,
) -> bool {
    let channel = super::normalize_channel_key(channel_login);
    if ACTIVE_SET_REFRESH_TARGETS
        .get(&channel)
        .is_some_and(|target| target.value() != set_id)
        || CHANNEL_TO_SET
            .get(&channel)
            .is_some_and(|current| current.value() != set_id)
    {
        return false;
    }

    if let Some(details) = details {
        if !retain_channel_user_subscription_without_set(&channel, details, &[set_id]) {
            return false;
        }
    } else {
        if let Some(mut current) = CHANNEL_DETAILS.get_mut(&channel) {
            if current.emote_set_id == set_id {
                current.emote_set_id.clear();
            } else if !current.emote_set_id.is_empty() {
                return false;
            }
        }
    }

    remove_channel_set_owner_if_matches(&channel, set_id);
    true
}

fn retire_terminal_active_set(
    channel_login: &str,
    old_set_id: &str,
    terminal_set_id: &str,
    details: SevenTvChannelSubscription,
) -> bool {
    let channel = super::normalize_channel_key(channel_login);
    if !active_set_refresh_is_current(&channel, terminal_set_id)
        || CHANNEL_TO_SET.get(&channel).is_some_and(|current| {
            current.value() != old_set_id && current.value() != terminal_set_id
        })
    {
        return false;
    }

    if !retain_channel_user_subscription_without_set(
        &channel,
        details,
        &[old_set_id, terminal_set_id],
    ) {
        return false;
    }

    if !old_set_id.is_empty() {
        remove_channel_set_owner_if_matches(&channel, old_set_id);
    }
    remove_channel_set_owner_if_matches(&channel, terminal_set_id);
    true
}

fn retire_terminal_refresh_owner(channel_login: &str, set_id: &str, is_global: bool) -> bool {
    if is_global {
        remove_global_set_owner_if_matches(set_id)
    } else {
        let removed = remove_channel_set_owner_if_matches(channel_login, set_id);
        if removed
            && channel_login != super::PERSONAL_SEVENTV_CHANNEL
            && let Some(mut details) = CHANNEL_DETAILS.get_mut(channel_login)
            && details.emote_set_id == set_id
        {
            details.emote_set_id.clear();
        }
        removed
    }
}

/// Register a channel's owning 7TV user and active emote set.
///
/// Subscribes to `user.update` in addition to `emote_set.update` so
/// active-set switches are detected immediately.
pub(crate) fn subscribe_channel_details(channel_login: &str, details: SevenTvChannelSubscription) {
    if crate::backend::irc::is_channel_closed(channel_login) {
        return;
    }
    if details.user_id.is_empty() {
        subscribe_channel_set(channel_login, &details.emote_set_id);
        return;
    }

    let channel = super::normalize_channel_key(channel_login);

    if let Some(old_details) = CHANNEL_DETAILS.insert(channel.clone(), details.clone())
        && old_details.user_id != details.user_id
    {
        remove_user_channel_mapping(&old_details.user_id, &channel);
        if !user_is_desired(&old_details.user_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(SubscriptionKey::user(
                old_details.user_id,
            )));
        }
    }

    add_user_channel_mapping(&details.user_id, &channel);
    queue_subscription_command(SubscriptionCommand::Subscribe(SubscriptionKey::user(
        details.user_id.clone(),
    )));

    if details.emote_set_id.is_empty() {
        clear_channel_set_subscription(&channel);
    } else {
        subscribe_channel_set(&channel, &details.emote_set_id);
    }

    crate::log_emotes!(
        debug,
        "7TV EventAPI: registered user {} connection {} for channel {} set {}",
        details.user_id,
        details.connection_index,
        channel,
        details.emote_set_id
    );
}

/// Register the 7TV global emote set ID for real-time updates.
/// Called from emotes/mod.rs when the global set ID is discovered.
pub fn subscribe_global_set(set_id: &str) {
    if set_id.is_empty() {
        return;
    }
    let set = set_id.to_string();

    let old_set = {
        let mut current = GLOBAL_SET_OWNER.lock_or_recover();
        current.replace(set.clone())
    };
    if let Some(old_set) = old_set.filter(|old_set| old_set != &set) {
        remove_set_channel_mapping(&old_set, GLOBAL_SET_CHANNEL);
        if !set_is_desired(&old_set) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(old_set),
            ));
        }
    }

    add_set_channel_mapping(&set, GLOBAL_SET_CHANNEL);

    crate::log_emotes!(
        info,
        "7TV EventAPI: registered global set {} for subscription",
        set_id
    );

    queue_subscription_command(SubscriptionCommand::Subscribe(SubscriptionKey::emote_set(
        set,
    )));
}

/// Remove all EventAPI ownership for a channel that is no longer active.
pub fn unsubscribe_channel(channel_login: &str) {
    let channel = super::normalize_channel_key(channel_login);

    if let Some((_, details)) = CHANNEL_DETAILS.remove(&channel) {
        remove_user_channel_mapping(&details.user_id, &channel);
        if !user_is_desired(&details.user_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(SubscriptionKey::user(
                details.user_id,
            )));
        }
    }

    if let Some((_, set_id)) = CHANNEL_TO_SET.remove(&channel) {
        remove_set_channel_mapping(&set_id, &channel);
        if !set_is_desired(&set_id) {
            queue_subscription_command(SubscriptionCommand::Unsubscribe(
                SubscriptionKey::emote_set(set_id),
            ));
        }
    }

    if let Some((_, twitch_channel_id)) = CHANNEL_ENTITLEMENT_IDS.remove(&channel) {
        unsubscribe_entitlements_if_unused(&twitch_channel_id);
    }

    crate::log_emotes!(
        debug,
        "7TV EventAPI: removed channel subscriptions for {}",
        channel
    );
}

pub(crate) fn diagnostics() -> EventApiDiagnostics {
    let mut diagnostics = DIAGNOSTICS.lock_or_recover().clone();
    let last_heartbeat = LAST_HEARTBEAT_AT_MS.load(Ordering::Relaxed);
    if last_heartbeat > 0 {
        diagnostics.last_heartbeat_ms_ago =
            Some(crate::utils::unix_now_ms().saturating_sub(last_heartbeat));
    }
    diagnostics.desired_subscriptions = desired_subscriptions().len();
    diagnostics.queued_subscription_commands = PENDING_SUBSCRIPTIONS.lock_or_recover().len();
    diagnostics.pending_refreshes = PENDING_REFRESHES.lock_or_recover().len();
    diagnostics.active_set_refreshes = ACTIVE_SET_REFRESH_TARGETS.len();
    diagnostics.owned_channels = CHANNEL_TO_SET.len();
    diagnostics.owned_sets = SET_TO_CHANNELS.len();
    diagnostics.owned_users = USER_TO_CHANNELS.len();
    diagnostics
}

/// Whether a channel's registered 7TV set is covered by the live EventAPI stream.
///
/// Channel fetch caching can use the normal emote-cache window only after the
/// authoritative registry is fully subscribed. Disconnects, queued subscription
/// changes, and active-set transitions fall back to short polling automatically.
pub(crate) fn channel_updates_are_live(channel_login: &str) -> bool {
    let channel = super::normalize_channel_key(channel_login);
    if channel.is_empty()
        || !CHANNEL_TO_SET.contains_key(&channel)
        || ACTIVE_SET_REFRESH_TARGETS.contains_key(&channel)
    {
        return false;
    }

    let diagnostics = diagnostics();
    let Some(set_id) = CHANNEL_TO_SET.get(&channel).map(|set_id| set_id.clone()) else {
        return false;
    };
    subscription_coverage_is_live(
        diagnostics.connected,
        diagnostics.queued_subscription_commands,
        diagnostics.in_flight_subscription_commands,
        &RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover(),
        &set_id,
    )
}

fn active_subscriptions_cover_set(active: &HashSet<SubscriptionKey>, set_id: &str) -> bool {
    !set_id.is_empty() && active.contains(&SubscriptionKey::emote_set(set_id))
}

fn subscription_coverage_is_live(
    connected: bool,
    queued_commands: usize,
    in_flight_commands: usize,
    active: &HashSet<SubscriptionKey>,
    set_id: &str,
) -> bool {
    connected
        && queued_commands == 0
        && in_flight_commands == 0
        && active_subscriptions_cover_set(active, set_id)
}

pub(crate) fn ownership_snapshot() -> EventApiOwnershipSnapshot {
    let mut channels = Vec::new();
    for entry in CHANNEL_TO_SET.iter() {
        let channel = entry.key().clone();
        let details = CHANNEL_DETAILS.get(&channel).map(|details| details.clone());
        channels.push(EventApiChannelOwnership {
            channel,
            emote_set_id: entry.value().clone(),
            user_id: details.as_ref().map(|details| details.user_id.clone()),
            connection_index: details.as_ref().map(|details| details.connection_index),
        });
    }
    channels.sort_by(|a, b| a.channel.cmp(&b.channel));

    let mut sets = SET_TO_CHANNELS
        .iter()
        .map(|entry| {
            let mut owners = entry.value().iter().cloned().collect::<Vec<_>>();
            owners.sort();
            EventApiSetOwnership {
                emote_set_id: entry.key().clone(),
                channels: owners,
            }
        })
        .collect::<Vec<_>>();
    sets.sort_by(|a, b| a.emote_set_id.cmp(&b.emote_set_id));

    let mut users = USER_TO_CHANNELS
        .iter()
        .map(|entry| {
            let mut owners = entry.value().iter().cloned().collect::<Vec<_>>();
            owners.sort();
            EventApiUserOwnership {
                user_id: entry.key().clone(),
                channels: owners,
            }
        })
        .collect::<Vec<_>>();
    users.sort_by(|a, b| a.user_id.cmp(&b.user_id));

    let mut active_set_refreshes = ACTIVE_SET_REFRESH_TARGETS
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect::<Vec<_>>();
    active_set_refreshes.sort_by(|a, b| a.0.cmp(&b.0));

    EventApiOwnershipSnapshot {
        channels,
        sets,
        users,
        active_set_refreshes,
    }
}

pub fn start_eventapi_replay_from_env() -> bool {
    let Some(path) = echo_core::env::value("ECHO_7TV_EVENTAPI_REPLAY").map(PathBuf::from) else {
        return false;
    };

    let replay_only = replay_only();
    if REPLAY_RUNNING.swap(true, Ordering::SeqCst) {
        return replay_only;
    }

    if let Some(mappings) = echo_core::env::value("ECHO_7TV_EVENTAPI_REPLAY_MAPPINGS") {
        let mut applied = 0usize;
        for mapping in mappings
            .split([',', ';'])
            .map(str::trim)
            .filter(|mapping| !mapping.is_empty())
        {
            if let Some((set_id, channel)) = mapping.split_once(':') {
                apply_replay_mapping(set_id.trim(), channel.trim());
                applied += 1;
            } else {
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI replay: ignored invalid mapping '{}'; expected set_id:channel",
                    mapping
                );
            }
        }
        crate::log_emotes!(
            info,
            "7TV EventAPI replay: applied {} set/channel mapping(s)",
            applied
        );
    } else if let (Some(set_id), Some(channel)) = (
        echo_core::env::value("ECHO_7TV_EVENTAPI_REPLAY_SET_ID"),
        echo_core::env::value("ECHO_7TV_EVENTAPI_REPLAY_CHANNEL"),
    ) {
        apply_replay_mapping(&set_id, &channel);
    } else {
        crate::log_emotes!(
            warn,
            "7TV EventAPI replay: set/channel env mapping not provided; fixture set IDs must already be registered"
        );
    }

    let delay_ms =
        echo_core::env::parsed::<u64>("ECHO_7TV_EVENTAPI_REPLAY_DELAY_MS").unwrap_or(1500);

    let spawn_result = std::thread::Builder::new()
        .name("7tv-eventapi-replay".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::log_emotes!(
                        error,
                        "7TV EventAPI replay: failed to create runtime: {}",
                        e
                    );
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = replay_eventapi_path(&path).await {
                    crate::log_emotes!(error, "7TV EventAPI replay failed: {}", e);
                }
            });
        });

    if let Err(e) = spawn_result {
        REPLAY_RUNNING.store(false, Ordering::SeqCst);
        crate::log_emotes!(
            error,
            "7TV EventAPI replay: failed to spawn replay thread: {}",
            e
        );
    }

    replay_only
}

fn apply_replay_mapping(set_id: &str, channel: &str) {
    if set_id.is_empty() || channel.is_empty() {
        return;
    }

    if channel.eq_ignore_ascii_case("global") || channel == GLOBAL_SET_CHANNEL {
        subscribe_global_set(set_id);
        return;
    }

    if let Some(user_id) = echo_core::env::value("ECHO_7TV_EVENTAPI_REPLAY_USER_ID") {
        let connection_index =
            echo_core::env::parsed("ECHO_7TV_EVENTAPI_REPLAY_CONNECTION_INDEX").unwrap_or(0);
        subscribe_channel_details(
            channel,
            SevenTvChannelSubscription {
                user_id,
                emote_set_id: set_id.to_string(),
                connection_index,
            },
        );
    } else {
        subscribe_channel_set(channel, set_id);
    }
}

/// Desired subscriptions from the authoritative channel/user registry.
fn desired_subscriptions() -> Vec<SubscriptionKey> {
    let mut subscriptions = HashSet::new();
    for entry in SET_TO_CHANNELS.iter() {
        if !entry.key().is_empty() {
            subscriptions.insert(SubscriptionKey::emote_set(entry.key().clone()));
        }
    }
    for entry in CHANNEL_TO_SET.iter() {
        let set_id = entry.value();
        if !set_id.is_empty() {
            subscriptions.insert(SubscriptionKey::emote_set(set_id.clone()));
        }
    }
    for entry in CHANNEL_DETAILS.iter() {
        let details = entry.value();
        if !details.user_id.is_empty() {
            subscriptions.insert(SubscriptionKey::user(details.user_id.clone()));
        }
    }
    for entry in CHANNEL_ENTITLEMENT_IDS.iter() {
        subscriptions.extend(SubscriptionKey::channel_entitlements(entry.value()));
    }
    subscriptions.into_iter().collect()
}

fn unsubscribe_entitlements_if_unused(twitch_channel_id: &str) {
    if CHANNEL_ENTITLEMENT_IDS
        .iter()
        .any(|entry| entry.value() == twitch_channel_id)
    {
        return;
    }
    for key in SubscriptionKey::channel_entitlements(twitch_channel_id) {
        queue_subscription_command(SubscriptionCommand::Unsubscribe(key));
    }
}

/// Subscribe to 7TV entitlement events for an open channel, identified by its
/// Twitch channel id (from ROOMSTATE).
pub(crate) fn subscribe_channel_entitlements(channel_login: &str, twitch_channel_id: &str) {
    let twitch_channel_id = twitch_channel_id.trim();
    if twitch_channel_id.is_empty() || crate::backend::irc::is_channel_closed(channel_login) {
        return;
    }
    let channel = super::normalize_channel_key(channel_login);
    if CHANNEL_ENTITLEMENT_IDS
        .get(&channel)
        .is_some_and(|current| current.value() == twitch_channel_id)
    {
        return;
    }
    if let Some(old_id) = CHANNEL_ENTITLEMENT_IDS.insert(channel, twitch_channel_id.to_string()) {
        unsubscribe_entitlements_if_unused(&old_id);
    }
    for key in SubscriptionKey::channel_entitlements(twitch_channel_id) {
        queue_subscription_command(SubscriptionCommand::Subscribe(key));
    }
}

fn set_is_desired(set_id: &str) -> bool {
    SET_TO_CHANNELS
        .get(set_id)
        .is_some_and(|entry| !entry.value().is_empty())
        || CHANNEL_TO_SET.iter().any(|entry| entry.value() == set_id)
}

fn channel_maps_to_set(channel: &str, set_id: &str) -> bool {
    let channel = super::normalize_channel_key(channel);
    if channel == GLOBAL_SET_CHANNEL {
        return GLOBAL_SET_OWNER.lock_or_recover().as_deref() == Some(set_id);
    }
    if let Some(target) = ACTIVE_SET_REFRESH_TARGETS.get(&channel) {
        return target.value() == set_id;
    }
    CHANNEL_TO_SET
        .get(&channel)
        .is_some_and(|current| current.value() == set_id)
}

fn schedule_authoritative_reconcile_after_session_loss() {
    let mut targets = HashSet::new();
    let assignment_targets = CHANNEL_DETAILS
        .iter()
        .filter_map(|entry| {
            let channel = entry.key().clone();
            let details = entry.value().clone();
            if channel.is_empty()
                || channel == GLOBAL_SET_CHANNEL
                || channel == super::PERSONAL_SEVENTV_CHANNEL
                || details.user_id.is_empty()
                || ACTIVE_SET_REFRESH_TARGETS.contains_key(&channel)
            {
                return None;
            }

            Some((channel, details))
        })
        .collect::<Vec<_>>();

    for entry in CHANNEL_TO_SET.iter() {
        let channel = entry.key().clone();
        if ACTIVE_SET_REFRESH_TARGETS.contains_key(&channel) {
            continue;
        }
        let set_id = entry.value().clone();
        if !set_id.is_empty() {
            targets.insert((set_id, channel, false));
        }
    }

    for entry in SET_TO_CHANNELS.iter() {
        if entry.value().contains(GLOBAL_SET_CHANNEL) {
            targets.insert((entry.key().clone(), GLOBAL_SET_CHANNEL.to_string(), true));
        }
    }

    if targets.is_empty() && assignment_targets.is_empty() {
        crate::log_emotes!(
            debug,
            "7TV EventAPI: fresh session has no owned emote sets or channel assignments to reconcile"
        );
        return;
    }

    let target_count = targets.len();
    for (set_id, channel, is_global) in targets {
        schedule_refresh_after_dispatch(
            &set_id,
            &channel,
            is_global,
            DispatchChanges {
                requires_full_refresh: true,
                ..DispatchChanges::default()
            },
        );
    }

    let mut assignment_target_count = 0usize;
    for (channel, details) in assignment_targets {
        if schedule_channel_assignment_reconcile_after_session_loss(channel, details) {
            assignment_target_count += 1;
        }
    }
    crate::backend::metrics::record_emote_event("7tv.eventapi.fresh_session_reconcile_queued");
    crate::log_emotes!(
        info,
        "7TV EventAPI: queued authoritative reconciliation for {} owned set target(s) and {} channel assignment(s) after a lost session",
        target_count,
        assignment_target_count
    );
}

fn schedule_channel_assignment_reconcile_after_session_loss(
    channel: String,
    details: SevenTvChannelSubscription,
) -> bool {
    if channel == GLOBAL_SET_CHANNEL
        || channel == super::PERSONAL_SEVENTV_CHANNEL
        || details.user_id.is_empty()
    {
        return false;
    }

    crate::backend::metrics::record_emote_event(
        "7tv.eventapi.fresh_session_channel_assignment_reconcile_queued",
    );
    crate::backend::bridge::get_runtime().spawn(async move {
        reconcile_channel_assignment_after_session_loss(channel, details).await;
    });
    true
}

async fn reconcile_channel_assignment_after_session_loss(
    channel: String,
    details: SevenTvChannelSubscription,
) {
    if !channel_assignment_reconcile_is_current(&channel, &details) {
        crate::backend::metrics::record_emote_event(
            "7tv.eventapi.fresh_session_channel_assignment_reconcile_stale",
        );
        return;
    }

    let channel_id = resolve_channel_id_for_assignment_reconcile(&channel).await;
    let Some(channel_id) = channel_id else {
        crate::backend::metrics::record_emote_event(
            "7tv.eventapi.fresh_session_channel_assignment_reconcile_failed",
        );
        crate::log_emotes!(
            debug,
            "7TV EventAPI: could not resolve Twitch channel ID for fresh-session assignment reconcile of #{}",
            channel
        );
        return;
    };

    super::invalidate_seventv_fetch_cache_for_event(Some(&channel), false);
    match super::EMOTE_CACHE
        .refresh_channel_emotes(&channel_id, &channel)
        .await
    {
        Ok(()) => {
            if ACTIVE_SET_REFRESH_TARGETS.contains_key(&channel) {
                crate::backend::metrics::record_emote_event(
                    "7tv.eventapi.fresh_session_channel_assignment_reconcile_stale",
                );
            } else {
                crate::backend::metrics::record_emote_event(
                    "7tv.eventapi.fresh_session_channel_assignment_reconcile_completed",
                );
            }
        }
        Err(error) => {
            crate::backend::metrics::record_emote_event(
                "7tv.eventapi.fresh_session_channel_assignment_reconcile_failed",
            );
            crate::log_emotes!(
                warn,
                "7TV EventAPI: fresh-session assignment reconcile failed for #{}: {}",
                channel,
                error
            );
        }
    }
}

async fn resolve_channel_id_for_assignment_reconcile(channel: &str) -> Option<String> {
    if let Some(channel_id) = crate::backend::twitch_api::get_cached_channel_id(channel) {
        return Some(channel_id);
    }

    let token = crate::backend::settings::get_token()?;
    match crate::backend::twitch_api::get_user_id_helix(channel, &token).await {
        Ok(channel_id) => {
            crate::backend::twitch_api::cache_channel_id(channel, &channel_id);
            Some(channel_id)
        }
        Err(error) => {
            crate::log_emotes!(
                debug,
                "7TV EventAPI: failed to resolve Twitch channel ID for fresh-session assignment reconcile of #{}: {}",
                channel,
                error
            );
            None
        }
    }
}

fn channel_assignment_reconcile_is_current(
    channel: &str,
    expected_details: &SevenTvChannelSubscription,
) -> bool {
    !ACTIVE_SET_REFRESH_TARGETS.contains_key(channel)
        && CHANNEL_DETAILS.get(channel).is_some_and(|details| {
            details.user_id == expected_details.user_id
                && details.connection_index == expected_details.connection_index
                && details.emote_set_id == expected_details.emote_set_id
        })
}

fn user_is_desired(user_id: &str) -> bool {
    USER_TO_CHANNELS
        .get(user_id)
        .is_some_and(|entry| !entry.value().is_empty())
        || CHANNEL_DETAILS
            .iter()
            .any(|entry| entry.value().user_id == user_id)
}

fn get_channels_for_set(set_id: &str) -> Vec<String> {
    SET_TO_CHANNELS
        .get(set_id)
        .map(|owners| {
            owners
                .iter()
                .filter(|channel| !channel.is_empty())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn get_channels_for_user(user_id: &str) -> Vec<String> {
    USER_TO_CHANNELS
        .get(user_id)
        .map(|owners| owners.iter().cloned().collect())
        .unwrap_or_else(|| {
            CHANNEL_DETAILS
                .iter()
                .filter(|entry| entry.value().user_id == user_id)
                .map(|entry| entry.key().clone())
                .collect()
        })
}

fn add_set_channel_mapping(set_id: &str, channel: &str) {
    let mut owners = SET_TO_CHANNELS.entry(set_id.to_string()).or_default();
    owners.insert(channel.to_string());
}

fn add_user_channel_mapping(user_id: &str, channel: &str) {
    let mut owners = USER_TO_CHANNELS.entry(user_id.to_string()).or_default();
    owners.insert(channel.to_string());
}

fn remove_set_channel_mapping(set_id: &str, channel: &str) {
    SET_TO_CHANNELS.remove_if_mut(set_id, |_, owners| {
        owners.remove(channel);
        owners.is_empty()
    });
}

fn remove_user_channel_mapping(user_id: &str, channel: &str) {
    let remove_user = {
        match USER_TO_CHANNELS.get_mut(user_id) {
            Some(mut owners) => {
                owners.remove(channel);
                owners.is_empty()
            }
            _ => false,
        }
    };

    if remove_user {
        USER_TO_CHANNELS.remove(user_id);
    }
}

fn channel_details_for_user_connection(
    user_id: &str,
    connection_index: usize,
) -> Vec<(String, SevenTvChannelSubscription)> {
    CHANNEL_DETAILS
        .iter()
        .filter(|entry| {
            let details = entry.value();
            details.user_id == user_id && details.connection_index == connection_index
        })
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect()
}

fn queue_subscription_command(command: SubscriptionCommand) {
    let mut pending = PENDING_SUBSCRIPTIONS.lock_or_recover();
    if !pending.contains(&command) {
        pending.push(command);
    }
}

fn update_diagnostics(update: impl FnOnce(&mut EventApiDiagnostics)) {
    let mut diagnostics = DIAGNOSTICS.lock_or_recover();
    update(&mut diagnostics);
}
fn next_reconnect_delay_ms(current_ms: u64, lifetime: Duration, greeted: bool, slow: bool) -> u64 {
    let base_ms = if slow {
        RECONNECT_SLOW_BASE_MS
    } else {
        RECONNECT_BASE_MS
    };
    let healthy = lifetime >= HEALTHY_CONNECTION_LIFETIME
        || (greeted && lifetime >= GREETED_CONNECTION_MIN_LIFETIME);
    if healthy {
        base_ms
    } else {
        current_ms.max(base_ms).min(RECONNECT_MAX_MS)
    }
}

async fn run_supervisor() {
    let mut delay_ms = RECONNECT_BASE_MS;
    let mut attempt = 0u32;

    loop {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            update_diagnostics(|diag| {
                diag.connected = false;
            });
            crate::log_emotes!(info, "7TV EventAPI: supervisor stopped by shutdown");
            break;
        }

        if attempt > 0 {
            update_diagnostics(|diag| {
                diag.connected = false;
                diag.reconnects = diag.reconnects.saturating_add(1);
            });
            crate::log_emotes!(
                info,
                "7TV EventAPI: reconnecting in {}ms (attempt {})",
                delay_ms,
                attempt
            );
            sleep(crate::backend::retry::with_reconnect_jitter(
                Duration::from_millis(delay_ms),
            ))
            .await;
            if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
                continue;
            }
            delay_ms = (delay_ms as f64 * RECONNECT_BACKOFF) as u64;
            delay_ms = delay_ms.min(RECONNECT_MAX_MS);
        }

        CONNECTION_GREETED.store(false, Ordering::SeqCst);
        let connected_at = Instant::now();
        match run_connection().await {
            Ok(reason) => {
                if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
                    crate::log_emotes!(info, "7TV EventAPI: connection closed for shutdown");
                    continue;
                }
                if reason == STREAM_ENDED {
                    crate::log_emotes!(
                        warn,
                        "7TV EventAPI: connection lost ({reason}), reconnecting"
                    );
                } else {
                    crate::log_emotes!(
                        info,
                        "7TV EventAPI: connection closed ({reason}), reconnecting"
                    );
                }
                delay_ms = next_reconnect_delay_ms(
                    delay_ms,
                    connected_at.elapsed(),
                    CONNECTION_GREETED.load(Ordering::SeqCst),
                    SLOW_RECONNECT_REQUESTED.swap(false, Ordering::SeqCst),
                );
            }
            Err(e) => {
                update_diagnostics(|diag| {
                    diag.connected = false;
                    diag.last_error = Some(e.clone());
                });
                crate::backend::reliability::notify_provider_degraded(
                    crate::backend::reliability::Provider::SevenTvEventApi,
                    e.clone(),
                );
                crate::log_emotes!(warn, "7TV EventAPI: connection error: {}", e);
                delay_ms = next_reconnect_delay_ms(
                    delay_ms,
                    connected_at.elapsed(),
                    CONNECTION_GREETED.load(Ordering::SeqCst),
                    SLOW_RECONNECT_REQUESTED.swap(false, Ordering::SeqCst),
                );
            }
        }

        attempt += 1;
    }
}

const STREAM_ENDED: &str = "stream ended without a close frame";

/// Run one EventAPI connection. `Ok` carries why it ended cleanly; the
/// supervisor logs that single "down" transition.
async fn run_connection() -> Result<&'static str, String> {
    crate::log_emotes!(debug, "7TV EventAPI: connecting to {}", EVENTAPI_URL);

    let (ws_stream, _) = crate::backend::ws::connect_websocket(EVENTAPI_URL)
        .await
        .map_err(|e| format!("WebSocket connect: {e}"))?;

    crate::log_emotes!(info, "7TV EventAPI: WebSocket connected");
    LAST_HEARTBEAT_AT_MS.store(crate::utils::unix_now_ms(), Ordering::Relaxed);
    update_diagnostics(|diag| {
        diag.connected = false;
        diag.last_error = None;
        diag.desired_subscriptions = desired_subscriptions().len();
        diag.active_subscriptions = 0;
        diag.in_flight_subscription_commands = 0;
        diag.last_heartbeat_ms_ago = Some(0);
    });

    let (mut ws_sink, mut ws_stream) = ws_stream.split();
    let previous_session_id = if RESUME_REQUIRES_FRESH_SESSION.swap(false, Ordering::SeqCst) {
        RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clear();
        AUTHORITATIVE_RECONCILE_REQUIRED.store(true, Ordering::SeqCst);
        LAST_SESSION_ID.lock().await.take();
        None
    } else {
        LAST_SESSION_ID.lock().await.clone()
    };
    let mut subscriptions = ConnectionSubscriptions::new(previous_session_id);

    let mut health = ConnectionHealth::new();

    loop {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            let _ = ws_sink.send(Message::Close(None)).await;
            return Ok("shutdown");
        }

        let heartbeat_wait = health.timeout_remaining();
        tokio::select! {
            _ = sleep(Duration::from_millis(250)) => {
                if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
                    let _ = ws_sink.send(Message::Close(None)).await;
                    return Ok("shutdown");
                }
                if subscriptions.has_expired_ack_at(Instant::now()) {
                    subscriptions.require_fresh_session();
                    crate::backend::metrics::record_emote_event("7tv.eventapi.command_ack_timeout");
                    return Err(
                        "command ACK timed out; reconnecting with a fresh authoritative session"
                            .to_string(),
                    );
                }
                flush_connection_commands(&mut ws_sink, &mut subscriptions).await?;
            }

            _ = sleep(heartbeat_wait) => {
                if health.is_timed_out() {
                    return Err(format!(
                        "heartbeat timeout after {}ms",
                        health.last_heartbeat.elapsed().as_millis()
                    ));
                }
            }

            msg = ws_stream.next() => {
                match msg {
                    None => {
                        return Ok(STREAM_ENDED);
                    }
                    Some(Err(e)) => {
                        return Err(format!("WebSocket error: {e}"));
                    }
                    Some(Ok(Message::Text(text))) => {
                        if handle_message(&text, &mut health, &mut subscriptions).await? == MessageControl::Reconnect {
                            update_diagnostics(|diag| {
                                diag.connected = false;
                            });
                            return Ok("server requested reconnect");
                        }
                        update_diagnostics(|diag| {
                            diag.last_heartbeat_ms_ago =
                                Some(health.last_heartbeat.elapsed().as_millis() as u64);
                        });
                        flush_connection_commands(&mut ws_sink, &mut subscriptions).await?;
                    }
                    Some(Ok(Message::Close(_))) => {
                        return Ok("server sent close frame");
                    }
                    Some(Ok(_)) => {}
                }
            }

        }
    }
}

async fn send_subscription_command<S, E>(
    ws_sink: &mut S,
    subscriptions: &mut ConnectionSubscriptions,
    command: SubscriptionCommand,
) -> Result<(), String>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match command {
        SubscriptionCommand::Subscribe(key) => {
            if key.object_id.is_empty()
                || subscriptions.active.contains(&key)
                || subscriptions
                    .pending
                    .iter()
                    .any(|pending| pending.command == SubscriptionCommand::Subscribe(key.clone()))
            {
                return Ok(());
            }
            crate::log_emotes!(
                debug,
                "7TV EventAPI: subscribing to {} {}",
                key.event_type(),
                key.object_id
            );
            if let Err(error) = ws_sink
                .send(make_subscription_message(op::SUBSCRIBE, &key))
                .await
            {
                subscriptions.require_fresh_session();
                crate::backend::metrics::record_emote_event("7tv.eventapi.command_send_failed");
                return Err(format!("Subscribe send: {error}"));
            }
            subscriptions.pending.push(PendingSubscriptionCommand {
                command: SubscriptionCommand::Subscribe(key),
                sent_at: Instant::now(),
            });
        }
        SubscriptionCommand::Unsubscribe(key) => {
            if key.object_id.is_empty()
                || !subscriptions.active.contains(&key)
                || subscriptions
                    .pending
                    .iter()
                    .any(|pending| pending.command == SubscriptionCommand::Unsubscribe(key.clone()))
            {
                return Ok(());
            }
            crate::log_emotes!(
                debug,
                "7TV EventAPI: unsubscribing from {} {}",
                key.event_type(),
                key.object_id
            );
            if let Err(error) = ws_sink
                .send(make_subscription_message(op::UNSUBSCRIBE, &key))
                .await
            {
                subscriptions.require_fresh_session();
                crate::backend::metrics::record_emote_event("7tv.eventapi.command_send_failed");
                return Err(format!("Unsubscribe send: {error}"));
            }
            subscriptions.pending.push(PendingSubscriptionCommand {
                command: SubscriptionCommand::Unsubscribe(key),
                sent_at: Instant::now(),
            });
        }
    }
    Ok(())
}

async fn flush_connection_commands<S, E>(
    ws_sink: &mut S,
    subscriptions: &mut ConnectionSubscriptions,
) -> Result<(), String>
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    if subscriptions.command_gate() == CommandGate::AwaitHello {
        return Ok(());
    }

    if !subscriptions.resume_sent {
        subscriptions.resume_sent = true;
        if let Some(session_id) = subscriptions.resume_session_id.as_deref() {
            crate::log_emotes!(info, "7TV EventAPI: attempting RESUME with session_id");
            let payload =
                serde_json::json!({ "op": op::RESUME, "d": { "session_id": session_id } });
            if let Err(error) = ws_sink
                .send(Message::Text(payload.to_string().into()))
                .await
            {
                subscriptions.require_fresh_session();
                crate::backend::metrics::record_emote_event("7tv.eventapi.command_send_failed");
                return Err(format!("RESUME send: {error}"));
            }
            subscriptions.resume_pending = true;
            subscriptions.resume_sent_at = Some(Instant::now());
            publish_subscription_diagnostics(subscriptions);
            return Ok(());
        }
    }

    if subscriptions.command_gate() != CommandGate::Ready {
        return Ok(());
    }

    PENDING_SUBSCRIPTIONS.lock_or_recover().clear();
    let forced = std::mem::take(&mut *FORCED_RESUBSCRIBES.lock_or_recover());
    for key in forced {
        if subscriptions.active.contains(&key) {
            crate::log_emotes!(
                info,
                "7TV EventAPI: re-establishing {} {}",
                key.event_type(),
                key.object_id
            );
            send_subscription_command(
                ws_sink,
                subscriptions,
                SubscriptionCommand::Unsubscribe(key),
            )
            .await?;
        }
    }
    let desired: HashSet<_> = desired_subscriptions().into_iter().collect();
    let commands =
        desired_subscription_commands(&subscriptions.active, &subscriptions.pending, &desired);
    let committed = subscriptions.active.len() + subscriptions.pending.len();
    let limit = SUBSCRIPTION_LIMIT.load(Ordering::Relaxed) as usize;
    let (commands, deferred) = cap_subscribe_commands(commands, committed, limit);
    if deferred > 0 && should_warn_drop("subscription-limit".to_string()) {
        crate::log_emotes!(
            warn,
            "7TV EventAPI: {} desired subscription(s) exceed the server limit of {}; emote sets are subscribed first",
            deferred,
            limit
        );
    }
    for command in commands {
        send_subscription_command(ws_sink, subscriptions, command).await?;
    }
    publish_subscription_diagnostics(subscriptions);
    Ok(())
}

/// Keep every unsubscribe, order subscribes emote sets first, and admit only
/// as many subscribes as the server's per-connection limit leaves room for.
/// Returns the admitted commands and how many subscribes were held back.
fn cap_subscribe_commands(
    commands: Vec<SubscriptionCommand>,
    committed: usize,
    limit: usize,
) -> (Vec<SubscriptionCommand>, usize) {
    let (mut admitted, mut subscribes): (Vec<_>, Vec<_>) = commands
        .into_iter()
        .partition(|command| matches!(command, SubscriptionCommand::Unsubscribe(_)));
    subscribes.sort_by_key(|command| match command {
        SubscriptionCommand::Subscribe(key) | SubscriptionCommand::Unsubscribe(key) => {
            match key.kind {
                SubscriptionKind::EmoteSetUpdate => 0,
                SubscriptionKind::UserUpdate => 1,
                SubscriptionKind::EntitlementCreate | SubscriptionKind::EntitlementDelete => 2,
            }
        }
    });
    let room = if limit == 0 {
        subscribes.len()
    } else {
        limit.saturating_sub(committed)
    };
    let deferred = subscribes.len().saturating_sub(room);
    admitted.extend(subscribes.into_iter().take(room));
    (admitted, deferred)
}

fn desired_subscription_commands(
    active: &HashSet<SubscriptionKey>,
    pending: &[PendingSubscriptionCommand],
    desired: &HashSet<SubscriptionKey>,
) -> Vec<SubscriptionCommand> {
    let is_pending =
        |command: &SubscriptionCommand| pending.iter().any(|pending| pending.command == *command);
    let mut commands = active
        .difference(desired)
        .cloned()
        .map(SubscriptionCommand::Unsubscribe)
        .filter(|command| !is_pending(command))
        .collect::<Vec<_>>();
    commands.extend(
        desired
            .difference(active)
            .cloned()
            .map(SubscriptionCommand::Subscribe)
            .filter(|command| !is_pending(command)),
    );
    commands
}

fn publish_subscription_diagnostics(subscriptions: &ConnectionSubscriptions) {
    update_diagnostics(|diag| {
        diag.desired_subscriptions = desired_subscriptions().len();
        diag.active_subscriptions = subscriptions.active_count();
        diag.in_flight_subscription_commands = subscriptions.in_flight_count();
    });
}

fn make_subscription_message(opcode: u32, key: &SubscriptionKey) -> Message {
    let payload = serde_json::json!({
        "op": opcode,
        "d": {
            "type": key.event_type(),
            "condition": key.condition()
        }
    });
    Message::Text(payload.to_string().into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageControl {
    Continue,
    Reconnect,
}

fn eventapi_payload_code(payload: &serde_json::Value) -> Option<u16> {
    payload
        .get("code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
}

fn eventapi_code_requires_reconnect(code: u16) -> bool {
    matches!(code, 4000 | 4006 | 4007 | 4008)
}

fn error_payload_is_resume_session_failure(payload: &serde_json::Value) -> bool {
    fn contains_resume_opcode(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
                ((key == "op" || key == "opcode") && value.as_u64() == Some(op::RESUME as u64))
                    || contains_resume_opcode(value)
            }),
            serde_json::Value::Array(values) => values.iter().any(contains_resume_opcode),
            _ => false,
        }
    }

    let resume_opcode = contains_resume_opcode(payload);
    let text = payload.to_string().to_ascii_lowercase();
    let invalid_session = (text.contains("session") || text.contains("resume"))
        && [
            "invalid",
            "expired",
            "unknown",
            "not found",
            "missing",
            "closed",
            "ended",
            "rejected",
            "failed",
            "denied",
        ]
        .iter()
        .any(|marker| text.contains(marker));

    resume_opcode || invalid_session
}

async fn handle_message(
    text: &str,
    health: &mut ConnectionHealth,
    subscriptions: &mut ConnectionSubscriptions,
) -> Result<MessageControl, String> {
    let msg: EventApiMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            crate::log_emotes!(debug, "7TV EventAPI: failed to parse message: {e}");
            return Ok(MessageControl::Continue);
        }
    };

    match msg.op {
        op::HELLO => {
            let heartbeat = msg
                .d
                .get("heartbeat_interval")
                .and_then(|v| v.as_u64())
                .unwrap_or(25_000);
            crate::log_emotes!(
                debug,
                "7TV EventAPI: HELLO received (heartbeat_interval: {}ms)",
                heartbeat
            );
            health.update_interval_ms(heartbeat);
            let limit = msg
                .d
                .get("subscription_limit")
                .and_then(serde_json::Value::as_i64)
                .filter(|limit| *limit > 0)
                .unwrap_or(0) as u64;
            SUBSCRIPTION_LIMIT.store(limit, Ordering::Relaxed);
            crate::log_emotes!(debug, "7TV EventAPI: session subscription limit {}", limit);
            subscriptions.hello_received = true;
            CONNECTION_GREETED.store(true, Ordering::SeqCst);
            SLOW_RECONNECT_REQUESTED.store(false, Ordering::SeqCst);
            crate::backend::reliability::notify_provider_recovered(
                crate::backend::reliability::Provider::SevenTvEventApi,
            );
            update_diagnostics(|diag| {
                diag.connected = true;
                diag.last_error = None;
            });
            if let Some(sid) = msg.d.get("session_id").and_then(|v| v.as_str()) {
                *LAST_SESSION_ID.lock().await = Some(sid.to_string());
                crate::log_emotes!(debug, "7TV EventAPI: stored session_id for RESUME");
            }
            if AUTHORITATIVE_RECONCILE_REQUIRED.swap(false, Ordering::SeqCst) {
                schedule_authoritative_reconcile_after_session_loss();
            }
        }

        op::HEARTBEAT => {
            health.mark_heartbeat();
            LAST_HEARTBEAT_AT_MS.store(crate::utils::unix_now_ms(), Ordering::Relaxed);
            update_diagnostics(|diag| {
                diag.last_heartbeat_ms_ago = Some(0);
            });
        }

        op::RECONNECT => {
            crate::backend::metrics::record_emote_event("7tv.eventapi.server_reconnect");
            crate::log_emotes!(debug, "7TV EventAPI: server requested reconnect");
            return Ok(MessageControl::Reconnect);
        }

        op::ACK => {
            let resume_outcome = (ack_is_resume(&msg.d) && subscriptions.resume_pending)
                .then(|| resume_ack_outcome(&msg.d, subscriptions.active_count()));
            if resume_outcome == Some(ResumeAckOutcome::Empty) {
                // A failed RESUME still leaves a valid fresh session: start
                // over on it instead of trusting the previous subscriptions.
                subscriptions.acknowledge_resume();
                subscriptions.active.clear();
                RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clear();
                crate::backend::metrics::record_emote_event("7tv.eventapi.resume_not_restored");
                crate::log_emotes!(
                    info,
                    "7TV EventAPI: RESUME restored no subscriptions; resubscribing on the new session: {}",
                    msg.d
                );
                schedule_authoritative_reconcile_after_session_loss();
                publish_subscription_diagnostics(subscriptions);
            } else if resume_outcome == Some(ResumeAckOutcome::Partial) {
                subscriptions.require_fresh_session();
                crate::backend::metrics::record_emote_event("7tv.eventapi.resume_partial");
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI: RESUME restored only part of {} subscription(s); reconnecting fresh: {}",
                    subscriptions.active_count(),
                    msg.d
                );
                return Ok(MessageControl::Reconnect);
            } else if ack_is_resume(&msg.d) && subscriptions.acknowledge_resume() {
                crate::log_emotes!(info, "7TV EventAPI: RESUME ACK received");
                publish_subscription_diagnostics(subscriptions);
            } else if let Some(command) = ack_subscription_command(&msg.d) {
                if subscriptions.acknowledge(&command) {
                    crate::log_emotes!(debug, "7TV EventAPI: {:?} ACK received", command);
                    publish_subscription_diagnostics(subscriptions);
                } else {
                    crate::log_emotes!(
                        debug,
                        "7TV EventAPI: ignored unexpected subscription ACK: {:?}",
                        command
                    );
                }
            } else {
                crate::log_emotes!(
                    debug,
                    "7TV EventAPI: ignored ACK without a recognized command/data echo"
                );
            }
        }

        op::ERROR => {
            update_diagnostics(|diag| {
                diag.last_error = Some(msg.d.to_string());
            });
            let code = eventapi_payload_code(&msg.d);
            if code == Some(4007) {
                SLOW_RECONNECT_REQUESTED.store(true, Ordering::SeqCst);
            }
            if error_payload_is_resume_session_failure(&msg.d) {
                let cleared_session = LAST_SESSION_ID.lock().await.take().is_some();
                RESUME_ACKED_SUBSCRIPTIONS.lock_or_recover().clear();
                AUTHORITATIVE_RECONCILE_REQUIRED.store(true, Ordering::SeqCst);
                crate::backend::metrics::record_emote_event("7tv.eventapi.resume_session_rejected");
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI: reconnectable session error (cleared_session={}); reconnecting fresh: {}",
                    cleared_session,
                    msg.d
                );
                return Ok(MessageControl::Reconnect);
            }
            if command_error_requires_fresh(subscriptions.in_flight_count()) {
                let echoed_command = ack_subscription_command(&msg.d);
                subscriptions.require_fresh_session();
                crate::backend::metrics::record_emote_event("7tv.eventapi.command_error_reconnect");
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI: command error while {} command(s) awaited ACK{}; reconnecting fresh: {}",
                    subscriptions.in_flight_count(),
                    echoed_command
                        .as_ref()
                        .map(|command| format!(" (echoed {command:?})"))
                        .unwrap_or_default(),
                    msg.d
                );
                return Ok(MessageControl::Reconnect);
            }
            if code.is_some_and(eventapi_code_requires_reconnect) {
                crate::backend::metrics::record_emote_event("7tv.eventapi.reconnectable_error");
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI: reconnectable error code {:?}; preserving the session for RESUME: {}",
                    code,
                    msg.d
                );
                return Ok(MessageControl::Reconnect);
            }

            crate::backend::metrics::record_emote_event("7tv.eventapi.error_nonfatal");
            crate::log_emotes!(
                warn,
                "7TV EventAPI: non-fatal command/subscription error payload: {}",
                msg.d
            );
        }

        op::END_OF_STREAM => {
            let code = eventapi_payload_code(&msg.d);
            if code == Some(4007) {
                SLOW_RECONNECT_REQUESTED.store(true, Ordering::SeqCst);
            }
            crate::backend::metrics::record_emote_event("7tv.eventapi.end_of_stream");
            crate::log_emotes!(
                info,
                "7TV EventAPI: server ended stream (code={:?}); reconnecting with session resume: {}",
                code,
                msg.d
            );
            return Ok(MessageControl::Reconnect);
        }

        op::DISPATCH => {
            handle_dispatch(&msg.d).await;
        }

        _ => {
            crate::log_emotes!(debug, "7TV EventAPI: unknown opcode {}", msg.op);
        }
    }

    Ok(MessageControl::Continue)
}

fn ack_is_resume(payload: &serde_json::Value) -> bool {
    payload
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| {
            command.eq_ignore_ascii_case("resume") || command == op::RESUME.to_string()
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeAckOutcome {
    Restored,
    /// The server kept nothing (`success: false`); the new session is empty.
    Empty,
    /// Some but not all subscriptions came back; which ones is unknowable.
    Partial,
}

/// 7TV acknowledges every RESUME, including one that failed
/// (`{"success":false,"subscriptions_restored":0}` - the answer to resuming a
/// cleanly closed session). Trusting such an ACK leaves Echo believing it is
/// subscribed while the server delivers nothing.
fn resume_ack_outcome(
    payload: &serde_json::Value,
    expected_subscriptions: usize,
) -> ResumeAckOutcome {
    let data = payload.get("data");
    let success = data
        .and_then(|data| data.get("success"))
        .and_then(serde_json::Value::as_bool);
    let restored = data
        .and_then(|data| data.get("subscriptions_restored"))
        .and_then(serde_json::Value::as_u64)
        .map(|restored| restored as usize);
    match (success, restored) {
        (Some(false), None | Some(0)) => ResumeAckOutcome::Empty,
        (Some(false), Some(_)) => ResumeAckOutcome::Partial,
        (_, Some(0)) if expected_subscriptions > 0 => ResumeAckOutcome::Empty,
        (_, Some(restored)) if restored < expected_subscriptions => ResumeAckOutcome::Partial,
        _ => ResumeAckOutcome::Restored,
    }
}

fn ack_subscription_command(payload: &serde_json::Value) -> Option<SubscriptionCommand> {
    let command = payload.get("command")?.as_str()?;
    let opcode = if command.eq_ignore_ascii_case("subscribe") || command == "35" {
        op::SUBSCRIBE
    } else if command.eq_ignore_ascii_case("unsubscribe") || command == "36" {
        op::UNSUBSCRIBE
    } else {
        return None;
    };
    let data = payload.get("data")?;
    let kind = SubscriptionKind::from_event_type(data.get("type")?.as_str()?)?;
    let condition = data.get("condition")?;
    let object_id = if kind.is_channel_scoped() {
        condition.get("id")
    } else {
        condition.get("object_id")
    }?
    .as_str()?
    .trim();
    if object_id.is_empty() {
        return None;
    }
    let key = SubscriptionKey {
        kind,
        object_id: object_id.to_string(),
    };
    Some(if opcode == op::SUBSCRIBE {
        SubscriptionCommand::Subscribe(key)
    } else {
        SubscriptionCommand::Unsubscribe(key)
    })
}

async fn handle_dispatch(payload: &serde_json::Value) {
    let event_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

    if event_type == "user.update" {
        handle_user_update_dispatch(payload).await;
        return;
    }

    if matches!(event_type, "entitlement.create" | "entitlement.delete") {
        handle_entitlement_dispatch(payload, event_type == "entitlement.create");
        return;
    }

    if event_type != "emote_set.update" {
        return;
    }

    let body = match payload.get("body") {
        Some(b) => b,
        None => return,
    };

    let set_id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let actor = dispatch_actor_name(body);

    if set_id.is_empty() {
        return;
    }

    let channels = get_channels_for_set(set_id);
    if channels.is_empty() {
        crate::backend::metrics::record_emote_event("7tv.dispatch.unowned_set_dropped");
        if should_warn_drop(format!("unowned-set:{set_id}")) {
            crate::log_emotes!(
                warn,
                "7TV EventAPI: dropped emote_set.update for set {set_id}: no open channel owns it (owned sets: {})",
                SET_TO_CHANNELS.len()
            );
        }
        return;
    }

    for channel in channels {
        let is_global = channel == GLOBAL_SET_CHANNEL;
        let display_channel = if is_global { "global" } else { &channel };

        let changes =
            log_dispatch_changes(body, &channel, display_channel, is_global, actor.clone());
        if changes.summary.added == 0
            && changes.summary.removed == 0
            && changes.summary.updated == 0
        {
            if dispatch_has_emote_items(body) {
                crate::backend::metrics::record_emote_event("7tv.dispatch.unparsed_emote_items");
                if should_warn_drop(format!("unparsed:{set_id}")) {
                    crate::log_emotes!(
                        warn,
                        "7TV EventAPI: emote_set.update for #{} carried emote items Echo could not read; reconciling the full set",
                        display_channel
                    );
                }
                schedule_refresh_after_dispatch(
                    set_id,
                    &channel,
                    is_global,
                    DispatchChanges {
                        requires_full_refresh: true,
                        ..DispatchChanges::default()
                    },
                );
                continue;
            }
            crate::log_emotes!(
                debug,
                "7TV EventAPI: emote_set.update for #{} had no emote changes",
                display_channel
            );
            continue;
        }

        crate::managers::frame_rate_controller::record_activity();
        crate::managers::frame_rate_controller::wakeup();
        schedule_refresh_after_dispatch(set_id, &channel, is_global, changes);
    }
}

/// `entitlement.create` / `entitlement.delete` for a chatter in a subscribed
/// channel. Personal emote sets are applied directly. Paint and badge
/// entitlements still resolve through the chatter's cosmetics profile; they
/// trigger a bounded refetch when they disagree with the cached profile.
fn handle_entitlement_dispatch(payload: &serde_json::Value, created: bool) {
    let Some(entitlement) = payload.get("body").and_then(|body| body.get("object")) else {
        return;
    };
    let cosmetic_kind = match entitlement.get("kind").and_then(|kind| kind.as_str()) {
        Some("EMOTE_SET") => None,
        Some("PAINT") => Some(crate::backend::cosmetics::CosmeticEntitlementKind::Paint),
        Some("BADGE") => Some(crate::backend::cosmetics::CosmeticEntitlementKind::Badge),
        _ => return,
    };
    let Some(ref_id) = entitlement
        .get("ref_id")
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return;
    };
    let Some(twitch_user_id) = entitlement
        .get("user")
        .and_then(|user| user.get("connections"))
        .and_then(|connections| connections.as_array())
        .and_then(|connections| {
            connections.iter().find(|connection| {
                connection
                    .get("platform")
                    .and_then(|platform| platform.as_str())
                    .is_some_and(|platform| platform.eq_ignore_ascii_case("TWITCH"))
            })
        })
        .and_then(|connection| connection.get("id"))
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return;
    };
    if let Some(kind) = cosmetic_kind {
        crate::backend::cosmetics::refresh_user_for_entitlement(
            twitch_user_id,
            kind,
            ref_id,
            created,
        );
    } else if created {
        let channels = CHANNEL_ENTITLEMENT_IDS
            .iter()
            .map(|entry| std::sync::Arc::from(entry.key().as_str()))
            .collect();
        super::personal::note_entitled_personal_set(twitch_user_id, ref_id, channels);
    } else {
        super::personal::forget_entitled_personal_set(twitch_user_id, ref_id);
    }
}

async fn handle_user_update_dispatch(payload: &serde_json::Value) {
    let Some(body) = payload.get("body") else {
        return;
    };

    let user_id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let actor = dispatch_actor_name(body);
    let Some(updates) = body.get("updated").and_then(|v| v.as_array()) else {
        return;
    };
    let has_cosmetic_update = updates.iter().any(user_update_touches_cosmetics);

    for update in updates.iter().filter(|item| {
        item.get("key")
            .and_then(|v| v.as_str())
            .is_some_and(|key| key == "connections")
    }) {
        let Some(connection_index) = update.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let connection_index = connection_index as usize;

        let Some(connection_updates) = update.get("value").and_then(|v| v.as_array()) else {
            continue;
        };

        for connection_update in connection_updates.iter().filter(|item| {
            item.get("key")
                .and_then(|v| v.as_str())
                .is_some_and(|key| key == "emote_set")
        }) {
            let old_set_id = connection_update
                .get("old_value")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_set_id = connection_update
                .get("value")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if old_set_id == new_set_id {
                continue;
            }

            let mut candidate_channels = HashSet::new();
            if !old_set_id.is_empty() {
                candidate_channels.extend(get_channels_for_set(old_set_id));
            }
            if !user_id.is_empty() {
                candidate_channels.extend(get_channels_for_user(user_id));
                candidate_channels.extend(
                    channel_details_for_user_connection(user_id, connection_index)
                        .into_iter()
                        .map(|(channel, _)| channel),
                );
            }

            if candidate_channels.is_empty() {
                if should_warn_drop(format!("user-update:{user_id}")) {
                    crate::log_emotes!(
                        warn,
                        "7TV EventAPI: dropped user.update active set change for user {} old={} new={}: no open channel owns it",
                        user_id,
                        old_set_id,
                        new_set_id
                    );
                }
                continue;
            }

            for channel in candidate_channels {
                let Some(details) = CHANNEL_DETAILS.get(&channel).map(|entry| entry.clone()) else {
                    crate::log_emotes!(
                        debug,
                        "7TV EventAPI: user.update matched #{} but no channel details are registered",
                        channel
                    );
                    continue;
                };

                if !user_id.is_empty() && details.user_id != user_id {
                    crate::log_emotes!(
                        debug,
                        "7TV EventAPI: ignored user.update for #{} user {} because registered user is {}",
                        channel,
                        user_id,
                        details.user_id
                    );
                    continue;
                }

                if details.connection_index != connection_index {
                    crate::log_emotes!(
                        debug,
                        "7TV EventAPI: ignored user.update for #{} connection {} because registered connection is {}",
                        channel,
                        connection_index,
                        details.connection_index
                    );
                    continue;
                }

                crate::backend::metrics::record_emote_event("7tv.user_update.active_set");
                if new_set_id.is_empty() {
                    schedule_active_set_clear(
                        channel,
                        details,
                        old_set_id.to_string(),
                        actor.clone(),
                    );
                } else {
                    schedule_active_set_refresh(
                        channel,
                        details,
                        old_set_id.to_string(),
                        new_set_id.to_string(),
                        actor.clone(),
                    );
                }
            }
        }
    }

    if !user_id.is_empty() && has_cosmetic_update {
        crate::backend::cosmetics::refresh_live_seventv_user(user_id);
        crate::backend::metrics::record_emote_event("7tv.user_update.cosmetics");
    }
}

fn user_update_touches_cosmetics(update: &serde_json::Value) -> bool {
    const COSMETIC_KEYS: &[&str] = &[
        "style",
        "avatar_url",
        "avatar",
        "paint_id",
        "badge_id",
        "color",
    ];

    if update
        .get("key")
        .and_then(|value| value.as_str())
        .is_some_and(|key| {
            COSMETIC_KEYS
                .iter()
                .any(|candidate| key.eq_ignore_ascii_case(candidate))
        })
    {
        return true;
    }

    update
        .get("value")
        .and_then(|value| value.as_array())
        .is_some_and(|items| items.iter().any(user_update_touches_cosmetics))
}

fn dispatch_actor_name(body: &serde_json::Value) -> Option<String> {
    body.get("actor")
        .and_then(|actor| {
            actor
                .get("display_name")
                .or_else(|| actor.get("username"))
                .or_else(|| actor.get("name"))
        })
        .and_then(|v| v.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn schedule_active_set_refresh(
    channel: String,
    details: SevenTvChannelSubscription,
    old_set_id: String,
    new_set_id: String,
    actor: Option<String>,
) {
    if channel == GLOBAL_SET_CHANNEL {
        return;
    }

    ACTIVE_SET_REFRESH_TARGETS.insert(channel.clone(), new_set_id.clone());

    crate::log_emotes!(
        info,
        "7TV EventAPI: #{} active emote set changed {} -> {}{}",
        channel,
        old_set_id,
        new_set_id,
        actor
            .as_deref()
            .map(|name| format!(" by {}", name))
            .unwrap_or_default()
    );

    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();

    crate::backend::bridge::RUNTIME.spawn(run_active_set_refresh(
        channel, details, old_set_id, new_set_id, actor,
    ));
}

async fn run_active_set_refresh(
    channel: String,
    mut details: SevenTvChannelSubscription,
    old_set_id: String,
    new_set_id: String,
    actor: Option<String>,
) {
    super::invalidate_seventv_fetch_cache_for_event(Some(&channel), false);
    let mut attempt = 0;
    let fetch_result = loop {
        let result = super::fetch_seventv_emote_set_by_id(&new_set_id, Some(&channel)).await;
        let retry_delay = SET_FETCH_RETRY_DELAYS_SECS.get(attempt);
        match (&result, retry_delay) {
            (Err(err), Some(delay)) if active_set_refresh_is_current(&channel, &new_set_id) => {
                crate::log_emotes!(
                    warn,
                    "7TV EventAPI: active set {} fetch for #{} failed (attempt {}); retrying in {}s: {}",
                    new_set_id,
                    channel,
                    attempt + 1,
                    delay,
                    err
                );
                attempt += 1;
                sleep(Duration::from_secs(*delay)).await;
            }
            _ => break result,
        }
    };
    match fetch_result {
        Ok(super::SevenTvExactSetFetchOutcome::Present {
            emotes,
            emote_set_id,
        }) => {
            let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
            if !active_set_refresh_is_current(&channel, &new_set_id) {
                crate::backend::metrics::record_emote_event("7tv.user_update.active_set.stale");
                crate::log_emotes!(
                    debug,
                    "7TV EventAPI: dropped stale active set refresh {} for #{}",
                    new_set_id,
                    channel
                );
                return;
            }

            let applied_set_id = emote_set_id.unwrap_or_else(|| new_set_id.clone());
            let count =
                super::apply_seventv_channel_emote_rows(&channel, emotes, &applied_set_id).await;
            if !active_set_refresh_is_current(&channel, &new_set_id) {
                crate::backend::metrics::record_emote_event(
                    "7tv.user_update.active_set.stale_after_apply",
                );
                crate::log_emotes!(
                    debug,
                    "7TV EventAPI: did not re-subscribe stale active set {} for #{} after a newer transition arrived",
                    new_set_id,
                    channel
                );
                return;
            }
            details.emote_set_id = applied_set_id.clone();
            subscribe_channel_details(&channel, details);
            clear_active_set_refresh_target(&channel, &new_set_id);
            crate::backend::metrics::record_emote_event("7tv.user_update.active_set.applied");
            crate::log_emotes!(
                info,
                "7TV EventAPI: applied active 7TV set {} for #{} ({} emotes)",
                applied_set_id,
                channel,
                count
            );
            let actor_suffix = actor
                .as_deref()
                .map(|name| format!(" by {}", name))
                .unwrap_or_default();
            push_emote_notification(
                &channel,
                &format!(
                    "7TV active emote set changed{}; loaded {} emotes",
                    actor_suffix, count
                ),
            );
        }
        Ok(super::SevenTvExactSetFetchOutcome::TerminallyAbsent { emote_set_id }) => {
            let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
            if !active_set_refresh_is_current(&channel, &new_set_id) {
                crate::backend::metrics::record_emote_event(
                    "7tv.user_update.active_set.terminal_stale",
                );
                crate::log_emotes!(
                    debug,
                    "7TV EventAPI: dropped stale terminal active-set result {} for #{}",
                    emote_set_id,
                    channel
                );
                return;
            }

            if retire_terminal_active_set(&channel, &old_set_id, &new_set_id, details) {
                let applied_set_id = if old_set_id.is_empty() {
                    new_set_id.as_str()
                } else {
                    old_set_id.as_str()
                };
                let removed =
                    super::clear_seventv_channel_emote_rows_if_set(&channel, applied_set_id)
                        .await
                        .unwrap_or(0);
                clear_active_set_refresh_target(&channel, &new_set_id);
                crate::backend::metrics::record_emote_event(
                    "7tv.user_update.active_set.terminal_cleared",
                );
                crate::log_emotes!(
                    info,
                    "7TV EventAPI: active set {} is terminally absent for #{}; cleared {} 7TV emotes and retained user.update",
                    emote_set_id,
                    channel,
                    removed
                );
                let actor_suffix = actor
                    .as_deref()
                    .map(|name| format!(" by {}", name))
                    .unwrap_or_default();
                push_emote_notification(
                    &channel,
                    &format!(
                        "7TV active emote set changed{} but is no longer available; cleared {} emotes",
                        actor_suffix, removed
                    ),
                );
            } else {
                clear_active_set_refresh_target(&channel, &new_set_id);
                crate::backend::metrics::record_emote_event(
                    "7tv.user_update.active_set.terminal_stale",
                );
            }
        }
        Err(err) => {
            // 7TV already switched the channel to the new set, so follow it:
            // its live updates (and the next channel refresh) heal the rows
            // this fetch could not replace.
            let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
            let followed = active_set_refresh_is_current(&channel, &new_set_id);
            if followed {
                details.emote_set_id = new_set_id.clone();
                subscribe_channel_details(&channel, details);
            }
            clear_active_set_refresh_target(&channel, &new_set_id);
            if followed {
                // The rows are still the old set's: keep retrying the exact
                // new set through the reconcile backoff.
                queue_pending_refresh(PendingRefresh {
                    set_id: new_set_id.clone(),
                    channel: channel.clone(),
                    is_global: false,
                    added: 0,
                    removed: 0,
                    updated: 0,
                    requires_full_refresh: true,
                    patch: Default::default(),
                    notifications: Vec::new(),
                    fetch_attempt: 0,
                });
            }
            crate::backend::metrics::record_emote_event("7tv.user_update.active_set.failed");
            crate::log_emotes!(
                warn,
                "7TV EventAPI: failed to apply active set {} for #{} (live subscription followed: {}): {}",
                new_set_id,
                channel,
                followed,
                err
            );
        }
    }
    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();
}

fn schedule_active_set_clear(
    channel: String,
    details: SevenTvChannelSubscription,
    old_set_id: String,
    actor: Option<String>,
) {
    if channel == GLOBAL_SET_CHANNEL {
        return;
    }

    ACTIVE_SET_REFRESH_TARGETS.insert(channel.clone(), String::new());

    crate::log_emotes!(
        info,
        "7TV EventAPI: #{} active emote set removed from {}{}",
        channel,
        old_set_id,
        actor
            .as_deref()
            .map(|name| format!(" by {}", name))
            .unwrap_or_default()
    );

    crate::managers::frame_rate_controller::record_activity();
    crate::managers::frame_rate_controller::wakeup();

    crate::backend::bridge::RUNTIME
        .spawn(run_active_set_clear(channel, details, old_set_id, actor));
}

async fn run_active_set_clear(
    channel: String,
    details: SevenTvChannelSubscription,
    old_set_id: String,
    actor: Option<String>,
) {
    let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
    if !active_set_refresh_is_current(&channel, "")
        || !retire_terminal_active_set(&channel, &old_set_id, "", details)
    {
        crate::backend::metrics::record_emote_event("7tv.user_update.active_set.clear_stale");
        clear_active_set_refresh_target(&channel, "");
        return;
    }
    let removed = super::clear_seventv_channel_emote_rows_if_set(&channel, &old_set_id)
        .await
        .unwrap_or(0);
    clear_active_set_refresh_target(&channel, "");
    crate::backend::metrics::record_emote_event("7tv.user_update.active_set.cleared");
    crate::log_emotes!(
        info,
        "7TV EventAPI: cleared active 7TV set for #{} (removed {} emotes)",
        channel,
        removed
    );
    let actor_suffix = actor
        .as_deref()
        .map(|name| format!(" by {}", name))
        .unwrap_or_default();
    push_emote_notification(
        &channel,
        &format!(
            "7TV active emote set was removed{}; cleared {} emotes",
            actor_suffix, removed
        ),
    );
}

fn active_set_refresh_is_current(channel: &str, set_id: &str) -> bool {
    ACTIVE_SET_REFRESH_TARGETS
        .get(channel)
        .is_some_and(|target| target.value() == set_id)
}

fn clear_active_set_refresh_target(channel: &str, set_id: &str) {
    ACTIVE_SET_REFRESH_TARGETS.remove_if(channel, |_, target| target == set_id);
}

async fn replay_eventapi_path(path: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    if path.is_dir() {
        let entries =
            std::fs::read_dir(path).map_err(|e| format!("read_dir {}: {}", path.display(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("read_dir entry: {}", e))?;
            let file_path = entry.path();
            let is_json = file_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
            if is_json {
                files.push(file_path);
            }
        }
        files.sort();
    } else {
        files.push(path.to_path_buf());
    }

    if files.is_empty() {
        return Err(format!("no replay JSON files found at {}", path.display()));
    }

    crate::log_emotes!(
        info,
        "7TV EventAPI replay: replaying {} file(s) from {}",
        files.len(),
        path.display()
    );

    let mut message_count = 0usize;
    for file in files {
        let content = std::fs::read_to_string(&file)
            .map_err(|e| format!("read {}: {}", file.display(), e))?;
        let parsed = parse_replay_content(&content)
            .map_err(|e| format!("parse {}: {}", file.display(), e))?;
        for value in parsed {
            replay_eventapi_value(value).await;
            message_count += 1;
        }
    }

    crate::log_emotes!(
        info,
        "7TV EventAPI replay: submitted {} replay message(s)",
        message_count
    );
    Ok(())
}

fn parse_replay_content(content: &str) -> Result<Vec<serde_json::Value>, String> {
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(serde_json::Value::Array(values)) => Ok(values),
        Ok(value) => Ok(vec![value]),
        Err(json_err) => {
            let mut values = Vec::new();
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                    continue;
                }
                values.push(
                    serde_json::from_str::<serde_json::Value>(trimmed).map_err(|e| {
                        format!(
                            "invalid JSON document ({}) and invalid JSONL at line {} ({})",
                            json_err,
                            idx + 1,
                            e
                        )
                    })?,
                );
            }
            if values.is_empty() {
                Err(format!("no replay messages found ({})", json_err))
            } else {
                Ok(values)
            }
        }
    }
}

async fn replay_eventapi_value(value: serde_json::Value) {
    if let Ok(msg) = serde_json::from_value::<EventApiMessage>(value.clone()) {
        match msg.op {
            op::DISPATCH => {
                handle_dispatch(&msg.d).await;
            }
            op::HELLO if replay_only() => {
                replay_message_through_handler(&value, msg.op).await;
            }
            op::HELLO => {
                crate::log_emotes!(info, "7TV EventAPI replay: HELLO fixture observed");
            }
            op::HEARTBEAT => {
                crate::log_emotes!(debug, "7TV EventAPI replay: HEARTBEAT fixture observed");
            }
            op::RECONNECT | op::ERROR | op::END_OF_STREAM if replay_only() => {
                replay_message_through_handler(&value, msg.op).await;
            }
            op::RECONNECT | op::ERROR | op::END_OF_STREAM => {
                crate::log_emotes!(
                    info,
                    "7TV EventAPI replay: opcode {} fixture observed without mutating the live connection",
                    msg.op
                );
            }
            other => {
                crate::log_emotes!(debug, "7TV EventAPI replay: ignored opcode {}", other);
            }
        }
        return;
    }

    if value.get("type").and_then(|v| v.as_str()).is_some() {
        handle_dispatch(&value).await;
        return;
    }

    crate::log_emotes!(
        debug,
        "7TV EventAPI replay: ignored non-dispatch JSON value"
    );
}

async fn replay_message_through_handler(value: &serde_json::Value, opcode: u32) {
    let mut health = ConnectionHealth::new();
    let mut subscriptions = ConnectionSubscriptions::new(None);
    match handle_message(&value.to_string(), &mut health, &mut subscriptions).await {
        Ok(control) => crate::log_emotes!(
            info,
            "7TV EventAPI replay: opcode {} produced {:?} connection control",
            opcode,
            control
        ),
        Err(error) => crate::log_emotes!(
            warn,
            "7TV EventAPI replay: opcode {} control handling failed: {}",
            opcode,
            error
        ),
    }
}

fn log_dispatch_changes(
    body: &serde_json::Value,
    channel: &str,
    display_channel: &str,
    is_global: bool,
    actor: Option<String>,
) -> DispatchChanges {
    let mut changes = DispatchChanges::default();
    let patch_channel = (!is_global).then_some(channel);
    let is_personal = channel == super::PERSONAL_SEVENTV_CHANNEL;

    if let Some(pushed) = body.get("pushed").and_then(|v| v.as_array()) {
        for item in pushed.iter().filter(|item| is_emote_update_item(item)) {
            let name = extract_emote_name_owned(item);
            if name.is_empty() {
                continue;
            }
            changes.summary.added += 1;
            crate::backend::metrics::record_emote_event("7tv.dispatch.added");
            if let Some(value) = dispatch_new_value(item) {
                match super::seventv_dispatch_emote(value, patch_channel) {
                    Some(emote) => changes.patch.added.push(emote),
                    None => changes.requires_full_refresh = true,
                }
            } else {
                changes.requires_full_refresh = true;
            }
            crate::log_emotes!(
                info,
                "7TV EventAPI: emote '{}' added to #{}",
                name,
                display_channel
            );
            if !is_global && !is_personal {
                changes.notifications.push(EmoteSetNotification::Added {
                    actor: actor.clone(),
                    name,
                });
            }
        }
    }

    for removed_key in ["pulled", "removed"] {
        if let Some(removed) = body.get(removed_key).and_then(|v| v.as_array()) {
            for item in removed.iter().filter(|item| is_emote_update_item(item)) {
                let name = extract_old_emote_name_owned(item);
                if name.is_empty() {
                    changes.requires_full_refresh = true;
                    continue;
                }
                changes.summary.removed += 1;
                let identity = extract_old_emote_identity(item);
                if identity.is_empty() {
                    changes.requires_full_refresh = true;
                } else {
                    changes.patch.removed.push(identity);
                }
                crate::backend::metrics::record_emote_event("7tv.dispatch.removed");
                crate::log_emotes!(
                    info,
                    "7TV EventAPI: emote '{}' removed from #{}",
                    name,
                    display_channel
                );
                if !is_global && !is_personal {
                    changes.notifications.push(EmoteSetNotification::Removed {
                        actor: actor.clone(),
                        name,
                    });
                }
            }
        }
    }

    if let Some(updated) = body.get("updated").and_then(|v| v.as_array()) {
        for item in updated.iter().filter(|item| is_emote_update_item(item)) {
            let old_name = extract_old_emote_name_owned(item);
            let new_name = extract_emote_name_owned(item);

            if old_name.is_empty() && new_name.is_empty() {
                changes.requires_full_refresh = true;
                continue;
            }

            changes.summary.updated += 1;
            let old_identity = extract_old_emote_identity(item);
            if let Some(value) = dispatch_new_value(item) {
                match super::seventv_dispatch_emote(value, patch_channel) {
                    Some(emote) => changes.patch.updated.push(super::SevenTvEmoteUpdate {
                        old: old_identity,
                        emote,
                    }),
                    None => changes.requires_full_refresh = true,
                }
            } else {
                changes.requires_full_refresh = true;
            }
            crate::backend::metrics::record_emote_event("7tv.dispatch.updated");
            if !old_name.is_empty() && !new_name.is_empty() && old_name != new_name {
                crate::log_emotes!(
                    info,
                    "7TV EventAPI: emote '{}' renamed to '{}' in #{}",
                    old_name,
                    new_name,
                    display_channel
                );
                if !is_global && !is_personal {
                    changes.notifications.push(EmoteSetNotification::Renamed {
                        actor: actor.clone(),
                        old_name,
                        new_name,
                    });
                }
            } else {
                let name = if !new_name.is_empty() {
                    new_name
                } else {
                    old_name
                };
                crate::log_emotes!(
                    info,
                    "7TV EventAPI: emote '{}' updated in #{}",
                    name,
                    display_channel
                );
            }
        }
    }

    changes
}

fn schedule_refresh_after_dispatch(
    set_id: &str,
    channel: &str,
    is_global: bool,
    changes: DispatchChanges,
) {
    queue_pending_refresh(PendingRefresh {
        set_id: set_id.to_string(),
        channel: channel.to_string(),
        is_global,
        added: changes.summary.added,
        removed: changes.summary.removed,
        updated: changes.summary.updated,
        requires_full_refresh: changes.requires_full_refresh,
        patch: changes.patch,
        notifications: changes.notifications,
        fetch_attempt: 0,
    });
}

fn queue_pending_refresh(refresh: PendingRefresh) {
    let key = pending_refresh_key(&refresh.set_id, &refresh.channel);
    {
        let mut pending = PENDING_REFRESHES.lock_or_recover();
        match pending.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let queued = entry.get_mut();
                queued.added += refresh.added;
                queued.removed += refresh.removed;
                queued.updated += refresh.updated;
                queued.is_global = refresh.is_global;
                queued.requires_full_refresh |= refresh.requires_full_refresh;
                queued.patch.merge(refresh.patch);
                queued.notifications.extend(refresh.notifications);
                queued.fetch_attempt = queued.fetch_attempt.max(refresh.fetch_attempt);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(refresh);
            }
        }
    }

    if REFRESH_WORKER_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        spawn_refresh_worker();
    }
}

fn pending_refresh_key(set_id: &str, channel: &str) -> String {
    format!("{set_id}|{channel}")
}

fn spawn_refresh_worker() {
    if let Err(e) = std::thread::Builder::new()
        .name("7tv-refresh-coalesce".into())
        .spawn(|| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    REFRESH_WORKER_RUNNING.store(false, Ordering::SeqCst);
                    crate::log_emotes!(
                        warn,
                        "7TV EventAPI: failed to create refresh runtime: {}",
                        e
                    );
                    return;
                }
            };

            loop {
                std::thread::sleep(Duration::from_millis(DISPATCH_REFRESH_DEBOUNCE_MS));

                let batch: Vec<PendingRefresh> = {
                    let mut pending = PENDING_REFRESHES.lock_or_recover();
                    pending.drain().map(|(_, refresh)| refresh).collect()
                };

                if !batch.is_empty() {
                    run_refresh_batch(&runtime, batch);
                }

                REFRESH_WORKER_RUNNING.store(false, Ordering::SeqCst);
                if PENDING_REFRESHES.lock_or_recover().is_empty() {
                    break;
                }

                if REFRESH_WORKER_RUNNING
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    break;
                }
            }
        })
    {
        REFRESH_WORKER_RUNNING.store(false, Ordering::SeqCst);
        crate::log_emotes!(
            warn,
            "7TV EventAPI: failed to spawn coalesced refresh worker: {}",
            e
        );
    }
}

fn run_refresh_batch(runtime: &tokio::runtime::Runtime, batch: Vec<PendingRefresh>) {
    let batch_started = std::time::Instant::now();
    let batch_len = batch.len();
    crate::log_emotes!(
        info,
        "7TV EventAPI: refreshing {} coalesced emote set update(s)",
        batch_len
    );
    crate::backend::metrics::record_emote_event("7tv.refresh_batch");

    runtime.block_on(async move {
        for refresh in batch {
            let is_personal =
                !refresh.is_global && refresh.channel == super::PERSONAL_SEVENTV_CHANNEL;
            let display_target = if refresh.is_global {
                "global"
            } else if is_personal {
                "personal"
            } else {
                refresh.channel.as_str()
            };

            if !channel_maps_to_set(&refresh.channel, &refresh.set_id) {
                crate::backend::metrics::record_emote_event(
                    "7tv.patch.stale_owner_dropped",
                );
                if should_warn_drop(format!("stale-owner:{}", refresh.set_id)) {
                    crate::log_emotes!(
                        warn,
                        "7TV EventAPI: dropping debounced update for #{} set {}; channel now maps to {:?} (active-set target {:?})",
                        refresh.channel,
                        refresh.set_id,
                        CHANNEL_TO_SET.get(&refresh.channel).map(|set| set.clone()),
                        ACTIVE_SET_REFRESH_TARGETS
                            .get(&refresh.channel)
                            .map(|set| set.clone())
                    );
                }
                continue;
            }

            super::invalidate_seventv_fetch_cache_for_event(
                (!refresh.is_global).then_some(refresh.channel.as_str()),
                refresh.is_global,
            );

            let cached_set_differs = !refresh.is_global
                && !is_personal
                && super::EMOTE_CACHE
                    .channel_emote_set_ids
                    .get(&refresh.channel)
                    .is_none_or(|cached| cached.value() != &refresh.set_id);
            if cached_set_differs {
                // Rows still belong to another set (e.g. a failed active-set
                // switch); patching them would mix both sets.
                crate::backend::metrics::record_emote_event("7tv.patch.cached_set_mismatch");
            }
            if !refresh.requires_full_refresh && !cached_set_differs {
                let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
                if !channel_maps_to_set(&refresh.channel, &refresh.set_id) {
                    crate::backend::metrics::record_emote_event(
                        "7tv.patch.stale_owner_dropped_before_patch",
                    );
                    continue;
                }
                let patch_result = if refresh.is_global {
                    super::apply_seventv_global_emote_patch(refresh.patch.clone()).await
                } else if is_personal {
                    super::apply_seventv_user_emote_patch(refresh.patch.clone()).await
                } else {
                    super::apply_seventv_channel_emote_patch(&refresh.channel, refresh.patch.clone())
                        .await
                };

                match patch_result {
                    Ok(outcome)
                        if outcome.changed > 0 && outcome.unmatched_old_identities == 0 =>
                    {
                        crate::backend::metrics::record_emote_event("7tv.patch.applied");
                        crate::log_emotes!(
                            info,
                            "7TV EventAPI: applied direct patch for #{} set {} (changed={}, add/remove/update={}/{}/{})",
                            display_target,
                            refresh.set_id,
                            outcome.changed,
                            refresh.added,
                            refresh.removed,
                            refresh.updated
                        );
                        emit_applied_emote_set_notifications(&refresh);
                        crate::managers::frame_rate_controller::record_activity();
                        crate::managers::frame_rate_controller::wakeup();
                        continue;
                    }
                    Ok(outcome) => {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.incomplete_cache_match",
                        );
                        crate::log_emotes!(
                            debug,
                            "7TV EventAPI: direct patch was incomplete for #{} set {} (changed={}, unmatched_old_identities={}); falling back to authoritative reconciliation",
                            display_target,
                            refresh.set_id,
                            outcome.changed,
                            outcome.unmatched_old_identities
                        );
                    }
                    Err(e) => {
                        crate::backend::metrics::record_emote_event("7tv.patch.fallback_refresh");
                        crate::log_emotes!(
                            debug,
                            "7TV EventAPI: direct patch unavailable for #{} set {}: {}; falling back to authoritative reconciliation",
                            display_target,
                            refresh.set_id,
                            e
                        );
                    }
                }
            } else {
                crate::backend::metrics::record_emote_event("7tv.patch.partial_payload_refresh");
            }

            crate::backend::metrics::record_emote_event(
                "7tv.patch.exact_set_reconcile.started",
            );
            crate::log_emotes!(
                info,
                "7TV EventAPI: reconciling {} from authoritative set {} after add/remove/update burst ({}/{}/{})",
                display_target,
                refresh.set_id,
                refresh.added,
                refresh.removed,
                refresh.updated
            );

            let emote_channel = (!refresh.is_global).then_some(refresh.channel.as_str());
            match super::fetch_seventv_emote_set_by_id(&refresh.set_id, emote_channel).await {
                Ok(super::SevenTvExactSetFetchOutcome::Present {
                    emotes,
                    emote_set_id,
                }) => {
                    let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
                    if !channel_maps_to_set(&refresh.channel, &refresh.set_id) {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.stale_owner_dropped_after_fetch",
                        );
                        crate::log_emotes!(
                            info,
                            "7TV EventAPI: dropping authoritative set {} response for {} because ownership changed while fetching",
                            refresh.set_id,
                            display_target
                        );
                        continue;
                    }

                    let applied_set_id =
                        emote_set_id.unwrap_or_else(|| refresh.set_id.clone());
                    let count = if refresh.is_global {
                        super::apply_seventv_global_emote_set(emotes).await
                    } else if is_personal {
                        super::apply_seventv_user_emote_set(emotes).await
                    } else {
                        super::apply_seventv_channel_emote_rows(
                            &refresh.channel,
                            emotes,
                            &applied_set_id,
                        )
                        .await
                    };
                    crate::backend::metrics::record_emote_event(
                        "7tv.patch.exact_set_reconcile.applied",
                    );
                    crate::log_emotes!(
                        info,
                        "7TV EventAPI: authoritative set reconcile applied for {} set {} ({} 7TV emotes; other providers preserved)",
                        display_target,
                        applied_set_id,
                        count
                    );
                    emit_applied_emote_set_notifications(&refresh);
                }
                Ok(super::SevenTvExactSetFetchOutcome::TerminallyAbsent {
                    emote_set_id,
                }) => {
                    let _apply_guard = LIVE_SET_APPLY_LOCK.lock().await;
                    if !channel_maps_to_set(&refresh.channel, &refresh.set_id)
                        || !retire_terminal_refresh_owner(
                            &refresh.channel,
                            &refresh.set_id,
                            refresh.is_global,
                        )
                    {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.exact_set_reconcile.terminal_stale",
                        );
                        continue;
                    }

                    let removed = if refresh.is_global {
                        super::clear_seventv_global_emote_set_if_matches(&refresh.set_id).await
                    } else if is_personal {
                        Some(super::apply_seventv_user_emote_set(Vec::new()).await)
                    } else {
                        super::clear_seventv_channel_emote_rows_if_set(
                            &refresh.channel,
                            &refresh.set_id,
                        )
                        .await
                    };
                    if let Some(removed) = removed {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.exact_set_reconcile.terminal_cleared",
                        );
                        crate::log_emotes!(
                            info,
                            "7TV EventAPI: exact set {} is terminally absent for {}; cleared {} provider rows without disturbing other owners",
                            emote_set_id,
                            display_target,
                            removed
                        );
                        emit_terminally_removed_notifications(&refresh);
                    } else {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.exact_set_reconcile.terminal_stale",
                        );
                        crate::log_emotes!(
                            warn,
                            "7TV EventAPI: terminal set {} for {} no longer matched the cache; preserved current provider rows",
                            emote_set_id,
                            display_target
                        );
                    }
                }
                Err(e) => {
                    crate::backend::metrics::record_emote_event(
                        "7tv.patch.exact_set_reconcile.preserved",
                    );
                    if let Some(delay) = SET_FETCH_RETRY_DELAYS_SECS.get(refresh.fetch_attempt) {
                        crate::log_emotes!(
                            warn,
                            "7TV EventAPI: authoritative set reconcile failed for {} set {} (attempt {}); retrying in {}s: {}",
                            display_target,
                            refresh.set_id,
                            refresh.fetch_attempt + 1,
                            delay,
                            e
                        );
                        schedule_reconcile_retry(&refresh, Duration::from_secs(*delay));
                    } else {
                        crate::backend::metrics::record_emote_event(
                            "7tv.patch.exact_set_reconcile.gave_up",
                        );
                        crate::log_emotes!(
                            warn,
                            "7TV EventAPI: authoritative set reconcile gave up for {} set {} after {} attempts; the next channel refresh or F5 reloads it: {}",
                            display_target,
                            refresh.set_id,
                            refresh.fetch_attempt + 1,
                            e
                        );
                    }
                }
            }

            crate::managers::frame_rate_controller::record_activity();
            crate::managers::frame_rate_controller::wakeup();
        }
    });

    crate::backend::metrics::record_emote_stage(
        "7tv.refresh_batch",
        batch_started.elapsed(),
        batch_len,
    );
}

/// Requeue a refresh whose authoritative fetch failed. The pending
/// notifications ride along so the chat row still appears once it applies.
fn schedule_reconcile_retry(refresh: &PendingRefresh, delay: Duration) {
    let mut retry = refresh.clone();
    retry.fetch_attempt += 1;
    retry.requires_full_refresh = true;
    crate::backend::bridge::get_runtime().spawn(async move {
        sleep(delay).await;
        if crate::backend::is_shutdown_started()
            || !channel_maps_to_set(&retry.channel, &retry.set_id)
        {
            return;
        }
        crate::backend::metrics::record_emote_event("7tv.patch.exact_set_reconcile.retry");
        queue_pending_refresh(retry);
    });
}

/// Rate-limit warn-level diagnostics per drop site/object.
fn should_warn_drop(key: String) -> bool {
    let now = Instant::now();
    let mut last = DROP_WARN_LAST_AT.lock_or_recover();
    if last.len() > 256 {
        last.retain(|_, at| now.duration_since(*at) < DROP_WARN_INTERVAL);
    }
    if last
        .get(&key)
        .is_some_and(|at| now.duration_since(*at) < DROP_WARN_INTERVAL)
    {
        return false;
    }
    last.insert(key, now);
    true
}

/// Whether a dispatch body lists any change keyed to the set's emotes.
fn dispatch_has_emote_items(body: &serde_json::Value) -> bool {
    ["pushed", "pulled", "removed", "updated"]
        .iter()
        .any(|key| {
            body.get(key)
                .and_then(|v| v.as_array())
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item.get("key").and_then(|v| v.as_str()) == Some("emotes"))
                })
        })
}

fn is_emote_update_item(item: &serde_json::Value) -> bool {
    match item.get("key").and_then(|v| v.as_str()) {
        Some("emotes") => true,
        Some(_) => false,
        None => has_emote_payload(item),
    }
}

fn emit_applied_emote_set_notifications(refresh: &PendingRefresh) {
    if refresh.is_global || refresh.channel == super::PERSONAL_SEVENTV_CHANNEL {
        return;
    }
    for notification in &refresh.notifications {
        push_emote_notification(&refresh.channel, &notification.format());
    }
}

fn terminally_removed_notifications(
    notifications: &[EmoteSetNotification],
) -> impl Iterator<Item = &EmoteSetNotification> {
    notifications
        .iter()
        .filter(|notification| matches!(notification, EmoteSetNotification::Removed { .. }))
}

fn emit_terminally_removed_notifications(refresh: &PendingRefresh) {
    if refresh.is_global || refresh.channel == super::PERSONAL_SEVENTV_CHANNEL {
        return;
    }
    for notification in terminally_removed_notifications(&refresh.notifications) {
        push_emote_notification(&refresh.channel, &notification.format());
    }
}

fn dispatch_new_value(item: &serde_json::Value) -> Option<&serde_json::Value> {
    item.get("value").or_else(|| {
        if item.get("data").is_some() && item.get("id").is_some() {
            Some(item)
        } else {
            None
        }
    })
}

fn has_emote_payload(item: &serde_json::Value) -> bool {
    !extract_emote_name_owned(item).is_empty() || !extract_old_emote_name_owned(item).is_empty()
}

fn extract_identity_from_value(value: &serde_json::Value) -> super::SevenTvEmoteIdentity {
    super::SevenTvEmoteIdentity {
        id: value
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

fn extract_old_emote_identity(item: &serde_json::Value) -> super::SevenTvEmoteIdentity {
    if let Some(old_value) = item.get("old_value") {
        let identity = extract_identity_from_value(old_value);
        if !identity.is_empty() {
            return identity;
        }
    }
    extract_identity_from_value(item)
}

fn extract_old_emote_name_owned(item: &serde_json::Value) -> String {
    if let Some(name) = item
        .get("old_value")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
    {
        return name.to_string();
    }
    extract_emote_name_owned(item)
}

/// Extract emote name as an owned String from a 7TV dispatch item.
fn extract_emote_name_owned(item: &serde_json::Value) -> String {
    if let Some(name) = item
        .get("value")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
    {
        return name.to_string();
    }
    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
        return name.to_string();
    }
    if let Some(name) = item
        .get("data")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
    {
        return name.to_string();
    }
    String::new()
}

/// Post an emote change system message to a channel's chat.
fn push_emote_notification(channel: &str, text: &str) {
    use crate::backend::models::SystemMessageType;
    crate::backend::irc::add_system_message(channel, text, SystemMessageType::Info);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn reconnect_backoff_resets_only_after_a_healthy_session() {
        let grown = 8_000;
        assert_eq!(
            next_reconnect_delay_ms(grown, Duration::from_secs(2), true, false),
            grown
        );
        assert_eq!(
            next_reconnect_delay_ms(grown, Duration::from_secs(20), true, false),
            RECONNECT_BASE_MS
        );
        assert_eq!(
            next_reconnect_delay_ms(grown, Duration::from_secs(20), false, false),
            grown
        );
        assert_eq!(
            next_reconnect_delay_ms(grown, HEALTHY_CONNECTION_LIFETIME, false, false),
            RECONNECT_BASE_MS
        );
        assert_eq!(
            next_reconnect_delay_ms(RECONNECT_BASE_MS, Duration::from_secs(1), true, true),
            RECONNECT_SLOW_BASE_MS
        );
    }

    #[test]
    fn formats_single_emote_rows_with_actor_or_capitalized_fallback() {
        assert_eq!(
            EmoteSetNotification::Added {
                actor: Some("fixture_editor".into()),
                name: "NuhUh".into(),
            }
            .format(),
            "fixture_editor added 7TV emote NuhUh."
        );
        assert_eq!(
            EmoteSetNotification::Removed {
                actor: None,
                name: "NuhUh".into(),
            }
            .format(),
            "Removed 7TV emote NuhUh."
        );
        assert_eq!(
            EmoteSetNotification::Added {
                actor: None,
                name: "NuhUh".into(),
            }
            .format(),
            "Added 7TV emote NuhUh."
        );
        assert_eq!(
            EmoteSetNotification::Renamed {
                actor: Some("fixture_editor".into()),
                old_name: "Old".into(),
                new_name: "New".into(),
            }
            .format(),
            "fixture_editor renamed 7TV emote Old to New."
        );
        assert_eq!(
            EmoteSetNotification::Renamed {
                actor: None,
                old_name: "Old".into(),
                new_name: "New".into(),
            }
            .format(),
            "Renamed 7TV emote Old to New."
        );
    }

    #[test]
    fn ack_requires_command_and_echoed_subscription_data() {
        let payload = serde_json::json!({
            "command": "SUBSCRIBE",
            "data": {
                "type": "emote_set.update",
                "condition": { "object_id": "set-id" }
            }
        });
        assert_eq!(
            ack_subscription_command(&payload),
            Some(SubscriptionCommand::Subscribe(SubscriptionKey::emote_set(
                "set-id"
            )))
        );
        assert!(
            ack_subscription_command(&serde_json::json!({
                "command": "SUBSCRIBE",
                "data": { "type": "emote_set.update" }
            }))
            .is_none()
        );
    }

    #[test]
    fn entitlement_subscriptions_use_the_channel_condition() {
        let [create, _delete] = SubscriptionKey::channel_entitlements("12345");
        assert_eq!(
            create.condition(),
            serde_json::json!({ "ctx": "channel", "platform": "TWITCH", "id": "12345" })
        );
        let payload = serde_json::json!({
            "command": "SUBSCRIBE",
            "data": { "type": "entitlement.create", "condition": create.condition() }
        });
        assert_eq!(
            ack_subscription_command(&payload),
            Some(SubscriptionCommand::Subscribe(create))
        );
    }

    #[test]
    fn active_subscriptions_change_only_after_matching_ack() {
        let key = SubscriptionKey::emote_set("set-id");
        let mut active = HashSet::new();
        let mut pending = vec![PendingSubscriptionCommand {
            command: SubscriptionCommand::Subscribe(key.clone()),
            sent_at: Instant::now(),
        }];
        assert!(active.is_empty());
        assert!(!acknowledge_subscription_command(
            &mut active,
            &mut pending,
            &SubscriptionCommand::Unsubscribe(key.clone())
        ));
        assert!(active.is_empty());
        assert!(acknowledge_subscription_command(
            &mut active,
            &mut pending,
            &SubscriptionCommand::Subscribe(key)
        ));
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn commands_wait_for_hello_and_resume_ack() {
        assert_eq!(command_gate(false, false), CommandGate::AwaitHello);
        assert_eq!(command_gate(true, true), CommandGate::AwaitResumeAck);
        assert_eq!(command_gate(true, false), CommandGate::Ready);
        assert_eq!(in_flight_command_count(true, 0), 1);
    }

    #[test]
    fn unacked_resume_or_command_requires_fresh_reconnect() {
        let now = Instant::now();
        assert!(connection_drop_requires_fresh(true, 0));
        assert!(connection_drop_requires_fresh(false, 1));
        assert!(!connection_drop_requires_fresh(false, 0));
        assert!(!connection_drop_requeues_unacked(true));
        assert!(connection_drop_requeues_unacked(false));
        assert!(command_ack_expired_at(
            true,
            Some(now - COMMAND_ACK_TIMEOUT),
            &[],
            now
        ));
        assert!(command_ack_expired_at(
            false,
            None,
            &[PendingSubscriptionCommand {
                command: SubscriptionCommand::Subscribe(SubscriptionKey::emote_set("set-id")),
                sent_at: now - COMMAND_ACK_TIMEOUT,
            }],
            now
        ));
    }

    #[test]
    fn command_error_with_in_flight_mutation_forces_fresh_reconnect() {
        assert!(!command_error_requires_fresh(0));
        assert!(command_error_requires_fresh(1));
    }

    #[test]
    fn opposite_intent_waits_for_the_first_ack_before_reconciling() {
        let key = SubscriptionKey::emote_set("set-id");
        let mut active = HashSet::new();
        let mut pending = vec![PendingSubscriptionCommand {
            command: SubscriptionCommand::Subscribe(key.clone()),
            sent_at: Instant::now(),
        }];
        let desired = HashSet::new();
        assert!(desired_subscription_commands(&active, &pending, &desired).is_empty());
        assert!(acknowledge_subscription_command(
            &mut active,
            &mut pending,
            &SubscriptionCommand::Subscribe(key.clone())
        ));
        assert_eq!(
            desired_subscription_commands(&active, &pending, &desired),
            vec![SubscriptionCommand::Unsubscribe(key)]
        );
    }

    #[test]
    fn live_coverage_requires_the_channel_exact_set_key() {
        let mut active = HashSet::new();
        active.insert(SubscriptionKey::emote_set("another-set"));
        assert!(!active_subscriptions_cover_set(&active, "set-id"));
        active.insert(SubscriptionKey::emote_set("set-id"));
        assert!(active_subscriptions_cover_set(&active, "set-id"));
        assert!(!subscription_coverage_is_live(
            true, 0, 1, &active, "set-id"
        ));
        assert!(subscription_coverage_is_live(true, 0, 0, &active, "set-id"));
    }

    #[test]
    fn terminally_absent_only_selects_removed_rows() {
        let notifications = vec![
            EmoteSetNotification::Added {
                actor: None,
                name: "Added".into(),
            },
            EmoteSetNotification::Removed {
                actor: None,
                name: "Removed".into(),
            },
            EmoteSetNotification::Renamed {
                actor: None,
                old_name: "Old".into(),
                new_name: "New".into(),
            },
        ];
        let names = terminally_removed_notifications(&notifications)
            .map(EmoteSetNotification::format)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Removed 7TV emote Removed."]);
    }

    #[test]
    fn dispatch_add_builds_a_patch_and_an_actor_notification() {
        let body = serde_json::json!({
            "id": "set-id",
            "actor": { "display_name": "Mod" },
            "pushed": [{
                "key": "emotes",
                "index": 0,
                "value": {
                    "id": "emote-id",
                    "name": "NewEmote",
                    "data": {
                        "name": "NewEmote",
                        "host": {
                            "url": "//cdn.7tv.app/emote/emote-id",
                            "files": [{ "name": "1x.webp", "format": "WEBP", "width": 28, "height": 28 }]
                        }
                    }
                }
            }]
        });
        let changes = log_dispatch_changes(
            &body,
            "channel",
            "channel",
            false,
            dispatch_actor_name(&body),
        );
        assert_eq!(changes.summary.added, 1);
        assert!(!changes.requires_full_refresh);
        assert_eq!(changes.patch.added.len(), 1);
        assert_eq!(changes.patch.added[0].name.as_ref(), "NewEmote");
        assert_eq!(
            changes
                .notifications
                .iter()
                .map(EmoteSetNotification::format)
                .collect::<Vec<_>>(),
            vec!["Mod added 7TV emote NewEmote."]
        );
    }

    #[test]
    fn unreadable_emote_items_are_detected_for_full_reconcile() {
        let body = serde_json::json!({
            "id": "set-id",
            "pushed": [{ "key": "emotes", "index": 0, "value": null }]
        });
        let changes = log_dispatch_changes(&body, "channel", "channel", false, None);
        assert_eq!(changes.summary.added, 0);
        assert!(dispatch_has_emote_items(&body));
        assert!(!dispatch_has_emote_items(&serde_json::json!({
            "id": "set-id",
            "updated": [{ "key": "name", "value": "Renamed set" }]
        })));
    }

    #[test]
    fn failed_or_partial_resume_is_not_trusted() {
        let ack =
            |data: serde_json::Value| serde_json::json!({ "command": "RESUME", "data": data });
        assert_eq!(
            resume_ack_outcome(
                &ack(serde_json::json!({ "success": true, "subscriptions_restored": 4 })),
                4
            ),
            ResumeAckOutcome::Restored
        );
        // Live 7TV reply to resuming a cleanly closed session.
        assert_eq!(
            resume_ack_outcome(
                &ack(serde_json::json!({
                    "success": false,
                    "dispatches_replayed": 0,
                    "subscriptions_restored": 0
                })),
                4
            ),
            ResumeAckOutcome::Empty
        );
        assert_eq!(
            resume_ack_outcome(
                &ack(serde_json::json!({ "success": true, "subscriptions_restored": 1 })),
                4
            ),
            ResumeAckOutcome::Partial
        );
        assert_eq!(
            resume_ack_outcome(&serde_json::json!({ "command": "RESUME" }), 4),
            ResumeAckOutcome::Restored
        );
    }

    #[test]
    fn subscription_limit_admits_emote_sets_first() {
        let commands = vec![
            SubscriptionCommand::Subscribe(SubscriptionKey::channel_entitlements("1")[0].clone()),
            SubscriptionCommand::Subscribe(SubscriptionKey::user("user")),
            SubscriptionCommand::Unsubscribe(SubscriptionKey::emote_set("old-set")),
            SubscriptionCommand::Subscribe(SubscriptionKey::emote_set("set")),
        ];
        let (admitted, deferred) = cap_subscribe_commands(commands.clone(), 8, 10);
        assert_eq!(deferred, 1);
        assert_eq!(
            admitted,
            vec![
                SubscriptionCommand::Unsubscribe(SubscriptionKey::emote_set("old-set")),
                SubscriptionCommand::Subscribe(SubscriptionKey::emote_set("set")),
                SubscriptionCommand::Subscribe(SubscriptionKey::user("user")),
            ]
        );
        let (unlimited, deferred) = cap_subscribe_commands(commands, 8, 0);
        assert_eq!((unlimited.len(), deferred), (4, 0));
    }

    #[test]
    fn drop_warnings_are_rate_limited_per_key() {
        let key = "test-drop-warning-rate-limit".to_string();
        assert!(should_warn_drop(key.clone()));
        assert!(!should_warn_drop(key));
    }
}
