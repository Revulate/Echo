//! Twitch EventSub WebSocket client for real-time event notifications
//!
//! Connects to Twitch EventSub to receive live events like subscriptions,
//! raids, channel point redemptions, and stream status changes.
//!
//! Protocol documentation: https://dev.twitch.tv/docs/eventsub/handling-websocket-events/

use super::deduplicator::MessageDeduplicator;
use super::lock_utils::{MutexExt, RwLockExt};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio_tungstenite::tungstenite::Message;

use super::components::event_notification::{
    TwitchEventType, push_event, update_active_prediction,
};

mod notification;

/// EventSub WebSocket URL with extended keepalive timeout
/// Default is 10s which is too short for initial subscription setup.
/// Using 30s gives enough time for API calls to get broadcaster IDs.
const EVENTSUB_WS_URL: &str = "wss://eventsub.wss.twitch.tv/ws?keepalive_timeout_seconds=30";

/// Twitch leaves the old socket available for a short overlap while clients establish
/// the server-provided reconnect URL. Bound each candidate attempt so the supervisor
/// still has time to retry or fall back to a fresh session.
const EVENTSUB_HANDOFF_ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const EVENTSUB_RECONNECT_GRACE_MS: u64 = 30_000;

/// Helix EventSub subscriptions endpoint
const HELIX_EVENTSUB_URL: &str = "https://api.twitch.tv/helix/eventsub/subscriptions";

/// Recent Twitch delivery IDs retained to make retry/reconnect delivery idempotent.
const EVENTSUB_DELIVERY_ID_CACHE_SIZE: usize = 10_000;

/// Changes immediately whenever the authenticated EventSub identity becomes invalid.
/// Read tasks and in-flight subscription requests capture this value so an old account
/// cannot produce side effects or publish session state after a login/logout switch.
static EVENTSUB_AUTH_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubscriptionRelation {
    Any,
    Broadcaster,
    ModeratorOrBroadcaster,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubscriptionCondition {
    Broadcaster,
    RaidToBroadcaster,
    Moderator,
}

#[derive(Clone, Copy, Debug)]
struct SubscriptionSpec {
    sub_type: &'static str,
    version: &'static str,
    condition: SubscriptionCondition,
    scopes: &'static [&'static str],
    relation: SubscriptionRelation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubscriptionAttemptOutcome {
    Complete,
    DeferredByRateLimit,
    /// The attempt failed for a reason that can succeed later (expired token, 5xx,
    /// network error). Without this the channel would leave the pending queue and
    /// nothing would ever reconcile it back for the rest of the session.
    Retryable,
}

const PUBLIC_EVENTSUB_SPECS: &[SubscriptionSpec] = &[
    SubscriptionSpec {
        sub_type: "stream.online",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &[],
        relation: SubscriptionRelation::Any,
    },
    SubscriptionSpec {
        sub_type: "stream.offline",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &[],
        relation: SubscriptionRelation::Any,
    },
    SubscriptionSpec {
        sub_type: "channel.raid",
        version: "1",
        condition: SubscriptionCondition::RaidToBroadcaster,
        scopes: &[],
        relation: SubscriptionRelation::Any,
    },
];

const BROADCASTER_EVENTSUB_SPECS: &[SubscriptionSpec] = &[
    SubscriptionSpec {
        sub_type: "channel.subscribe",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:subscriptions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.subscription.message",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:subscriptions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.subscription.gift",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:subscriptions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.channel_points_custom_reward_redemption.add",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:redemptions", "channel:manage:redemptions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.poll.begin",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:polls", "channel:manage:polls"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.poll.progress",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:polls", "channel:manage:polls"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.poll.end",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:polls", "channel:manage:polls"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.prediction.begin",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:predictions", "channel:manage:predictions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.prediction.progress",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:predictions", "channel:manage:predictions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.prediction.lock",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:predictions", "channel:manage:predictions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.prediction.end",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:predictions", "channel:manage:predictions"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.hype_train.begin",
        version: "2",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:hype_train"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.hype_train.progress",
        version: "2",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:hype_train"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.hype_train.end",
        version: "2",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:hype_train"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.vip.add",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:vips", "channel:manage:vips"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.vip.remove",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:read:vips", "channel:manage:vips"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.moderator.add",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["moderation:read"],
        relation: SubscriptionRelation::Broadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.moderator.remove",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["moderation:read"],
        relation: SubscriptionRelation::Broadcaster,
    },
];

const MODERATOR_EVENTSUB_SPECS: &[SubscriptionSpec] = &[
    SubscriptionSpec {
        sub_type: "channel.ban",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:moderate"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.unban",
        version: "1",
        condition: SubscriptionCondition::Broadcaster,
        scopes: &["channel:moderate"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "automod.message.hold",
        version: "2",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:manage:automod"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "automod.message.update",
        version: "2",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:manage:automod"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.suspicious_user.message",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:suspicious_users"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.suspicious_user.update",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:suspicious_users"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.warning.acknowledge",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:warnings", "moderator:manage:warnings"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.warning.send",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:warnings", "moderator:manage:warnings"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.shield_mode.begin",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:shield_mode", "moderator:manage:shield_mode"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
    SubscriptionSpec {
        sub_type: "channel.shield_mode.end",
        version: "1",
        condition: SubscriptionCondition::Moderator,
        scopes: &["moderator:read:shield_mode", "moderator:manage:shield_mode"],
        relation: SubscriptionRelation::ModeratorOrBroadcaster,
    },
];

/// Used to suppress stream-live notifications during the configured startup grace period.
static EVENTSUB_NOTIFICATION_START_MS: LazyLock<u64> = LazyLock::new(crate::utils::unix_now_ms);

/// Message types from EventSub WebSocket
mod message_type {
    pub const SESSION_WELCOME: &str = "session_welcome";
    pub const SESSION_KEEPALIVE: &str = "session_keepalive";
    pub const NOTIFICATION: &str = "notification";
    pub const SESSION_RECONNECT: &str = "session_reconnect";
    pub const REVOCATION: &str = "revocation";
}

/// Root message structure from EventSub
#[derive(Debug, Deserialize)]
struct EventSubMessage {
    metadata: Metadata,
    payload: serde_json::Value,
}

/// Message metadata
#[derive(Debug, Deserialize)]
struct Metadata {
    message_id: String,
    message_type: String,
}

/// Session welcome payload
#[derive(Debug, Deserialize)]
struct WelcomePayload {
    session: WelcomeSessionInfo,
}

/// Session info from welcome message
#[derive(Debug, Deserialize)]
struct WelcomeSessionInfo {
    id: String,
    keepalive_timeout_seconds: u32,
}

/// Reconnect payload
#[derive(Debug, Deserialize)]
struct ReconnectPayload {
    session: ReconnectSessionInfo,
}

/// The reconnect control message deliberately carries a null keepalive timeout. Keep
/// it separate from the non-null welcome shape so a valid Twitch handoff cannot fail
/// deserialization before the reconnect URL is observed.
#[derive(Debug, Deserialize)]
struct ReconnectSessionInfo {
    #[serde(default)]
    reconnect_url: Option<String>,
}

/// Notification payload wrapper
#[derive(Debug, Deserialize)]
struct NotificationPayload {
    subscription: SubscriptionInfo,
    event: serde_json::Value,
}

/// Subscription info in notification
#[derive(Debug, Deserialize)]
struct SubscriptionInfo {
    #[serde(rename = "type")]
    sub_type: String,
}

/// Helix subscription request body
#[derive(Debug, Serialize)]
struct SubscriptionRequest {
    #[serde(rename = "type")]
    sub_type: String,
    version: String,
    condition: serde_json::Value,
    transport: TransportInfo,
}

#[derive(Debug, Serialize)]
struct TransportInfo {
    method: String,
    session_id: String,
}

/// Global EventSub client instance
pub static EVENTSUB_CLIENT: LazyLock<Arc<EventSubClient>> = LazyLock::new(EventSubClient::new);

/// Shared, pooled client for Helix subscription create/delete. The timeout keeps
/// a stalled request from hanging the supervisor before its keepalive check.
static EVENTSUB_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_eventsub_http_client));

fn build_eventsub_http_client() -> reqwest::Client {
    crate::backend::http::timed_client(Duration::from_secs(10))
}

/// Atomic flag to prevent multiple simultaneous connection attempts.
/// Uses compare-and-swap to ensure only one thread can be connecting at a time.
static CONNECTING: AtomicBool = AtomicBool::new(false);

/// EventSub WebSocket client
pub struct EventSubClient {
    /// WebSocket message sender
    sender: TokioMutex<Option<mpsc::Sender<Message>>>,
    /// Session ID from welcome message
    session_id: RwLock<Option<String>>,
    /// Keepalive timeout in seconds
    keepalive_timeout: RwLock<u32>,
    /// Reconnect URL (from reconnect message)
    reconnect_url: RwLock<Option<String>>,
    /// True while a server-requested handoff may preserve the current subscriptions.
    server_reconnect_pending: AtomicBool,
    /// Bounded wall-clock deadline for retrying the server-provided reconnect URL.
    server_reconnect_deadline_ms: AtomicU64,
    /// Subscribed channels (broadcaster_user_id -> subscription_ids)
    subscriptions: RwLock<HashMap<String, Vec<String>>>,
    /// Channel name to broadcaster ID mapping (for unsubscribe by name)
    channel_to_broadcaster: RwLock<HashMap<String, String>>,
    /// Connection status
    is_connected: RwLock<bool>,
    /// Last message received timestamp (unix epoch millis) for keepalive monitoring
    last_message_time: AtomicU64,
    /// Subscription types that failed with 403 Forbidden (lack of permissions)
    /// Key: "broadcaster_id:sub_type", prevents retrying subscriptions we don't have perms for
    forbidden_subs: RwLock<std::collections::HashSet<String>>,
    /// Subscription types that already exist (409 Conflict)
    /// Key: "broadcaster_id:sub_type"
    existing_subs: RwLock<std::collections::HashSet<String>>,
    /// Subscriptions rejected because the websocket session's total-cost
    /// budget is exhausted (429 whose body names the cost/subscription limit).
    /// Terminal for this session — retrying can never succeed; a new websocket
    /// session gets a fresh budget, so this clears wherever existing_subs does.
    cost_limited_subs: RwLock<std::collections::HashSet<String>>,
    /// Rate limit tracking - timestamp (millis) when rate limit expires.
    /// 0 = not rate limited, > 0 = rate limited until this timestamp.
    rate_limited_until: AtomicU64,
    /// Monotonic connection epoch used to ignore stale read-task cleanup.
    /// Incremented whenever we start a new connection or force-disconnect.
    active_connection_epoch: AtomicU64,
    /// Set when server asks us to reconnect via a specific URL.
    reconnect_requested: AtomicBool,
    /// Set when login/account/anonymous state invalidates the active user session.
    auth_reset_requested: AtomicBool,
    /// Bounded delivery-ID history checked before notification side effects.
    notification_ids: Mutex<MessageDeduplicator>,
    /// Single-flight validation snapshot for the current runtime token.
    token_authorization:
        TokioMutex<Option<(String, Arc<crate::backend::auth::TokenAuthorization>)>>,
}

impl EventSubClient {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sender: TokioMutex::new(None),
            session_id: RwLock::new(None),
            keepalive_timeout: RwLock::new(30),
            reconnect_url: RwLock::new(None),
            server_reconnect_pending: AtomicBool::new(false),
            server_reconnect_deadline_ms: AtomicU64::new(0),
            subscriptions: RwLock::new(HashMap::new()),
            channel_to_broadcaster: RwLock::new(HashMap::new()),
            is_connected: RwLock::new(false),
            last_message_time: AtomicU64::new(0),
            forbidden_subs: RwLock::new(std::collections::HashSet::new()),
            existing_subs: RwLock::new(std::collections::HashSet::new()),
            cost_limited_subs: RwLock::new(std::collections::HashSet::new()),
            rate_limited_until: AtomicU64::new(0),
            active_connection_epoch: AtomicU64::new(0),
            reconnect_requested: AtomicBool::new(false),
            auth_reset_requested: AtomicBool::new(false),
            notification_ids: Mutex::new(MessageDeduplicator::new(EVENTSUB_DELIVERY_ID_CACHE_SIZE)),
            token_authorization: TokioMutex::new(None),
        })
    }

    async fn authorization_for_token(
        &self,
        token: &str,
    ) -> Result<Arc<crate::backend::auth::TokenAuthorization>, String> {
        let normalized = crate::backend::auth::normalize_oauth_token(token);
        let mut cached = self.token_authorization.lock().await;
        if let Some((cached_token, authorization)) = cached.as_ref()
            && cached_token == normalized
        {
            return Ok(authorization.clone());
        }

        let authorization = crate::backend::auth::validate_token_authorization(normalized)
            .await?
            .ok_or_else(|| "Twitch rejected the EventSub user access token".to_string())?;
        let authorization = Arc::new(authorization);
        *cached = Some((normalized.to_string(), authorization.clone()));
        Ok(authorization)
    }

    async fn reset_auth_session(&self) {
        self.force_disconnect("authenticated account changed").await;
        self.channel_to_broadcaster.write_or_recover().clear();
        self.forbidden_subs.write_or_recover().clear();
        self.existing_subs.write_or_recover().clear();
        self.cost_limited_subs.write_or_recover().clear();
        self.subscriptions.write_or_recover().clear();
        *self.reconnect_url.write_or_recover() = None;
        self.server_reconnect_pending
            .store(false, Ordering::Release);
        self.server_reconnect_deadline_ms
            .store(0, Ordering::Release);
        *self.token_authorization.lock().await = None;
        *self.notification_ids.lock_or_recover() =
            MessageDeduplicator::new(EVENTSUB_DELIVERY_ID_CACHE_SIZE);
        self.clear_rate_limit();
    }
    /// Update last message time to now
    fn touch_keepalive(&self) {
        self.last_message_time
            .store(crate::utils::unix_now_ms(), Ordering::Relaxed);
    }

    /// Maximum EventSub subscription backoff after a 429 response.
    ///
    /// Twitch supplies `Ratelimit-Reset` as a Unix timestamp in seconds. Keep the
    /// fallback and any unexpectedly distant server deadline bounded so one bad
    /// response cannot stall subscription recovery indefinitely.
    const MAX_RATE_LIMIT_DELAY_MS: u64 = 60_000;

    /// Check if currently rate limited (with automatic expiry)
    fn rate_limit_remaining_ms(&self) -> Option<u64> {
        let until = self.rate_limited_until.load(Ordering::Relaxed);
        if until == 0 {
            return None;
        }

        let now = crate::utils::unix_now_ms();
        if now >= until {
            let _ = self.rate_limited_until.compare_exchange(
                until,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            None
        } else {
            Some(until - now)
        }
    }

    fn is_rate_limited(&self) -> bool {
        self.rate_limit_remaining_ms().is_some()
    }

    /// Record Twitch's reset deadline when it is a valid future Unix timestamp.
    /// Invalid, stale, overflowing, or missing values use the bounded fallback.
    fn set_rate_limited(&self, reset_epoch_seconds: Option<u64>) -> u64 {
        let now = crate::utils::unix_now_ms();
        let fallback = now.saturating_add(Self::MAX_RATE_LIMIT_DELAY_MS);
        let expires = reset_epoch_seconds
            .and_then(|seconds| seconds.checked_mul(1_000))
            .filter(|reset| *reset > now)
            .map(|reset| reset.min(fallback))
            .unwrap_or(fallback);
        self.rate_limited_until.store(expires, Ordering::Relaxed);
        expires.saturating_sub(now)
    }

    /// Clear rate limit (called on disconnect)
    fn clear_rate_limit(&self) {
        self.rate_limited_until.store(0, Ordering::Relaxed);
    }

    /// Check if keepalive has timed out
    /// Returns true if we should reconnect
    pub fn is_keepalive_expired(&self) -> bool {
        self.keepalive_deadline_ms()
            .is_some_and(|deadline| crate::utils::unix_now_ms() > deadline)
    }

    fn keepalive_deadline_ms(&self) -> Option<u64> {
        let last = self.last_message_time.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        let timeout_ms = u64::from(*self.keepalive_timeout.read_or_recover()) * 1000;
        Some(last.saturating_add(timeout_ms).saturating_add(5000))
    }

    /// Connect to EventSub WebSocket
    ///
    /// Uses atomic compare-and-swap to ensure only one connection attempt runs at a time.
    /// This prevents race conditions where multiple threads/tasks could simultaneously
    /// detect "not connected" and all try to connect.
    pub async fn connect(self: &Arc<Self>) -> Result<(), String> {
        let auth_generation = EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire);
        if !auth_ready_for_eventsub() {
            return Err(
                "EventSub is paused until authenticated Twitch access is ready".to_string(),
            );
        }

        if *self.is_connected.read_or_recover() {
            return Ok(());
        }

        if CONNECTING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            crate::log_net!(
                debug,
                "EventSub connection attempt already in progress, skipping"
            );
            return Ok(());
        }

        struct ConnectingGuard;
        impl Drop for ConnectingGuard {
            fn drop(&mut self) {
                CONNECTING.store(false, Ordering::SeqCst);
            }
        }
        let _guard = ConnectingGuard;

        crate::log_net!(debug, "Connecting to {}", EVENTSUB_WS_URL);

        let (ws_stream, _) = crate::backend::ws::connect_websocket(EVENTSUB_WS_URL)
            .await
            .map_err(|e| format!("Failed to connect to EventSub: {}", e))?;

        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
            || !auth_ready_for_eventsub()
        {
            return Err("EventSub authentication changed during WebSocket connect".to_string());
        }

        let (tx, rx) = mpsc::channel::<Message>(32);
        let old_sender;
        let connection_epoch;
        {
            let mut sender = self.sender.lock().await;
            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                || !auth_ready_for_eventsub()
            {
                return Err("EventSub authentication changed during WebSocket install".to_string());
            }
            if *self.is_connected.read_or_recover() {
                return Ok(());
            }

            connection_epoch = self.active_connection_epoch.fetch_add(1, Ordering::SeqCst) + 1;
            old_sender = sender.replace(tx);
            *self.is_connected.write_or_recover() = true;
            *self.session_id.write_or_recover() = None;
            // Start the keepalive clock at install, so a socket that never
            // delivers its welcome is reconnected like any other silent one.
            self.touch_keepalive();
            *self.reconnect_url.write_or_recover() = None;
            self.server_reconnect_pending
                .store(false, Ordering::Release);
            self.server_reconnect_deadline_ms
                .store(0, Ordering::Release);
            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                sender.take();
                *self.is_connected.write_or_recover() = false;
                return Err("EventSub authentication changed during WebSocket install".to_string());
            }
            self.spawn_connection_tasks(ws_stream, rx, auth_generation, connection_epoch);
        }

        if let Some(old_sender) = old_sender {
            let _ = old_sender.send(Message::Close(None)).await;
        }

        // The session welcome logs the "up" transition at info.
        crate::log_net!(
            debug,
            "EventSub WebSocket transport connected (epoch {})",
            connection_epoch
        );
        Ok(())
    }

    fn spawn_connection_tasks(
        self: &Arc<Self>,
        ws_stream: crate::backend::ws::ProxyAwareWebSocket,
        mut rx: mpsc::Receiver<Message>,
        auth_generation: u64,
        connection_epoch: u64,
    ) {
        let (mut write, mut read) = ws_stream.split();

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write.send(msg).await {
                    crate::log_net!(error, "EventSub write error: {}", e);
                    break;
                }
            }
        });

        let client = Arc::clone(self);
        tokio::spawn(async move {
            let mut end_reason = "stream ended without a close frame".to_string();
            let mut unexpected = true;
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = client
                            .handle_message(&text, auth_generation, connection_epoch)
                            .await
                        {
                            crate::log_net!(error, "Error handling EventSub message: {}", e);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        unexpected = frame.as_ref().is_some_and(|frame| {
                            frame.code != tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal
                        });
                        end_reason = match frame {
                            Some(frame) => {
                                format!("closed by server ({} {})", frame.code, frame.reason)
                            }
                            None => "closed by server".to_string(),
                        };
                        break;
                    }
                    Err(e) => {
                        end_reason = format!("read error: {e}");
                        break;
                    }
                    _ => {}
                }
            }

            client
                .finish_connection(connection_epoch, &end_reason, unexpected)
                .await;
        });
    }

    /// Clean up after the reader of `connection_epoch` stopped, logging the
    /// "down" transition once: warn when the server dropped us unexpectedly.
    async fn finish_connection(&self, connection_epoch: u64, reason: &str, unexpected: bool) {
        let mut sender = self.sender.lock().await;
        if self.active_connection_epoch.load(Ordering::SeqCst) != connection_epoch {
            crate::log_net!(
                debug,
                "Ignoring stale EventSub cleanup for epoch {} ({reason})",
                connection_epoch
            );
            return;
        }

        sender.take();
        *self.is_connected.write_or_recover() = false;
        wake_supervisor();

        let preserve_for_handoff = self.server_reconnect_pending.load(Ordering::Acquire)
            && self.reconnect_url.read_or_recover().is_some();
        if preserve_for_handoff {
            crate::log_net!(
                info,
                "EventSub epoch {} ended during server handoff ({reason}); preserving subscription state for the candidate",
                connection_epoch
            );
            return;
        }

        *self.session_id.write_or_recover() = None;
        self.existing_subs.write_or_recover().clear();
        self.cost_limited_subs.write_or_recover().clear();
        self.subscriptions.write_or_recover().clear();
        *self.reconnect_url.write_or_recover() = None;
        self.server_reconnect_pending
            .store(false, Ordering::Release);
        self.server_reconnect_deadline_ms
            .store(0, Ordering::Release);
        self.last_message_time.store(0, Ordering::Relaxed);
        self.clear_rate_limit();

        if unexpected {
            crate::log_net!(
                warn,
                "EventSub connection epoch {} disconnected: {reason}",
                connection_epoch
            );
        } else {
            crate::log_net!(
                info,
                "EventSub connection epoch {} closed: {reason}",
                connection_epoch
            );
        }
    }

    async fn handoff_reconnect(
        self: &Arc<Self>,
        reconnect_url: &str,
        source_epoch: u64,
    ) -> Result<(), String> {
        let auth_generation = EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire);
        if !auth_ready_for_eventsub()
            || self.active_connection_epoch.load(Ordering::SeqCst) != source_epoch
            || !self.server_reconnect_pending.load(Ordering::Acquire)
            || crate::utils::unix_now_ms()
                >= self.server_reconnect_deadline_ms.load(Ordering::Acquire)
        {
            return Err("EventSub server handoff is no longer current".to_string());
        }

        if CONNECTING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("another EventSub connection attempt is in progress".to_string());
        }
        struct ConnectingGuard;
        impl Drop for ConnectingGuard {
            fn drop(&mut self) {
                CONNECTING.store(false, Ordering::SeqCst);
            }
        }
        let _guard = ConnectingGuard;

        crate::log_net!(
            info,
            "Connecting EventSub handoff candidate for epoch {}",
            source_epoch
        );

        let candidate = async {
            let (mut ws_stream, _) = crate::backend::ws::connect_websocket(reconnect_url)
                .await
                .map_err(|error| format!("candidate WebSocket connect failed: {error}"))?;

            loop {
                let message = ws_stream
                    .next()
                    .await
                    .ok_or_else(|| "candidate closed before session_welcome".to_string())?
                    .map_err(|error| format!("candidate read failed: {error}"))?;

                match message {
                    Message::Text(text) => {
                        let envelope: EventSubMessage = serde_json::from_str(&text)
                            .map_err(|error| format!("candidate message parse failed: {error}"))?;
                        match envelope.metadata.message_type.as_str() {
                            message_type::SESSION_WELCOME => {
                                let welcome: WelcomePayload =
                                    serde_json::from_value(envelope.payload).map_err(|error| {
                                        format!("candidate welcome parse failed: {error}")
                                    })?;
                                return Ok::<_, String>((ws_stream, welcome));
                            }
                            message_type::NOTIFICATION => {
                                self.handle_message(&text, auth_generation, u64::MAX)
                                    .await?;
                            }
                            other => crate::log_net!(
                                debug,
                                "Ignoring candidate EventSub {} before session_welcome",
                                other
                            ),
                        }
                    }
                    Message::Ping(payload) => ws_stream
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|error| format!("candidate pong failed: {error}"))?,
                    Message::Close(_) => {
                        return Err("candidate closed before session_welcome".to_string());
                    }
                    _ => {}
                }
            }
        };

        let (ws_stream, welcome) =
            tokio::time::timeout(EVENTSUB_HANDOFF_ATTEMPT_TIMEOUT, candidate)
                .await
                .map_err(|_| {
                    format!(
                        "candidate did not reach session_welcome within {}s",
                        EVENTSUB_HANDOFF_ATTEMPT_TIMEOUT.as_secs()
                    )
                })??;

        let (tx, rx) = mpsc::channel::<Message>(32);
        let old_sender;
        let promoted_epoch;
        {
            let mut sender = self.sender.lock().await;
            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                || !auth_ready_for_eventsub()
                || self.active_connection_epoch.load(Ordering::SeqCst) != source_epoch
                || !self.server_reconnect_pending.load(Ordering::Acquire)
                || self.reconnect_url.read_or_recover().as_deref() != Some(reconnect_url)
            {
                return Err("EventSub handoff became stale before promotion".to_string());
            }

            promoted_epoch = self.active_connection_epoch.fetch_add(1, Ordering::SeqCst) + 1;
            old_sender = sender.replace(tx);
            *self.session_id.write_or_recover() = Some(welcome.session.id.clone());
            *self.keepalive_timeout.write_or_recover() = welcome.session.keepalive_timeout_seconds;
            *self.is_connected.write_or_recover() = true;
            *self.reconnect_url.write_or_recover() = None;
            self.server_reconnect_pending
                .store(false, Ordering::Release);
            self.server_reconnect_deadline_ms
                .store(0, Ordering::Release);
            self.reconnect_requested.store(false, Ordering::Release);
            self.touch_keepalive();
            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                sender.take();
                *self.is_connected.write_or_recover() = false;
                *self.session_id.write_or_recover() = None;
                self.last_message_time.store(0, Ordering::Relaxed);
                return Err("EventSub authentication changed during handoff promotion".to_string());
            }
            self.spawn_connection_tasks(ws_stream, rx, auth_generation, promoted_epoch);
        }

        if let Some(old_sender) = old_sender
            && let Err(error) = old_sender.send(Message::Close(None)).await
        {
            crate::log_net!(debug, "Could not send old EventSub close frame: {error}");
        }

        crate::log_net!(
            info,
            "Promoted EventSub handoff epoch {} -> {} (session {}, keepalive {}s)",
            source_epoch,
            promoted_epoch,
            welcome.session.id,
            welcome.session.keepalive_timeout_seconds
        );
        Ok(())
    }

    fn can_retry_server_handoff(&self, source_epoch: u64) -> bool {
        self.active_connection_epoch.load(Ordering::SeqCst) == source_epoch
            && self.server_reconnect_pending.load(Ordering::Acquire)
            && crate::utils::unix_now_ms()
                < self.server_reconnect_deadline_ms.load(Ordering::Acquire)
            && *self.is_connected.read_or_recover()
            && !self.is_keepalive_expired()
            && auth_ready_for_eventsub()
    }

    /// Force-close current WebSocket connection and mark state as disconnected.
    async fn force_disconnect(&self, reason: &str) {
        let sender;
        let closed_epoch;
        {
            let mut active_sender = self.sender.lock().await;
            closed_epoch = self.active_connection_epoch.fetch_add(1, Ordering::SeqCst);
            sender = active_sender.take();
            *self.is_connected.write_or_recover() = false;
            *self.session_id.write_or_recover() = None;
            self.existing_subs.write_or_recover().clear();
            self.cost_limited_subs.write_or_recover().clear();
            self.subscriptions.write_or_recover().clear();
            *self.reconnect_url.write_or_recover() = None;
            self.server_reconnect_pending
                .store(false, Ordering::Release);
            self.server_reconnect_deadline_ms
                .store(0, Ordering::Release);
            self.reconnect_requested.store(false, Ordering::Release);
            self.last_message_time.store(0, Ordering::Relaxed);
        }
        self.clear_rate_limit();

        if matches!(reason, "keepalive timeout" | "server handoff failed") {
            crate::log_net!(
                warn,
                "Forcing EventSub disconnect of epoch {} due to {}",
                closed_epoch,
                reason
            );
        } else {
            crate::log_net!(
                info,
                "Closing EventSub connection epoch {} because {}",
                closed_epoch,
                reason
            );
        }
        if let Some(sender) = sender
            && let Err(error) = sender.send(Message::Close(None)).await
        {
            crate::log_net!(debug, "Failed to send EventSub close frame: {error}");
        }
    }

    /// Handle incoming WebSocket message
    async fn handle_message(
        &self,
        text: &str,
        auth_generation: u64,
        connection_epoch: u64,
    ) -> Result<(), String> {
        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
            crate::log_net!(debug, "Dropped EventSub message from stale auth generation");
            return Ok(());
        }

        let msg: EventSubMessage =
            serde_json::from_str(text).map_err(|e| format!("Failed to parse message: {}", e))?;

        let control_epoch_is_active =
            self.active_connection_epoch.load(Ordering::SeqCst) == connection_epoch;
        if !control_epoch_is_active
            && msg.metadata.message_type.as_str() != message_type::NOTIFICATION
        {
            crate::log_net!(
                debug,
                "Ignored stale EventSub {} control message from epoch {}",
                msg.metadata.message_type,
                connection_epoch
            );
            return Ok(());
        }

        match msg.metadata.message_type.as_str() {
            message_type::SESSION_WELCOME => {
                let payload: WelcomePayload = serde_json::from_value(msg.payload)
                    .map_err(|e| format!("Failed to parse welcome: {}", e))?;

                let _sender = self.sender.lock().await;
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || self.active_connection_epoch.load(Ordering::SeqCst) != connection_epoch
                {
                    return Ok(());
                }
                self.touch_keepalive();

                crate::log_net!(
                    info,
                    "Session welcome - id: {}, keepalive: {}s",
                    payload.session.id,
                    payload.session.keepalive_timeout_seconds
                );

                {
                    let mut session_id = self.session_id.write_or_recover();
                    *session_id = Some(payload.session.id);
                }
                {
                    let mut keepalive = self.keepalive_timeout.write_or_recover();
                    *keepalive = payload.session.keepalive_timeout_seconds;
                }

                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                    *self.session_id.write_or_recover() = None;
                    return Ok(());
                }

                {
                    let mut reconnect_url = self.reconnect_url.write_or_recover();
                    *reconnect_url = None;
                }
            }

            message_type::SESSION_KEEPALIVE => {
                let _sender = self.sender.lock().await;
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || self.active_connection_epoch.load(Ordering::SeqCst) != connection_epoch
                {
                    return Ok(());
                }
                self.touch_keepalive();
                crate::log_net!(debug, "Keepalive received");
            }

            message_type::NOTIFICATION => {
                if control_epoch_is_active {
                    let _sender = self.sender.lock().await;
                    if auth_generation == EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                        && self.active_connection_epoch.load(Ordering::SeqCst) == connection_epoch
                    {
                        self.touch_keepalive();
                    }
                }
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                    return Ok(());
                }
                let delivery_id = msg.metadata.message_id.trim();
                if !delivery_id.is_empty()
                    && !self
                        .notification_ids
                        .lock_or_recover()
                        .check_and_add(delivery_id.to_string())
                {
                    crate::log_net!(debug, "Dropped duplicate EventSub delivery {}", delivery_id);
                    return Ok(());
                }
                let payload: NotificationPayload = serde_json::from_value(msg.payload)
                    .map_err(|e| format!("Failed to parse notification: {}", e))?;

                self.handle_notification(&payload).await?;
            }

            message_type::SESSION_RECONNECT => {
                let payload: ReconnectPayload = serde_json::from_value(msg.payload)
                    .map_err(|e| format!("Failed to parse reconnect: {}", e))?;

                let _sender = self.sender.lock().await;
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || self.active_connection_epoch.load(Ordering::SeqCst) != connection_epoch
                {
                    return Ok(());
                }
                self.touch_keepalive();

                crate::log_net!(
                    info,
                    "EventSub server requested reconnect to {}",
                    payload
                        .session
                        .reconnect_url
                        .as_deref()
                        .map_or_else(|| "<none>".to_string(), crate::log::redact_url)
                );

                if let Some(url) = payload
                    .session
                    .reconnect_url
                    .filter(|url| !url.trim().is_empty())
                {
                    *self.reconnect_url.write_or_recover() = Some(url);
                    self.server_reconnect_deadline_ms.store(
                        crate::utils::unix_now_ms().saturating_add(EVENTSUB_RECONNECT_GRACE_MS),
                        Ordering::Release,
                    );
                    self.server_reconnect_pending.store(true, Ordering::Release);
                    self.reconnect_requested.store(true, Ordering::SeqCst);
                } else {
                    crate::log_net!(
                        warn,
                        "EventSub reconnect control message did not contain a usable URL; scheduling a fresh session"
                    );
                    *self.reconnect_url.write_or_recover() = None;
                    self.server_reconnect_pending
                        .store(false, Ordering::Release);
                    self.server_reconnect_deadline_ms
                        .store(0, Ordering::Release);
                    self.reconnect_requested.store(true, Ordering::SeqCst);
                }
                wake_supervisor();

                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                    *self.reconnect_url.write_or_recover() = None;
                    self.server_reconnect_pending
                        .store(false, Ordering::Release);
                    self.server_reconnect_deadline_ms
                        .store(0, Ordering::Release);
                    self.reconnect_requested.store(false, Ordering::Release);
                }
            }

            message_type::REVOCATION => {
                let _sender = self.sender.lock().await;
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || self.active_connection_epoch.load(Ordering::SeqCst) != connection_epoch
                {
                    return Ok(());
                }
                self.touch_keepalive();
                crate::log_net!(warn, "Subscription revoked: {:?}", msg.payload);
            }

            _ => {
                if control_epoch_is_active {
                    let _sender = self.sender.lock().await;
                    if auth_generation == EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                        && self.active_connection_epoch.load(Ordering::SeqCst) == connection_epoch
                    {
                        self.touch_keepalive();
                    }
                }
                crate::log_net!(debug, "Unknown message type: {}", msg.metadata.message_type);
            }
        }

        Ok(())
    }

    /// Handle notification event
    async fn handle_notification(&self, payload: &NotificationPayload) -> Result<(), String> {
        let Some(event) =
            notification::parse_notification(&payload.subscription.sub_type, &payload.event)
        else {
            crate::log_net!(
                debug,
                "Unhandled event type: {}",
                payload.subscription.sub_type
            );
            return Ok(());
        };
        let event = event?;

        if !subscription_is_desired(&event.channel, None) {
            crate::log_net!(
                debug,
                "Suppressed EventSub delivery for released channel #{}",
                event.channel
            );
            return Ok(());
        }

        if matches!(event.event_type, TwitchEventType::Unban)
            && let Some(user_id) = payload
                .event
                .get("user_id")
                .and_then(|value| value.as_str())
                .filter(|id| !id.is_empty())
        {
            crate::backend::irc::room_state::clear_send_restriction_for_user(
                &event.channel,
                user_id,
            );
        }

        update_active_prediction(&event);
        if matches!(
            event.event_type,
            TwitchEventType::StreamOnline | TwitchEventType::StreamOffline
        ) {
            let live = event.event_type == TwitchEventType::StreamOnline;
            crate::backend::bridge::set_cached_channel_live_status(&event.channel, live);
            crate::backend::twitch_api::forget_proxy_stream_lookup(&event.channel);
            let started_at = live
                .then(|| {
                    payload
                        .event
                        .get("started_at")
                        .and_then(serde_json::Value::as_str)
                })
                .flatten()
                .and_then(|started_at| chrono::DateTime::parse_from_rfc3339(started_at).ok())
                .map(|started_at| started_at.timestamp());
            let last_hash = crate::backend::bridge::stage_eventsub_stream_info(
                &event.channel,
                live,
                started_at,
            );
            crate::log_net!(
                info,
                "EventSub live flip applied for {}: live={}",
                event.channel,
                live
            );

            let channel = event.channel.clone();
            crate::backend::bridge::get_runtime().spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                for attempt in 0..3 {
                    if crate::backend::bridge::queue_stream_info_refresh(
                        vec![channel.clone()],
                        vec![(channel.clone(), last_hash)],
                    ) {
                        return;
                    }
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                crate::log_net!(
                    debug,
                    "EventSub confirming refresh dropped for {channel}: poll in flight"
                );
            });
        }

        if notification::should_surface_event(&event) {
            push_event(event.clone());
            notification::play_event_sound(&event);
            super::notifications::notify_twitch_event(&event);
            if notification::should_insert_event_message(&event) {
                crate::backend::bridge::insert_event_message(&event);
            }
            notification::record_mod_log_event(&event);
        } else {
            crate::log_net!(
                debug,
                "Suppressed EventSub notification: {:?} in #{}",
                event.event_type,
                event.channel
            );
        }

        crate::log_net!(info, "Event: {:?} in #{}", event.event_type, event.channel);

        Ok(())
    }

    /// Subscribe to events for a channel (by broadcaster user ID)
    /// Retries with exponential backoff if not connected yet
    async fn subscribe_to_channel(
        &self,
        channel_name: &str,
        broadcaster_user_id: &str,
        token: &str,
        client_id: &str,
    ) -> Result<SubscriptionAttemptOutcome, String> {
        let auth_generation = EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire);
        if !auth_ready_for_eventsub() {
            return Ok(SubscriptionAttemptOutcome::Complete);
        }
        if !subscription_is_desired(channel_name, Some(broadcaster_user_id)) {
            crate::log_net!(
                debug,
                "Skipping EventSub setup for released channel {channel_name}"
            );
            return Ok(SubscriptionAttemptOutcome::Complete);
        }

        {
            let mut mapping = self.channel_to_broadcaster.write_or_recover();
            mapping.insert(channel_name.to_lowercase(), broadcaster_user_id.to_string());
        }

        let session_id = {
            let mut delay_ms = 500;
            let max_retries = 5;
            let mut session_id = None;

            for attempt in 0..max_retries {
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || !subscription_is_desired(channel_name, Some(broadcaster_user_id))
                {
                    return Ok(SubscriptionAttemptOutcome::Complete);
                }
                {
                    let session = self.session_id.read_or_recover();
                    if let Some(id) = session.clone() {
                        session_id = Some(id);
                        break;
                    }
                }

                if attempt < max_retries - 1 {
                    crate::log_net!(
                        debug,
                        "Waiting for connection before subscribing to {} (attempt {}/{})",
                        channel_name,
                        attempt + 1,
                        max_retries
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(5000);
                }
            }

            session_id.ok_or("Not connected - no session ID after retries")?
        };

        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
            return Ok(SubscriptionAttemptOutcome::Complete);
        }

        let authorization = self.authorization_for_token(token).await?;
        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
            return Ok(SubscriptionAttemptOutcome::Complete);
        }
        let authorization_user_id = authorization.user_id();
        let is_broadcaster = authorization_user_id == Some(broadcaster_user_id);
        let is_moderator = crate::backend::irc::get_self_state(channel_name).is_some_and(|state| {
            state
                .badges
                .iter()
                .any(|badge| crate::backend::badges::is_moderator_badge_key(badge))
        });
        if !subscription_is_desired(channel_name, Some(broadcaster_user_id)) {
            return Ok(SubscriptionAttemptOutcome::Complete);
        }

        let subscription_types: Vec<_> = PUBLIC_EVENTSUB_SPECS
            .iter()
            .chain(BROADCASTER_EVENTSUB_SPECS)
            .chain(MODERATOR_EVENTSUB_SPECS)
            .copied()
            .filter(|spec| {
                let relation_allowed = match spec.relation {
                    SubscriptionRelation::Any => true,
                    SubscriptionRelation::Broadcaster => is_broadcaster,
                    SubscriptionRelation::ModeratorOrBroadcaster => is_broadcaster || is_moderator,
                };
                relation_allowed && authorization.has_any_scope(spec.scopes)
            })
            .collect();

        crate::log_net!(
            debug,
            "Selected {} EventSub subscription families for #{} (broadcaster={}, moderator={})",
            subscription_types.len(),
            channel_name,
            is_broadcaster,
            is_moderator
        );

        if let Some(remaining_ms) = self.rate_limit_remaining_ms() {
            crate::log_net!(
                debug,
                "EventSub subscription rate limit active for ~{}s; deferring {}",
                remaining_ms.saturating_add(999) / 1_000,
                channel_name,
            );
            return Ok(SubscriptionAttemptOutcome::DeferredByRateLimit);
        }

        let client = EVENTSUB_HTTP_CLIENT.client();
        let mut sub_ids = Vec::new();
        let mut outcome = SubscriptionAttemptOutcome::Complete;

        for spec in subscription_types {
            let sub_type = spec.sub_type;
            let version = spec.version;
            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                || !subscription_is_desired(channel_name, Some(broadcaster_user_id))
            {
                crate::log_net!(
                    debug,
                    "Stopped EventSub setup for released channel {channel_name}"
                );
                break;
            }

            let sub_key = format!("{}:{}", broadcaster_user_id, sub_type);

            {
                let forbidden = self.forbidden_subs.read_or_recover();
                if forbidden.contains(&sub_key) {
                    continue;
                }
            }

            {
                let cost_limited = self.cost_limited_subs.read_or_recover();
                if cost_limited.contains(&sub_key) {
                    continue;
                }
            }

            {
                let existing = self.existing_subs.read_or_recover();
                if existing.contains(&sub_key) {
                    continue;
                }
            }

            let condition = match spec.condition {
                SubscriptionCondition::RaidToBroadcaster => {
                    serde_json::json!({
                        "to_broadcaster_user_id": broadcaster_user_id
                    })
                }
                SubscriptionCondition::Moderator => {
                    let Some(moderator_user_id) = authorization_user_id else {
                        continue;
                    };
                    serde_json::json!({
                        "broadcaster_user_id": broadcaster_user_id,
                        "moderator_user_id": moderator_user_id
                    })
                }
                SubscriptionCondition::Broadcaster => {
                    serde_json::json!({
                        "broadcaster_user_id": broadcaster_user_id
                    })
                }
            };

            let request = SubscriptionRequest {
                sub_type: sub_type.to_string(),
                version: version.to_string(),
                condition,
                transport: TransportInfo {
                    method: "websocket".to_string(),
                    session_id: session_id.clone(),
                },
            };

            let started = std::time::Instant::now();
            let response = client
                .post(HELIX_EVENTSUB_URL)
                .header("Client-Id", client_id)
                .header("Authorization", format!("Bearer {}", token))
                .json(&request)
                .send()
                .await;
            // Non-2xx statuses are logged below with the subscription type and
            // channel (403/409 are expected and stay quiet); a transport error
            // is logged once by the caller with the channel.
            if matches!(&response, Ok(response) if response.status().is_success()) {
                crate::backend::http::log_http_outcome(
                    crate::backend::http::LogCategory::Net,
                    "POST",
                    HELIX_EVENTSUB_URL,
                    response.as_ref().map(reqwest::Response::status),
                    started.elapsed(),
                    1,
                );
            }
            let response = response.map_err(|e| {
                format!(
                    "Failed to create subscription: {}",
                    crate::backend::http::error_kind(&e)
                )
            })?;

            if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
                return Ok(SubscriptionAttemptOutcome::Complete);
            }

            let status = response.status();

            if status.is_success() {
                let body: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse response: {}", e))?;

                if let Some(data) = body.get("data").and_then(|d| d.as_array())
                    && let Some(first) = data.first()
                    && let Some(id) = first.get("id").and_then(|i| i.as_str())
                {
                    sub_ids.push(id.to_string());
                    self.existing_subs
                        .write_or_recover()
                        .insert(sub_key.clone());
                    crate::log_net!(
                        debug,
                        "Subscribed to {} for {}",
                        sub_type,
                        broadcaster_user_id
                    );
                }
            } else if status.as_u16() == 403 {
                {
                    let mut forbidden = self.forbidden_subs.write_or_recover();
                    forbidden.insert(sub_key);
                }
                crate::log_net!(
                    debug,
                    "No permission for {} on {} (403 - will not retry)",
                    sub_type,
                    channel_name
                );
            } else if status.as_u16() == 409 {
                {
                    let mut existing = self.existing_subs.write_or_recover();
                    existing.insert(sub_key);
                }
            } else if status.as_u16() == 429 {
                let reset_epoch_seconds = response
                    .headers()
                    .get("Ratelimit-Reset")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok());
                let body = crate::backend::http::error_body(response).await;
                let body_lower = body.to_ascii_lowercase();
                if body_lower.contains("total cost")
                    || body_lower.contains("maximum subscriptions")
                    || body_lower.contains("cost limit")
                {
                    {
                        let mut cost_limited = self.cost_limited_subs.write_or_recover();
                        cost_limited.insert(sub_key);
                    }
                    crate::log_net!(
                        info,
                        "EventSub total-cost budget reached for {} on {} - deferring until reconnect",
                        sub_type,
                        channel_name
                    );
                    continue;
                }
                let delay_ms = self.set_rate_limited(reset_epoch_seconds);
                outcome = SubscriptionAttemptOutcome::DeferredByRateLimit;
                crate::log_net!(
                    warn,
                    "Rate limited by EventSub API; retrying deferred subscriptions in ~{}s",
                    delay_ms.saturating_add(999) / 1_000,
                );
                break;
            } else if status.as_u16() == 401 {
                let body = crate::backend::http::error_body(response).await;
                crate::log_net!(
                    warn,
                    "EventSub rejected the access token while subscribing to {} on {}: {}",
                    sub_type,
                    channel_name,
                    body
                );
                *self.token_authorization.lock().await = None;
                if let Err(error) = crate::backend::auth::refresh_and_save_tokens().await {
                    crate::log_net!(
                        warn,
                        "Token refresh after EventSub 401 failed: {}; retrying subscription later",
                        error
                    );
                }
                outcome = SubscriptionAttemptOutcome::Retryable;
                break;
            } else if status.is_server_error() || status.as_u16() == 408 {
                let body = crate::backend::http::error_body(response).await;
                crate::log_net!(
                    warn,
                    "Transient EventSub failure for {} on {}: {} - {}",
                    sub_type,
                    channel_name,
                    status,
                    body
                );
                outcome = SubscriptionAttemptOutcome::Retryable;
            } else {
                let body = crate::backend::http::error_body(response).await;
                crate::log_net!(
                    warn,
                    "Failed to subscribe to {}: {} - {}",
                    sub_type,
                    status,
                    body
                );
            }
        }

        {
            let mut subscriptions = self.subscriptions.write_or_recover();
            subscriptions
                .entry(broadcaster_user_id.to_string())
                .or_default()
                .extend(sub_ids);
        }

        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
            || !subscription_is_desired(channel_name, Some(broadcaster_user_id))
        {
            let subscription_ids =
                self.take_channel_subscription_ids(channel_name, Some(broadcaster_user_id));
            self.delete_subscription_ids(subscription_ids, token, client_id)
                .await;
            return Ok(SubscriptionAttemptOutcome::Complete);
        }

        if outcome == SubscriptionAttemptOutcome::Complete {
            crate::log_net!(
                info,
                "Subscribed to events for broadcaster {}",
                broadcaster_user_id
            );
        }
        Ok(outcome)
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        *self.is_connected.read_or_recover()
    }

    /// Remove all client-side state for a channel and return active Helix subscription IDs.
    fn take_channel_subscription_ids(
        &self,
        channel_name: &str,
        broadcaster_hint: Option<&str>,
    ) -> Vec<String> {
        let channel_lower = channel_name.to_lowercase();
        let broadcaster_id = self
            .channel_to_broadcaster
            .write_or_recover()
            .remove(&channel_lower)
            .or_else(|| broadcaster_hint.map(str::to_string));
        let Some(broadcaster_id) = broadcaster_id else {
            return Vec::new();
        };

        let subscription_ids = self
            .subscriptions
            .write_or_recover()
            .remove(&broadcaster_id)
            .unwrap_or_default();
        let key_prefix = format!("{broadcaster_id}:");
        self.forbidden_subs
            .write_or_recover()
            .retain(|key| !key.starts_with(&key_prefix));
        self.existing_subs
            .write_or_recover()
            .retain(|key| !key.starts_with(&key_prefix));
        subscription_ids
    }

    async fn delete_subscription_ids(
        &self,
        subscription_ids: Vec<String>,
        token: &str,
        client_id: &str,
    ) {
        if subscription_ids.is_empty() {
            return;
        }

        let client = EVENTSUB_HTTP_CLIENT.client();
        for subscription_id in subscription_ids {
            let delete_url = format!(
                "{HELIX_EVENTSUB_URL}?id={}",
                urlencoding::encode(&subscription_id)
            );
            let response = crate::backend::http::send_logged(
                crate::backend::http::LogCategory::Net,
                client
                    .delete(&delete_url)
                    .header("Client-Id", client_id)
                    .header("Authorization", format!("Bearer {token}")),
            )
            .await;
            // send_logged already warned about a failed request.
            if response.is_ok_and(|response| {
                response.status().is_success() || response.status().as_u16() == 404
            }) {
                crate::log_net!(debug, "Released EventSub subscription {subscription_id}");
            } else {
                crate::log_net!(
                    debug,
                    "Could not release EventSub subscription {subscription_id}"
                );
            }
        }
    }
}

fn auth_ready_for_eventsub() -> bool {
    if crate::backend::reliability::is_safe_mode()
        || crate::backend::irc::is_anonymous_mode()
        || crate::backend::settings::get_token().is_none_or(|token| token.trim().is_empty())
    {
        return false;
    }

    let auth = crate::backend::auth::AUTH_STATE.read_or_recover();
    auth.authenticated && !auth.needs_reauth
}

/// Invalidate every session-bound EventSub capability after login/account/anonymous changes.
/// Desired channel ownership is intentionally retained and rebuilt only when authenticated
/// access is ready again.
pub fn reset_for_auth_change(reason: &str) {
    let generation = EVENTSUB_AUTH_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    crate::log_net!(
        info,
        "Resetting EventSub authentication generation {} ({})",
        generation,
        reason
    );

    crate::backend::irc::reconnect_with_current_auth();

    PENDING_SUBSCRIPTIONS.lock_or_recover().clear();
    SUBSCRIPTION_RETRIES.lock_or_recover().clear();
    EVENTSUB_CLIENT
        .channel_to_broadcaster
        .write_or_recover()
        .clear();
    EVENTSUB_CLIENT.forbidden_subs.write_or_recover().clear();
    EVENTSUB_CLIENT.existing_subs.write_or_recover().clear();
    EVENTSUB_CLIENT.cost_limited_subs.write_or_recover().clear();
    EVENTSUB_CLIENT.subscriptions.write_or_recover().clear();
    *EVENTSUB_CLIENT.session_id.write_or_recover() = None;
    *EVENTSUB_CLIENT.reconnect_url.write_or_recover() = None;
    EVENTSUB_CLIENT
        .server_reconnect_pending
        .store(false, Ordering::Release);
    EVENTSUB_CLIENT
        .server_reconnect_deadline_ms
        .store(0, Ordering::Release);
    EVENTSUB_CLIENT
        .reconnect_requested
        .store(false, Ordering::Release);
    EVENTSUB_CLIENT.clear_rate_limit();
    EVENTSUB_CLIENT
        .auth_reset_requested
        .store(true, Ordering::Release);

    if auth_ready_for_eventsub() {
        queue_desired_for_current_auth();
    }

    if SUPERVISOR_STARTED.load(Ordering::Acquire)
        || (auth_ready_for_eventsub()
            && DESIRED_SUBSCRIPTIONS
                .read_or_recover()
                .values()
                .any(DesiredSubscription::is_owned))
    {
        init_eventsub();
    }
    wake_supervisor();
}

/// Flag to track if supervisor thread is running
static SUPERVISOR_STARTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Wakes the parked supervisor when queued work, a reconnect request, a closed
/// transport, or authenticated readiness changes.
static SUPERVISOR_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Backstop for a missed wake; the supervisor otherwise sleeps until its next deadline.
const SUPERVISOR_MAX_PARK: Duration = Duration::from_secs(60);
/// Floor between passes over subscriptions that are still queued after processing.
const SUPERVISOR_PENDING_RETRY: Duration = Duration::from_secs(1);

fn wake_supervisor() {
    SUPERVISOR_WAKE.notify_one();
}

/// Wake the EventSub supervisor after signed-in readiness may have changed.
pub fn notify_auth_readiness_changed() {
    wake_supervisor();
}

async fn park_supervisor(timeout: Duration) {
    let _ = tokio::time::timeout(timeout, SUPERVISOR_WAKE.notified()).await;
}

/// Sleep until the keepalive deadline or the earliest queued-subscription retry,
/// whichever comes first.
fn supervisor_park_duration() -> Duration {
    let now = crate::utils::unix_now_ms();
    let mut wait = SUPERVISOR_MAX_PARK;
    if let Some(deadline) = EVENTSUB_CLIENT.keepalive_deadline_ms() {
        wait = wait.min(Duration::from_millis(
            deadline.saturating_sub(now).saturating_add(1),
        ));
    }

    let pending: Vec<String> = PENDING_SUBSCRIPTIONS
        .lock_or_recover()
        .iter()
        .map(|(channel, _)| channel.clone())
        .collect();
    if pending.is_empty() {
        return wait;
    }
    let pending_wait = match EVENTSUB_CLIENT.rate_limit_remaining_ms() {
        Some(remaining_ms) => Duration::from_millis(remaining_ms),
        None => {
            let retries = SUBSCRIPTION_RETRIES.lock_or_recover();
            pending
                .iter()
                .map(|channel| {
                    retries
                        .get(channel)
                        .map_or(0, |retry| retry.next_attempt_ms.saturating_sub(now))
                })
                .min()
                .map_or(SUPERVISOR_PENDING_RETRY, Duration::from_millis)
        }
    };
    wait.min(pending_wait.max(SUPERVISOR_PENDING_RETRY))
}

/// Initialize EventSub connection (call once at app startup)
/// Spawns a persistent thread that maintains the WebSocket connection
/// Note: Connection is delayed until first subscription to avoid 10s timeout
pub fn init_eventsub() {
    if crate::backend::reliability::is_safe_mode() {
        crate::log_net!(info, "EventSub startup skipped because safe mode is active");
        return;
    }

    if SUPERVISOR_STARTED
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }

    let spawn_result = std::thread::Builder::new()
        .name("eventsub-supervisor".into())
        .spawn(|| {
            struct SupervisorGuard;
            impl Drop for SupervisorGuard {
                fn drop(&mut self) {
                    SUPERVISOR_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _guard = SupervisorGuard;

            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    crate::log_net!(error, "Failed to create EventSub runtime: {e}");
                    return;
                }
            };

            runtime.block_on(async {
                let mut backoff_secs = 1u64;
                const MAX_BACKOFF_SECS: u64 = 60;

                crate::log_net!(
                    info,
                    "Supervisor started, waiting for first subscription..."
                );
                loop {
                    if EVENTSUB_CLIENT
                        .auth_reset_requested
                        .swap(false, Ordering::AcqRel)
                    {
                        EVENTSUB_CLIENT.reset_auth_session().await;
                    }

                    let has_pending = {
                        let pending = PENDING_SUBSCRIPTIONS.lock_or_recover();
                        !pending.is_empty()
                    };

                    if has_pending && auth_ready_for_eventsub() {
                        break;
                    }

                    park_supervisor(SUPERVISOR_MAX_PARK).await;
                }

                crate::log_net!(info, "First subscription pending, connecting...");

                loop {
                    if EVENTSUB_CLIENT
                        .auth_reset_requested
                        .swap(false, Ordering::AcqRel)
                    {
                        EVENTSUB_CLIENT.reset_auth_session().await;
                        if auth_ready_for_eventsub() {
                            requeue_desired_subscriptions();
                        }
                    }

                    if !auth_ready_for_eventsub() {
                        if EVENTSUB_CLIENT.is_connected() {
                            EVENTSUB_CLIENT
                                .force_disconnect("authenticated access is paused")
                                .await;
                        }
                        park_supervisor(SUPERVISOR_MAX_PARK).await;
                        continue;
                    }

                    if EVENTSUB_CLIENT
                        .reconnect_requested
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        crate::log_net!(info, "Processing pending EventSub reconnect request");
                        let reconnect_url = if EVENTSUB_CLIENT
                            .server_reconnect_pending
                            .load(Ordering::Acquire)
                        {
                            EVENTSUB_CLIENT.reconnect_url.read_or_recover().clone()
                        } else {
                            None
                        };

                        if let Some(reconnect_url) = reconnect_url {
                            let source_epoch = EVENTSUB_CLIENT
                                .active_connection_epoch
                                .load(Ordering::SeqCst);
                            match EVENTSUB_CLIENT
                                .handoff_reconnect(&reconnect_url, source_epoch)
                                .await
                            {
                                Ok(()) => {
                                    crate::backend::reliability::notify_provider_recovered(
                                        crate::backend::reliability::Provider::EventSub,
                                    );
                                    backoff_secs = 1;
                                }
                                Err(error) => {
                                    crate::log_net!(
                                        warn,
                                        "EventSub handoff candidate failed: {}",
                                        error
                                    );
                                    if EVENTSUB_CLIENT
                                        .auth_reset_requested
                                        .load(Ordering::Acquire)
                                    {
                                        continue;
                                    }
                                    if EVENTSUB_CLIENT.can_retry_server_handoff(source_epoch) {
                                        EVENTSUB_CLIENT
                                            .reconnect_requested
                                            .store(true, Ordering::Release);
                                        tokio::time::sleep(std::time::Duration::from_secs(1))
                                            .await;
                                        continue;
                                    }

                                    crate::log_net!(
                                        warn,
                                        "EventSub handoff source is no longer viable; falling back to a fresh supervised session"
                                    );
                                    EVENTSUB_CLIENT
                                        .force_disconnect("server handoff failed")
                                        .await;
                                }
                            }
                        } else {
                            EVENTSUB_CLIENT
                                .force_disconnect("reconnect requested")
                                .await;
                        }
                    }

                    if !EVENTSUB_CLIENT.is_connected() {
                        crate::log_net!(
                            info,
                            "Connecting (backoff: {}s)...",
                            if backoff_secs > 1 { backoff_secs } else { 0 }
                        );

                        match EVENTSUB_CLIENT.connect().await {
                            Ok(()) => {
                                crate::backend::reliability::notify_provider_recovered(
                                    crate::backend::reliability::Provider::EventSub,
                                );
                                backoff_secs = 1;

                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                                requeue_desired_subscriptions();

                                process_pending_subscriptions().await;
                            }
                            Err(e) => {
                                crate::backend::reliability::notify_provider_degraded(
                                    crate::backend::reliability::Provider::EventSub,
                                    e.clone(),
                                );
                                crate::log_net!(error, "Connection failed: {}", e);
                                tokio::time::sleep(crate::backend::retry::with_reconnect_jitter(
                                    Duration::from_secs(backoff_secs),
                                ))
                                .await;
                                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                                continue;
                            }
                        }
                    }

                    park_supervisor(supervisor_park_duration()).await;

                    process_pending_subscriptions().await;

                    if EVENTSUB_CLIENT.is_keepalive_expired() {
                        EVENTSUB_CLIENT.force_disconnect("keepalive timeout").await;
                    }
                }
            });
        });

    if let Err(e) = spawn_result {
        SUPERVISOR_STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
        crate::log_net!(error, "Failed to spawn EventSub supervisor thread: {e}");
    }
}

/// Request a reconnect so the next EventSub WebSocket uses current network settings.
pub fn reconnect_for_proxy_change() {
    request_reconnect("proxy settings change");
}

/// Replace the EventSub WebSocket, which cannot outlive a system sleep.
///
/// Unlike a proxy change this never starts EventSub: anonymous sessions do not
/// run the supervisor.
pub fn reconnect_after_system_wake() {
    if SUPERVISOR_STARTED.load(std::sync::atomic::Ordering::SeqCst) {
        request_reconnect("system wake");
    }
}

fn request_reconnect(reason: &'static str) {
    if crate::backend::reliability::is_safe_mode() {
        crate::log_net!(
            info,
            reason,
            "EventSub reconnect skipped because safe mode is active"
        );
        return;
    }

    init_eventsub();
    EVENTSUB_CLIENT
        .reconnect_requested
        .store(true, std::sync::atomic::Ordering::SeqCst);
    wake_supervisor();
    crate::log_net!(info, reason, "EventSub reconnect requested");
}

/// Pending subscriptions queue (channel_name, broadcaster_id)
/// NOTE: Token and client_id are fetched fresh when processing to avoid stale tokens
/// that could occur if the token is refreshed after queueing but before processing.
static PENDING_SUBSCRIPTIONS: LazyLock<std::sync::Mutex<Vec<(String, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Bounded retry budget for a channel whose subscription attempt failed for a reason
/// that can clear (expired token, 5xx, network error). Cost-limit 429s are terminal for
/// the session and are tracked by `cost_limited_subs`, so they never reach this map.
const MAX_SUBSCRIPTION_RETRIES: u32 = 5;
const SUBSCRIPTION_RETRY_BASE_DELAY_MS: u64 = 2_000;
const SUBSCRIPTION_RETRY_MAX_DELAY_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug)]
struct SubscriptionRetry {
    attempts: u32,
    next_attempt_ms: u64,
}

static SUBSCRIPTION_RETRIES: LazyLock<Mutex<HashMap<String, SubscriptionRetry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a failed attempt and arm its backoff. Returns false once the budget is spent,
/// so a permanently broken channel stops burning Helix requests every second.
fn schedule_subscription_retry(channel: &str) -> bool {
    let now = crate::utils::unix_now_ms();
    let mut retries = SUBSCRIPTION_RETRIES.lock_or_recover();
    let attempts = {
        let entry = retries
            .entry(channel.to_string())
            .or_insert(SubscriptionRetry {
                attempts: 0,
                next_attempt_ms: now,
            });
        entry.attempts = entry.attempts.saturating_add(1);
        entry.attempts
    };
    if attempts > MAX_SUBSCRIPTION_RETRIES {
        retries.remove(channel);
        return false;
    }

    let delay_ms = SUBSCRIPTION_RETRY_BASE_DELAY_MS
        .saturating_mul(1u64 << (attempts - 1))
        .min(SUBSCRIPTION_RETRY_MAX_DELAY_MS);
    if let Some(entry) = retries.get_mut(channel) {
        entry.next_attempt_ms = now.saturating_add(delay_ms);
    }
    true
}

/// True while the channel's backoff deadline has not been reached yet.
fn subscription_retry_pending(channel: &str) -> bool {
    let now = crate::utils::unix_now_ms();
    SUBSCRIPTION_RETRIES
        .lock_or_recover()
        .get(channel)
        .is_some_and(|retry| now < retry.next_attempt_ms)
}

fn clear_subscription_retry(channel: &str) {
    SUBSCRIPTION_RETRIES.lock_or_recover().remove(channel);
}

/// Reason-owned EventSub channel state. Open chat and future explicit live-watch ownership
/// are independent so closing the last split cannot accidentally remove an opted-in watch.
#[derive(Clone, Debug, Default)]
struct DesiredSubscription {
    broadcaster_id: Option<String>,
    open_channel: bool,
    live_watch: bool,
}

impl DesiredSubscription {
    fn is_owned(&self) -> bool {
        self.open_channel || self.live_watch
    }
}

/// Desired subscriptions persist across reconnects but are removed when their last owner leaves.
static DESIRED_SUBSCRIPTIONS: LazyLock<RwLock<HashMap<String, DesiredSubscription>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn queue_desired_for_current_auth() {
    if !auth_ready_for_eventsub() {
        return;
    }

    let desired: Vec<_> = DESIRED_SUBSCRIPTIONS
        .read_or_recover()
        .iter()
        .filter(|(_, entry)| entry.is_owned())
        .map(|(channel, entry)| (channel.clone(), entry.broadcaster_id.clone()))
        .collect();
    if desired.is_empty() {
        return;
    }

    {
        let mut pending = PENDING_SUBSCRIPTIONS.lock_or_recover();
        for (channel, broadcaster_id) in &desired {
            if let Some(broadcaster_id) = broadcaster_id
                && !pending.iter().any(|(queued, _)| queued == channel)
            {
                pending.push((channel.clone(), broadcaster_id.clone()));
            }
        }
    }

    let Some(token) = crate::backend::settings::get_token() else {
        return;
    };
    let auth_generation = EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire);
    for (channel, broadcaster_id) in desired {
        if broadcaster_id.is_some() {
            continue;
        }

        let token = token.clone();
        crate::backend::bridge::get_runtime().spawn(async move {
            let mut delay = std::time::Duration::from_millis(250);
            for attempt in 1..=3 {
                let resolved =
                    crate::backend::twitch_api::get_user_id_helix(&channel, &token).await;
                if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire)
                    || !auth_ready_for_eventsub()
                    || !subscription_is_desired(&channel, None)
                {
                    return;
                }

                match resolved {
                    Ok(broadcaster_id) => {
                        subscribe_channel(&channel, &broadcaster_id);
                        return;
                    }
                    Err(error) if attempt == 3 => crate::log_net!(
                        warn,
                        "Failed to resolve EventSub broadcaster ID for #{} after auth restore: {}",
                        channel,
                        error
                    ),
                    Err(error) => {
                        crate::log_net!(
                            debug,
                            "Retrying EventSub broadcaster ID for #{} after attempt {}: {}",
                            channel,
                            attempt,
                            error
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 4).min(std::time::Duration::from_secs(2));
                    }
                }
            }
        });
    }
}

fn subscription_is_desired(channel_name: &str, broadcaster_id: Option<&str>) -> bool {
    let desired = DESIRED_SUBSCRIPTIONS.read_or_recover();
    let Some(entry) = desired.get(&channel_name.to_lowercase()) else {
        return false;
    };
    if !entry.is_owned() {
        return false;
    }
    broadcaster_id
        .is_none_or(|broadcaster_id| entry.broadcaster_id.as_deref() == Some(broadcaster_id))
}

/// Record that a channel is currently owned by at least one open chat surface.
pub fn retain_open_channel(channel_name: &str) {
    let channel_lower = channel_name.to_lowercase();
    if channel_lower.is_empty() {
        return;
    }
    DESIRED_SUBSCRIPTIONS
        .write_or_recover()
        .entry(channel_lower)
        .or_default()
        .open_channel = true;
}

/// Release open-chat ownership and delete remote subscriptions if no other owner remains.
pub fn release_open_channel(channel_name: &str) {
    let channel_lower = channel_name.to_lowercase();
    let broadcaster_id = {
        let mut desired = DESIRED_SUBSCRIPTIONS.write_or_recover();
        match desired.get_mut(&channel_lower) {
            Some(entry) => {
                entry.open_channel = false;
                if entry.is_owned() {
                    return;
                }
                let broadcaster_id = entry.broadcaster_id.clone();
                desired.remove(&channel_lower);
                broadcaster_id
            }
            None => None,
        }
    };

    PENDING_SUBSCRIPTIONS
        .lock_or_recover()
        .retain(|(channel, _)| channel != &channel_lower);
    clear_subscription_retry(&channel_lower);
    let subscription_ids =
        EVENTSUB_CLIENT.take_channel_subscription_ids(&channel_lower, broadcaster_id.as_deref());
    if subscription_ids.is_empty() {
        return;
    }

    let Some(token) = crate::backend::settings::get_token() else {
        crate::log_net!(
            debug,
            "Released local EventSub state for {} without a token; remote session cleanup deferred",
            channel_lower
        );
        return;
    };
    let client = EVENTSUB_CLIENT.clone();
    crate::backend::bridge::get_runtime().spawn(async move {
        client
            .delete_subscription_ids(
                subscription_ids,
                &token,
                crate::backend::constants::CLIENT_ID,
            )
            .await;
    });
}

/// Subscribe to a channel's events
/// Queues the subscription and ensures the supervisor is running.
///
/// NOTE: Token and client_id are intentionally NOT stored in the queue.
/// They are fetched fresh when processing to avoid stale token issues
/// that occur when the token is refreshed after queueing.
pub fn subscribe_channel(channel_name: &str, broadcaster_user_id: &str) {
    let channel_lower = channel_name.to_lowercase();

    let should_queue = {
        let mut desired = DESIRED_SUBSCRIPTIONS.write_or_recover();
        let Some(entry) = desired.get_mut(&channel_lower) else {
            crate::log_net!(
                debug,
                "Ignoring late EventSub setup for closed channel {channel_lower}"
            );
            return;
        };
        if !entry.is_owned() {
            false
        } else {
            entry.broadcaster_id = Some(broadcaster_user_id.to_string());
            true
        }
    };
    if !should_queue {
        return;
    }

    {
        let mut pending = PENDING_SUBSCRIPTIONS.lock_or_recover();
        if let Some((_, queued_id)) = pending
            .iter_mut()
            .find(|(channel, _)| channel == &channel_lower)
        {
            *queued_id = broadcaster_user_id.to_string();
        } else {
            pending.push((channel_lower.clone(), broadcaster_user_id.to_string()));
        }
        crate::log_net!(
            info,
            "Queued subscription for {} (queue size: {})",
            channel_lower,
            pending.len()
        );
    }

    init_eventsub();
    wake_supervisor();
}

/// Re-evaluate capability-gated subscriptions when IRC USERSTATE establishes
/// that the signed-in account is a moderator for an already-open channel.
pub fn refresh_channel_authorization(channel_name: &str) {
    let channel_lower = channel_name.to_lowercase();
    let broadcaster_id = DESIRED_SUBSCRIPTIONS
        .read_or_recover()
        .get(&channel_lower)
        .filter(|entry| entry.is_owned())
        .and_then(|entry| entry.broadcaster_id.clone());
    let Some(broadcaster_id) = broadcaster_id else {
        return;
    };

    {
        let mut pending = PENDING_SUBSCRIPTIONS.lock_or_recover();
        if !pending.iter().any(|(channel, _)| channel == &channel_lower) {
            pending.push((channel_lower, broadcaster_id));
        }
    }
    wake_supervisor();
}

/// Put deferred channel work back without racing duplicate queue entries from UI/auth events.
fn requeue_pending_subscriptions(
    subscriptions: impl IntoIterator<Item = (String, String)>,
) -> usize {
    let subscriptions: Vec<_> = subscriptions
        .into_iter()
        .filter(|(channel, broadcaster_id)| subscription_is_desired(channel, Some(broadcaster_id)))
        .collect();
    let deferred_count = subscriptions.len();
    if deferred_count == 0 {
        return 0;
    }

    let mut queue = PENDING_SUBSCRIPTIONS.lock_or_recover();
    for (channel, broadcaster_id) in subscriptions {
        if let Some((_, queued_id)) = queue
            .iter_mut()
            .find(|(queued_channel, _)| queued_channel == &channel)
        {
            *queued_id = broadcaster_id;
        } else {
            queue.push((channel, broadcaster_id));
        }
    }
    deferred_count
}

/// Process all pending subscriptions
/// Called after connection is established.
///
/// Fetches fresh token and client_id for each subscription to ensure
/// we always use valid credentials, even if the token was refreshed
/// after the subscription was queued.
async fn process_pending_subscriptions() {
    if !auth_ready_for_eventsub() {
        return;
    }
    let auth_generation = EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire);

    if EVENTSUB_CLIENT
        .server_reconnect_pending
        .load(Ordering::Acquire)
        || EVENTSUB_CLIENT.reconnect_requested.load(Ordering::Acquire)
    {
        return;
    }

    if EVENTSUB_CLIENT.is_rate_limited() {
        return;
    }

    let pending: Vec<_> = {
        let mut queue = PENDING_SUBSCRIPTIONS.lock_or_recover();
        std::mem::take(&mut *queue)
    }
    .into_iter()
    .filter(|(channel, broadcaster_id)| subscription_is_desired(channel, Some(broadcaster_id)))
    .collect();

    if pending.is_empty() {
        return;
    }

    let token = match crate::backend::settings::get_token() {
        Some(t) => t,
        None => {
            crate::log_net!(
                warn,
                "No token available, re-queueing {} subscriptions",
                pending.len()
            );
            requeue_pending_subscriptions(pending);
            return;
        }
    };

    let client_id = crate::backend::constants::CLIENT_ID;

    crate::log_net!(
        info,
        "Processing {} pending subscriptions with fresh token...",
        pending.len()
    );

    let mut pending = pending.into_iter();
    while let Some((channel, broadcaster_id)) = pending.next() {
        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
            return;
        }
        if !subscription_is_desired(&channel, Some(&broadcaster_id)) {
            clear_subscription_retry(&channel);
            continue;
        }
        if subscription_retry_pending(&channel) {
            requeue_pending_subscriptions(std::iter::once((channel, broadcaster_id)));
            continue;
        }
        let outcome = match EVENTSUB_CLIENT
            .subscribe_to_channel(&channel, &broadcaster_id, &token, client_id)
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                crate::log_net!(
                    error,
                    "Failed to subscribe to {} ({}): {}",
                    channel,
                    broadcaster_id,
                    e
                );
                SubscriptionAttemptOutcome::Retryable
            }
        };

        if auth_generation != EVENTSUB_AUTH_GENERATION.load(Ordering::Acquire) {
            return;
        }

        match outcome {
            SubscriptionAttemptOutcome::DeferredByRateLimit => {
                let deferred_count = requeue_pending_subscriptions(
                    std::iter::once((channel, broadcaster_id)).chain(pending),
                );
                if deferred_count > 0 {
                    crate::log_net!(
                        info,
                        "Deferred {} EventSub channel subscriptions after rate limiting",
                        deferred_count
                    );
                }
                return;
            }
            SubscriptionAttemptOutcome::Retryable => {
                if schedule_subscription_retry(&channel) {
                    requeue_pending_subscriptions(std::iter::once((channel, broadcaster_id)));
                } else {
                    crate::log_net!(
                        warn,
                        "Abandoning EventSub subscriptions for {} after {} failed attempts; they are rebuilt on the next session",
                        channel,
                        MAX_SUBSCRIPTION_RETRIES
                    );
                }
            }
            SubscriptionAttemptOutcome::Complete => clear_subscription_retry(&channel),
        }
    }
}

/// Requeue all desired subscriptions into pending queue
/// Call this after reconnection to restore all subscriptions.
fn requeue_desired_subscriptions() {
    SUBSCRIPTION_RETRIES.lock_or_recover().clear();

    let desired = DESIRED_SUBSCRIPTIONS.read_or_recover();
    if desired.is_empty() {
        return;
    }

    let mut pending = PENDING_SUBSCRIPTIONS.lock_or_recover();

    let already_pending: std::collections::HashSet<String> =
        pending.iter().map(|(ch, _)| ch.clone()).collect();

    let mut added = 0;
    for (channel, desired_subscription) in desired.iter() {
        if !desired_subscription.is_owned() {
            continue;
        }
        let Some(broadcaster_id) = desired_subscription.broadcaster_id.as_ref() else {
            continue;
        };
        if !already_pending.contains(channel) {
            pending.push((channel.clone(), broadcaster_id.clone()));
            added += 1;
        }
    }

    if added > 0 {
        crate::log_net!(
            info,
            "Requeued {} desired subscriptions for reconnect recovery (total queue: {})",
            added,
            pending.len()
        );
    }
}
