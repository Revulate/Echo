//! Twitch API utilities
//! Provides helper functions for Twitch Helix API calls

use super::constants::{API_PROXY, API_PROXY_NOT_CONFIGURED, CLIENT_ID};
use super::http;
use super::settings::get_token;
use dashmap::DashMap;
use futures_util::stream::{self, StreamExt};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use url::Url;

use crate::backend::lock_utils::MutexExt;

/// Shared HTTP client for Twitch API requests - avoids creating new clients per request
pub(crate) static TWITCH_HTTP_CLIENT: LazyLock<http::ClientCache> =
    LazyLock::new(|| http::ClientCache::new(build_twitch_http_client));

/// Force the `TWITCH_HTTP_CLIENT` LazyLock to initialise now.
///
/// Called from the startup prewarm thread so that the first Twitch API call
/// does not pay the reqwest `Client` construction cost on the hot path.
pub fn prewarm_http_client() {
    let _ = &*TWITCH_HTTP_CLIENT;
}

fn build_twitch_http_client() -> reqwest::Client {
    http::client_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("Failed to create HTTP client for Twitch API: {error}");
            http::timed_client(Duration::from_secs(10))
        })
}

/// Attaches the Helix `Authorization: Bearer` and `Client-Id` headers.
pub(crate) trait HelixAuthExt {
    fn helix_auth(self, token: &str) -> Self;
}

impl HelixAuthExt for reqwest::RequestBuilder {
    fn helix_auth(self, token: &str) -> Self {
        self.header("Authorization", format!("Bearer {token}"))
            .header("Client-Id", CLIENT_ID)
    }
}

/// Renew the session after a Helix 401 for `rejected_token`. True when the
/// active account's session was renewed, so the user can simply try again.
pub(crate) async fn renew_helix_session(rejected_token: &str, detail: &str) -> bool {
    match super::auth::active_authenticated_user_id() {
        Some(owner) => renew_helix_session_for(&owner, rejected_token, detail)
            .await
            .is_some(),
        None => false,
    }
}

async fn renew_helix_session_for(
    owner: &str,
    rejected_token: &str,
    detail: &str,
) -> Option<String> {
    if super::auth::get_needs_reauth() {
        return None;
    }
    let token = super::auth::recover_helix_unauthorized(rejected_token, detail).await?;
    (super::auth::active_authenticated_user_id().as_deref() == Some(owner)).then_some(token)
}

/// Send an idempotent Helix request built by `build` for `token`. A 401 renews
/// the session once and re-sends the request while the same account is signed
/// in; any other outcome, or a 401 that could not be renewed, is returned as is.
pub(crate) async fn send_helix_idempotent(
    token: &str,
    build: impl Fn(&str) -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    send_helix_idempotent_with(token, false, build).await
}

/// [`send_helix_idempotent`] for high-frequency lookups, logged through
/// [`http::send_logged_quiet`].
async fn send_helix_idempotent_quiet(
    token: &str,
    build: impl Fn(&str) -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    send_helix_idempotent_with(token, true, build).await
}

async fn send_helix_idempotent_with(
    token: &str,
    quiet: bool,
    build: impl Fn(&str) -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    let send = |request: reqwest::RequestBuilder| async move {
        if quiet {
            http::send_logged_quiet(http::LogCategory::Net, request).await
        } else {
            http::send_logged(http::LogCategory::Net, request).await
        }
    };
    let owner = super::auth::active_authenticated_user_id();
    let response = send(build(token)).await?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    let Some(owner) = owner else {
        return Ok(response);
    };
    match renew_helix_session_for(&owner, token, "").await {
        Some(renewed) => send(build(&renewed)).await,
        None => Ok(response),
    }
}

/// Cache of channel login -> channel ID to avoid repeated API calls
/// This dramatically improves emote loading performance
static CHANNEL_ID_CACHE: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);
/// Logins Twitch definitively reported as missing or suspended, keyed by login.
static CHANNEL_LOOKUP_MISSES: LazyLock<DashMap<String, ChannelLookupState>> =
    LazyLock::new(DashMap::new);
const CHANNEL_LOOKUP_MISSES_MAX: usize = 512;
/// Last time a lookup failure was logged per `kind:key`, so a channel that
/// keeps failing produces one warning per [`LOOKUP_FAILURE_LOG_INTERVAL`].
static LOOKUP_FAILURE_LOGGED: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const LOOKUP_FAILURE_LOG_INTERVAL: Duration = Duration::from_secs(10 * 60);
const LOOKUP_FAILURE_LOG_MAX: usize = 512;
/// Reverse cache of channel ID -> channel login for shared-chat/source-room lookups
static CHANNEL_LOGIN_BY_ID: LazyLock<DashMap<String, String>> = LazyLock::new(DashMap::new);
static USER_PROFILE_CACHE: LazyLock<DashMap<String, CachedEntry<TwitchUserProfile>>> =
    LazyLock::new(DashMap::new);
static USER_PROFILE_STATUS: LazyLock<DashMap<String, UserCardFetchStatus>> =
    LazyLock::new(DashMap::new);
static USER_PROFILE_FETCHES_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static USER_FOLLOW_CACHE: LazyLock<DashMap<String, CachedEntry<TwitchUserFollowInfo>>> =
    LazyLock::new(DashMap::new);
static USER_FOLLOW_STATUS: LazyLock<DashMap<String, UserCardFetchStatus>> =
    LazyLock::new(DashMap::new);
static USER_FOLLOW_FETCHES_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static USER_SUBSCRIPTION_CACHE: LazyLock<DashMap<String, CachedEntry<TwitchUserSubscriptionInfo>>> =
    LazyLock::new(DashMap::new);
static USER_SUBSCRIPTION_STATUS: LazyLock<DashMap<String, UserCardFetchStatus>> =
    LazyLock::new(DashMap::new);
static USER_SUBSCRIPTION_FETCHES_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static USER_LIVE_CACHE: LazyLock<DashMap<String, CachedEntry<bool>>> = LazyLock::new(DashMap::new);
static USER_LIVE_LAST_ATTEMPT: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);
static USER_LIVE_OBSERVED_USERS: LazyLock<DashMap<String, ObservedUserLiveStatus>> =
    LazyLock::new(DashMap::new);
static USER_LIVE_FETCHES_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

const USER_CARD_SUCCESS_TTL: Duration = Duration::from_secs(5 * 60);
const USER_CARD_FAILURE_TTL: Duration = Duration::from_secs(45);
const USER_CARD_LOADING_TTL: Duration = Duration::from_secs(15);
/// Twitch live state is deliberately short-lived. Expired values remain visible
/// while a refresh runs so a LIVE marker never flickers off between requests.
const USER_LIVE_SUCCESS_TTL: Duration = Duration::from_secs(75);
const USER_LIVE_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const USER_LIVE_EVICTION_TTL: Duration = Duration::from_secs(15 * 60);
const USER_LIVE_HELIX_BATCH_MAX: usize = 100;
const USER_LIVE_PROXY_CONCURRENCY: usize = 8;

/// A proxy `/streams` or `/users` answer is reused for this long: the proxy
/// marks it `max-age=60` and its CDN keeps it about as long, so asking sooner
/// returns the same data and only spends the proxy's per-IP rate limit.
const PROXY_LOOKUP_REUSE: Duration = Duration::from_secs(60);
/// Pause after a proxy 429 without a usable `Retry-After`.
const PROXY_RATE_LIMIT_FALLBACK: Duration = Duration::from_secs(60);
/// Longest pause a 429 can ask for: the proxy's window is 60 s, so a larger
/// `Retry-After` (from a broken or hostile hop) must not disable lookups for the session.
const PROXY_RATE_LIMIT_MAX: Duration = Duration::from_secs(300);
/// Larger proxy answers are returned but not kept (a `/streams` or `/users` answer is a few KB).
const PROXY_LOOKUP_BODY_MAX: usize = 256 * 1024;
const PROXY_LOOKUP_CACHE_MAX: usize = 512;
static PROXY_LOOKUPS: LazyLock<Mutex<ProxyLookups>> = LazyLock::new(Mutex::default);

/// Size threshold above which CHANNEL_ID_CACHE / CHANNEL_LOGIN_BY_ID are cleared.
/// These are correctness caches that re-warm on demand; no TTL needed.
const CHANNEL_ID_CACHE_MAX: usize = 4096;

/// Thin wrapper that pairs a cached value with its insertion timestamp.
struct CachedEntry<T> {
    value: T,
    fetched_at: Instant,
}

impl<T: Clone> CachedEntry<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            fetched_at: Instant::now(),
        }
    }
}

struct UserLiveStatusRequest {
    username: Arc<str>,
    user_id: Option<Arc<str>>,
    channels: HashSet<Arc<str>>,
}

#[derive(Clone)]
struct ObservedUserLiveStatus {
    user_id: Option<Arc<str>>,
    channels: HashSet<Arc<str>>,
    observed_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserCardFetchState {
    Idle,
    Loading,
    Ready,
    Unavailable,
    Forbidden,
    AuthRequired,
    RetryableError,
    Error,
}

#[derive(Clone, Debug)]
pub struct UserCardFetchStatus {
    pub state: UserCardFetchState,
    pub message: Option<String>,
    pub updated_at: Instant,
    pub stale: bool,
    /// Credential generation that produced this status. Auth-required results
    /// can retry immediately after login/account/token changes without polling
    /// the same rejected token on every user-card render.
    pub auth_generation: u64,
}

impl UserCardFetchStatus {
    fn idle() -> Self {
        Self {
            state: UserCardFetchState::Idle,
            message: None,
            updated_at: Instant::now(),
            stale: false,
            auth_generation: super::auth::credential_generation(),
        }
    }

    fn loading() -> Self {
        Self {
            state: UserCardFetchState::Loading,
            message: None,
            updated_at: Instant::now(),
            stale: false,
            auth_generation: super::auth::credential_generation(),
        }
    }

    fn ready() -> Self {
        Self {
            state: UserCardFetchState::Ready,
            message: None,
            updated_at: Instant::now(),
            stale: false,
            auth_generation: super::auth::credential_generation(),
        }
    }

    fn with_message(state: UserCardFetchState, message: impl Into<String>, stale: bool) -> Self {
        Self {
            state,
            message: Some(message.into()),
            updated_at: Instant::now(),
            stale,
            auth_generation: super::auth::credential_generation(),
        }
    }
}

/// Cached Twitch user profile data for non-blocking user cards and overlays.
#[derive(Clone, Debug, Default)]
pub struct TwitchUserProfile {
    pub id: String,
    pub login: String,
    pub display_name: String,
    pub description: Option<String>,
    pub profile_image_url: Option<String>,
    pub created_at: Option<String>,
}

/// Cached channel-follow relationship and follower count for user cards.
#[derive(Clone, Debug, Default)]
pub struct TwitchUserFollowInfo {
    pub channel_login: String,
    pub user_id: String,
    pub user_login: String,
    pub followed_at: Option<String>,
    pub follower_count: Option<u64>,
}

/// Cached Twitch channel-subscription relationship for user cards.
#[derive(Clone, Debug, Default)]
pub struct TwitchUserSubscriptionInfo {
    pub channel_login: String,
    pub user_id: String,
    pub user_login: String,
    pub is_subscribed: bool,
    pub tier: Option<String>,
    pub plan_name: Option<String>,
    pub is_gift: bool,
    pub gifter_login: Option<String>,
    pub gifter_name: Option<String>,
}

/// Get cached channel ID (O(1) lookup, no network call)
pub fn get_cached_channel_id(channel_login: &str) -> Option<String> {
    let key = channel_login.to_lowercase();
    CHANNEL_ID_CACHE.get(&key).map(|v| v.clone())
}

/// What Echo knows about whether a channel login exists on Twitch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelLookupState {
    /// No lookup has answered yet.
    Unknown,
    /// Twitch returned a user id for the login.
    Found,
    /// Twitch answered the lookup with no such user (deleted, banned or never existed).
    NotFound,
    /// Twitch chat refused the join because the channel is suspended.
    Suspended,
}

/// Lookup state for a channel login, for empty-pane status text.
pub fn channel_lookup_state(channel: &str) -> ChannelLookupState {
    let key = channel.trim().trim_start_matches('#').to_lowercase();
    if let Some(state) = CHANNEL_LOOKUP_MISSES.get(&key) {
        return *state;
    }
    if CHANNEL_ID_CACHE.contains_key(&key) {
        ChannelLookupState::Found
    } else {
        ChannelLookupState::Unknown
    }
}

fn record_channel_lookup_miss(login: &str, state: ChannelLookupState) {
    let key = login.trim().trim_start_matches('#').to_lowercase();
    if key.is_empty() {
        return;
    }
    if CHANNEL_LOOKUP_MISSES.len() >= CHANNEL_LOOKUP_MISSES_MAX {
        CHANNEL_LOOKUP_MISSES.clear();
    }
    let previous = CHANNEL_LOOKUP_MISSES.insert(key, state);
    if previous != Some(state) {
        crate::backend::irc::bump_chat_version();
    }
}

/// Twitch chat reported the channel as suspended (NOTICE `msg_channel_suspended`).
pub(crate) fn note_channel_suspended(login: &str) {
    record_channel_lookup_miss(login, ChannelLookupState::Suspended);
}

/// Forget a missing/suspended verdict so a reopened channel is looked up afresh.
pub(crate) fn clear_channel_lookup_state(login: &str) {
    if CHANNEL_LOOKUP_MISSES.is_empty() {
        return;
    }
    CHANNEL_LOOKUP_MISSES.remove(&login.trim().trim_start_matches('#').to_lowercase());
}

/// Cache a channel ID (called when we learn the ID from IRC or API)
pub fn cache_channel_id(channel_login: &str, channel_id: &str) {
    let key = channel_login.to_lowercase();
    if !CHANNEL_LOOKUP_MISSES.is_empty() {
        CHANNEL_LOOKUP_MISSES.remove(&key);
    }
    CHANNEL_ID_CACHE.insert(key, channel_id.to_string());
    CHANNEL_LOGIN_BY_ID.insert(channel_id.to_string(), channel_login.to_lowercase());
}

/// Get cached Twitch user profile data by login or user ID.
pub fn get_cached_user_profile(login_or_id: &str) -> Option<TwitchUserProfile> {
    let key = login_or_id.trim().trim_start_matches('@').to_lowercase();
    if key.is_empty() {
        return None;
    }
    USER_PROFILE_CACHE
        .get(&key)
        .map(|entry| entry.value.clone())
}

pub fn get_user_profile_status(login_or_id: &str) -> UserCardFetchStatus {
    let key = login_or_id.trim().trim_start_matches('@').to_lowercase();
    if key.is_empty() {
        return UserCardFetchStatus::idle();
    }

    USER_PROFILE_STATUS
        .get(&key)
        .map(|status| status.value().clone())
        .unwrap_or_else(UserCardFetchStatus::idle)
}

/// Queue a background Twitch user profile fetch if the cache does not already have it.
pub fn queue_fetch_user_profile(username: &str) {
    queue_fetch_user_profile_inner(username, false);
}

fn queue_fetch_user_profile_inner(username: &str, force: bool) {
    let username = username.trim().trim_start_matches('@').to_lowercase();
    if username.is_empty() {
        return;
    }
    let current_status = get_user_profile_status(&username);
    if !force && !should_fetch_status(&current_status) {
        return;
    }

    {
        let mut in_flight = USER_PROFILE_FETCHES_IN_FLIGHT.lock_or_recover();
        if !in_flight.insert(username.clone()) {
            return;
        }
    }
    set_profile_status(
        &username,
        UserCardFetchStatus {
            stale: get_cached_user_profile(&username).is_some(),
            ..UserCardFetchStatus::loading()
        },
    );

    crate::backend::bridge::get_runtime().spawn(async move {
        let rejected_token = get_token();
        let mut result = fetch_user_profile(&username).await;
        let retry = match &result {
            Err(error) => refreshed_after_unauthorized(error, rejected_token.as_deref()).await,
            Ok(_) => false,
        };
        if retry {
            result = fetch_user_profile(&username).await;
        }
        if let Err(error) = result {
            set_profile_failure_status(&username, classify_profile_error(&error));
        }
        USER_PROFILE_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .remove(&username);
    });
}

fn cache_user_profile(profile: TwitchUserProfile) {
    let login_key = profile.login.to_lowercase();
    let profile_id = profile.id.clone();
    cache_channel_id(&profile.login, &profile_id);
    USER_PROFILE_CACHE.insert(login_key.clone(), CachedEntry::new(profile.clone()));
    USER_PROFILE_CACHE.insert(profile_id.clone(), CachedEntry::new(profile));
    let ready = UserCardFetchStatus::ready();
    USER_PROFILE_STATUS.insert(login_key.clone(), ready.clone());
    USER_PROFILE_STATUS.insert(profile_id, ready);
    publish_user_card_metadata_update(None, &login_key);
}

#[derive(Clone)]
pub struct UserCardMetadataUpdate {
    pub channel: Option<String>,
    pub username: String,
}

static USER_CARD_METADATA_UPDATES: LazyLock<broadcast::Sender<UserCardMetadataUpdate>> =
    LazyLock::new(|| broadcast::channel(64).0);

pub fn subscribe_user_card_metadata_updates() -> broadcast::Receiver<UserCardMetadataUpdate> {
    USER_CARD_METADATA_UPDATES.subscribe()
}

fn publish_user_card_metadata_update(channel: Option<&str>, username: &str) {
    let _ = USER_CARD_METADATA_UPDATES.send(UserCardMetadataUpdate {
        channel: channel.map(str::to_owned),
        username: username.to_owned(),
    });
}

fn normalize_live_status_login(username: &str) -> String {
    username.trim().trim_start_matches('@').to_lowercase()
}

/// Return the last successful Twitch live-state result for a user.
///
/// The value intentionally remains readable after its refresh TTL. Callers keep
/// displaying the last good state while the visible-user loader refreshes it,
/// which avoids a periodic LIVE-marker flicker.
pub fn get_cached_user_live_status(username: &str) -> Option<bool> {
    let username = normalize_live_status_login(username);
    if username.is_empty() {
        return None;
    }
    USER_LIVE_CACHE.get(&username).map(|entry| entry.value)
}

pub fn is_user_live_status_pending(username: &str) -> bool {
    let username = normalize_live_status_login(username);
    !username.is_empty()
        && USER_LIVE_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .contains(&username)
}

/// Queue a live-state lookup for a user card. Visible chat uses the batched
/// variant below so one Helix request can answer up to 100 names.
pub fn queue_user_live_status(username: &str) {
    let username = normalize_live_status_login(username);
    if username.is_empty() {
        return;
    }

    let request = UserLiveStatusRequest {
        username: Arc::from(username.as_str()),
        user_id: None,
        channels: HashSet::new(),
    };
    queue_user_live_status_requests(vec![request]);
}

/// Queue live-state refreshes for the bounded visible-user scan. Repeated rows
/// from one sender are already deduplicated by the canvas; this second merge
/// preserves every channel that needs a targeted layout invalidation.
pub fn queue_visible_user_live_status_batch(users: &[(Arc<str>, Arc<str>, Arc<str>)]) {
    let mut requests: HashMap<String, UserLiveStatusRequest> = HashMap::new();
    for (user_id, username, channel) in users {
        let login = normalize_live_status_login(username);
        if login.is_empty() {
            continue;
        }
        let channel = Arc::<str>::from(channel.to_ascii_lowercase());
        requests
            .entry(login.clone())
            .and_modify(|request| {
                request.channels.insert(Arc::clone(&channel));
                if request.user_id.is_none() && !user_id.trim().is_empty() {
                    request.user_id = Some(Arc::clone(user_id));
                }
            })
            .or_insert_with(|| UserLiveStatusRequest {
                username: Arc::from(login.as_str()),
                user_id: (!user_id.trim().is_empty()).then(|| Arc::clone(user_id)),
                channels: HashSet::from([channel]),
            });
    }

    queue_user_live_status_requests(requests.into_values().collect());
}

fn queue_user_live_status_requests(requests: Vec<UserLiveStatusRequest>) {
    // Without the proxy only Helix can answer, and that needs a login.
    if API_PROXY.is_none() && super::auth::capture_authenticated_resource_fence().is_none() {
        return;
    }
    let now = Instant::now();
    let mut queued = Vec::with_capacity(requests.len());
    for request in requests {
        let username = request.username.as_ref();
        record_user_live_status_observation(&request);
        let cache_is_fresh = USER_LIVE_CACHE
            .get(username)
            .is_some_and(|entry| now.duration_since(entry.fetched_at) < USER_LIVE_SUCCESS_TTL);
        let attempt_is_recent = USER_LIVE_LAST_ATTEMPT.get(username).is_some_and(|attempt| {
            now.duration_since(*attempt.value()) < USER_LIVE_FAILURE_BACKOFF
        });
        if cache_is_fresh || attempt_is_recent {
            continue;
        }

        if !USER_LIVE_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .insert(username.to_string())
        {
            continue;
        }
        USER_LIVE_LAST_ATTEMPT.insert(username.to_string(), now);
        queued.push(request);
    }

    if queued.is_empty() {
        return;
    }

    crate::backend::bridge::get_runtime().spawn(async move {
        let logins: Vec<String> = queued
            .iter()
            .map(|request| request.username.to_string())
            .collect();
        let mut results = fetch_user_live_states(&logins).await;

        for request in queued {
            let username = request.username.as_ref();
            match results.remove(username).unwrap_or_else(|| {
                Err("Twitch live-state batch omitted a requested login".to_string())
            }) {
                Ok(is_live) => publish_user_live_status(&request, is_live),
                Err(error) => crate::log_net!(
                    debug,
                    "Failed to refresh Twitch live state for {}: {}",
                    username,
                    error
                ),
            }
            USER_LIVE_FETCHES_IN_FLIGHT
                .lock_or_recover()
                .remove(username);
            publish_user_card_metadata_update(None, username);
        }
    });
}

fn record_user_live_status_observation(request: &UserLiveStatusRequest) {
    const USER_LIVE_OBSERVED_MAX: usize = 2_000;
    if USER_LIVE_OBSERVED_USERS.len() >= USER_LIVE_OBSERVED_MAX {
        let prune_count = USER_LIVE_OBSERVED_MAX / 10;
        let to_remove: Vec<String> = USER_LIVE_OBSERVED_USERS
            .iter()
            .take(prune_count)
            .map(|entry| entry.key().clone())
            .collect();
        for username in to_remove {
            USER_LIVE_OBSERVED_USERS.remove(&username);
        }
    }

    USER_LIVE_OBSERVED_USERS
        .entry(request.username.to_string())
        .and_modify(|observed| {
            if observed.user_id.is_none() {
                observed.user_id = request.user_id.clone();
            }
            observed.channels.extend(request.channels.iter().cloned());
            observed.observed_at = Instant::now();
        })
        .or_insert_with(|| ObservedUserLiveStatus {
            user_id: request.user_id.clone(),
            channels: request.channels.clone(),
            observed_at: Instant::now(),
        });
}

fn publish_user_live_status(request: &UserLiveStatusRequest, is_live: bool) {
    let username = request.username.as_ref();
    let previous = USER_LIVE_CACHE.get(username).map(|entry| entry.value);
    USER_LIVE_CACHE.insert(username.to_string(), CachedEntry::new(is_live));

    if previous == Some(is_live) || (previous.is_none() && !is_live) {
        return;
    }
    let observed = USER_LIVE_OBSERVED_USERS
        .get(username)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| ObservedUserLiveStatus {
            user_id: request.user_id.clone(),
            channels: request.channels.clone(),
            observed_at: Instant::now(),
        });
    for channel in &observed.channels {
        crate::backend::observers::get_observer_registry().notify(
            channel.as_ref(),
            crate::backend::observers::ChannelEvent::UserDecorationsChanged {
                user_login: Arc::clone(&request.username),
                user_id: observed.user_id.clone(),
            },
        );
    }
}

async fn fetch_user_live_states(logins: &[String]) -> HashMap<String, Result<bool, String>> {
    if let Some(auth) = super::auth::capture_authenticated_resource_fence() {
        let mut results = HashMap::with_capacity(logins.len());
        for chunk in logins.chunks(USER_LIVE_HELIX_BATCH_MAX) {
            let response = fetch_live_logins_helix(chunk, auth.access_token()).await;
            if !auth.is_current() {
                for login in chunk {
                    results.insert(
                        login.clone(),
                        Err(
                            "Authenticated Twitch account changed during live-state lookup"
                                .to_string(),
                        ),
                    );
                }
                continue;
            }
            match response {
                Ok(live_logins) => {
                    for login in chunk {
                        results.insert(login.clone(), Ok(live_logins.contains(login)));
                    }
                }
                Err(error) => {
                    for login in chunk {
                        results.insert(login.clone(), Err(error.clone()));
                    }
                }
            }
        }
        return results;
    }

    stream::iter(logins.iter().cloned().map(|login| async move {
        let result = fetch_user_live_state_proxy(&login).await;
        (login, result)
    }))
    .buffer_unordered(USER_LIVE_PROXY_CONCURRENCY)
    .collect()
    .await
}

/// One Echo API proxy answer.
#[derive(Clone)]
struct ProxyAnswer {
    status: reqwest::StatusCode,
    body: Arc<[u8]>,
}

impl ProxyAnswer {
    /// Stands in for a request withheld while the proxy is rate limiting us.
    fn rate_limited() -> Self {
        Self {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body: Arc::from([].as_slice()),
        }
    }

    /// Stands in for every request in a build without [`API_PROXY`], so
    /// callers take the same Helix-or-error path as when the proxy is down.
    fn not_configured() -> Self {
        Self {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            body: Arc::from([].as_slice()),
        }
    }

    fn json<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }
}

/// Recent proxy `/streams` and `/users` answers per path, and the pause after
/// a 429. The proxy limits each client IP, so a 429 pauses every lookup,
/// whatever the login.
#[derive(Default)]
struct ProxyLookups {
    answers: HashMap<String, (ProxyAnswer, Instant)>,
    paused_until: Option<Instant>,
}

impl ProxyLookups {
    /// The answer for `path` without a request: a local 429 while paused,
    /// else an answer from the last [`PROXY_LOOKUP_REUSE`].
    fn reuse(&self, path: &str, now: Instant) -> Option<ProxyAnswer> {
        if self.paused_until.is_some_and(|until| now < until) {
            return Some(ProxyAnswer::rate_limited());
        }
        self.answers
            .get(path)
            .filter(|(_, at)| now.saturating_duration_since(*at) < PROXY_LOOKUP_REUSE)
            .map(|(answer, _)| answer.clone())
    }

    /// Pause every lookup for the 429's `Retry-After` (at most
    /// [`PROXY_RATE_LIMIT_MAX`]), or [`PROXY_RATE_LIMIT_FALLBACK`] without one.
    /// Never shortens a pause.
    fn pause(&mut self, retry_after: Option<Duration>, now: Instant) -> Duration {
        let wait = retry_after
            .unwrap_or(PROXY_RATE_LIMIT_FALLBACK)
            .min(PROXY_RATE_LIMIT_MAX);
        let until = now
            .checked_add(wait)
            .unwrap_or(now + PROXY_RATE_LIMIT_FALLBACK);
        self.paused_until = Some(self.paused_until.map_or(until, |paused| paused.max(until)));
        wait
    }

    fn record(&mut self, path: &str, answer: ProxyAnswer, now: Instant) {
        if !self.answers.contains_key(path) && self.answers.len() >= PROXY_LOOKUP_CACHE_MAX {
            self.answers
                .retain(|_, (_, at)| now.saturating_duration_since(*at) < PROXY_LOOKUP_REUSE);
            if self.answers.len() >= PROXY_LOOKUP_CACHE_MAX
                && let Some(oldest) = self
                    .answers
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(path, _)| path.clone())
            {
                self.answers.remove(&oldest);
            }
        }
        self.answers.insert(path.to_string(), (answer, now));
    }
}

fn proxy_lookup_path(endpoint: &str, login: &str) -> String {
    format!("{endpoint}/{}", urlencoding::encode(&login.to_lowercase()))
}

/// GET `{proxy}/{endpoint}/{login}` on [`API_PROXY`] (`streams` or `users`). Reuses an
/// answer from the last [`PROXY_LOOKUP_REUSE`], and after a 429 sends nothing
/// until its `Retry-After` has passed, answering with a local 429 instead.
/// Without a configured proxy it answers with a local 503.
/// `quiet` logs through [`http::send_logged_quiet`].
async fn proxy_lookup(endpoint: &str, login: &str, quiet: bool) -> reqwest::Result<ProxyAnswer> {
    let Some((proxy_url, proxy_key)) = API_PROXY else {
        return Ok(ProxyAnswer::not_configured());
    };
    let path = proxy_lookup_path(endpoint, login);
    let reused = PROXY_LOOKUPS.lock_or_recover().reuse(&path, Instant::now());
    if let Some(answer) = reused {
        return Ok(answer);
    }
    let request = TWITCH_HTTP_CLIENT
        .get(format!("{proxy_url}/{path}"))
        .header("X-Echo-Key", proxy_key);
    let response = if quiet {
        http::send_logged_quiet(http::LogCategory::Net, request).await?
    } else {
        http::send_logged(http::LogCategory::Net, request).await?
    };
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = super::retry::server_retry_wait(response.headers());
        let wait = PROXY_LOOKUPS
            .lock_or_recover()
            .pause(retry_after, Instant::now());
        crate::log_net!(
            debug,
            "Echo API proxy rate limited; pausing stream/user lookups for {wait:?}"
        );
        return Ok(ProxyAnswer::rate_limited());
    }
    let answer = ProxyAnswer {
        status,
        body: Arc::from(response.bytes().await?.as_ref()),
    };
    // Only real answers are reused: an error (5xx, 401) is asked again next time.
    let reusable = matches!(
        status,
        reqwest::StatusCode::OK | reqwest::StatusCode::NOT_FOUND
    ) && answer.body.len() <= PROXY_LOOKUP_BODY_MAX;
    if reusable {
        PROXY_LOOKUPS
            .lock_or_recover()
            .record(&path, answer.clone(), Instant::now());
    }
    Ok(answer)
}

/// Forget the reused proxy `/streams` answer for `login`: EventSub just
/// reported a live flip that the answer predates, and the confirming refresh
/// must not put the old state back.
pub(crate) fn forget_proxy_stream_lookup(login: &str) {
    PROXY_LOOKUPS
        .lock_or_recover()
        .answers
        .remove(&proxy_lookup_path("streams", login));
}

async fn fetch_user_live_state_proxy(username: &str) -> Result<bool, String> {
    let response = proxy_lookup("streams", username, true)
        .await
        .map_err(|error| {
            format!(
                "Twitch live-state proxy request failed: {}",
                error.without_url()
            )
        })?;
    if !response.status.is_success() {
        return Err(format!(
            "Twitch live-state proxy request returned status {}",
            response.status
        ));
    }
    let data: StreamsResponse = response
        .json()
        .map_err(|error| format!("Failed to parse Twitch live-state proxy response: {error}"))?;
    Ok(data
        .data
        .first()
        .is_some_and(|stream| stream.stream_type == "live"))
}

async fn fetch_live_logins_helix(
    logins: &[String],
    token: &str,
) -> Result<HashSet<String>, String> {
    let url = live_logins_helix_url(logins);
    let response = send_helix_idempotent_quiet(token, |token| {
        TWITCH_HTTP_CLIENT.get(url.clone()).helix_auth(token)
    })
    .await
    .map_err(|error| {
        format!(
            "Twitch live-state batch request failed: {}",
            error.without_url()
        )
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "Twitch live-state batch request returned status {}",
            response.status()
        ));
    }
    let data: StreamsResponse = response
        .json()
        .await
        .map_err(|error| format!("Failed to parse Twitch live-state batch: {error}"))?;
    Ok(data
        .data
        .into_iter()
        .filter(|stream| stream.stream_type == "live")
        .map(|stream| stream.user_login.to_lowercase())
        .collect())
}

fn live_logins_helix_url(logins: &[String]) -> Url {
    let mut url = Url::parse("https://api.twitch.tv/helix/streams")
        .expect("static Twitch Helix streams URL must be valid");
    url.query_pairs_mut()
        .extend_pairs(logins.iter().map(|login| ("user_login", login)));
    url
}

#[cfg(test)]
mod lookup_failure_log_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn logs_once_per_key_per_interval() {
        let mut logged = HashMap::new();
        let start = Instant::now();
        assert!(should_log_lookup_failure(&mut logged, "id:a".into(), start));
        assert!(!should_log_lookup_failure(
            &mut logged,
            "id:a".into(),
            start
        ));
        assert!(should_log_lookup_failure(&mut logged, "id:b".into(), start));
        let later = start + LOOKUP_FAILURE_LOG_INTERVAL;
        assert!(should_log_lookup_failure(&mut logged, "id:a".into(), later));
    }

    #[test]
    fn cap_is_hard_and_evicts_oldest() {
        let mut logged = HashMap::new();
        let start = Instant::now();
        for index in 0..LOOKUP_FAILURE_LOG_MAX + 8 {
            let now = start + Duration::from_millis(index as u64);
            assert!(should_log_lookup_failure(
                &mut logged,
                format!("id:{index}"),
                now
            ));
            assert!(logged.len() <= LOOKUP_FAILURE_LOG_MAX);
        }
        assert!(!logged.contains_key("id:0"));
    }
}

#[cfg(test)]
mod live_logins_helix_url_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn preserves_repeated_login_order_and_query_encoding() {
        let logins = vec!["first_user".to_string(), "reserved+value".to_string()];
        let url = live_logins_helix_url(&logins);
        let pairs = url.query_pairs().into_owned().collect::<Vec<_>>();

        assert_eq!(
            pairs,
            vec![
                ("user_login".to_string(), "first_user".to_string()),
                ("user_login".to_string(), "reserved+value".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod proxy_lookup_tests {
    use super::*;
    use core::prelude::v1::test;
    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    fn ok() -> ProxyAnswer {
        ProxyAnswer {
            status: StatusCode::OK,
            body: Arc::from(br#"{"data":[]}"#.as_slice()),
        }
    }

    fn retry_after(value: &'static str) -> Option<Duration> {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static(value));
        crate::backend::retry::server_retry_wait(&headers)
    }

    fn is_paused(lookups: &ProxyLookups, path: &str, now: Instant) -> bool {
        lookups
            .reuse(path, now)
            .is_some_and(|answer| answer.status == StatusCode::TOO_MANY_REQUESTS)
    }

    #[test]
    fn reuses_a_logins_answer_for_sixty_seconds() {
        let mut lookups = ProxyLookups::default();
        let start = Instant::now();
        let path = proxy_lookup_path("streams", "FinalFantasyXIV");
        assert_eq!(path, "streams/finalfantasyxiv");
        assert!(lookups.reuse(&path, start).is_none());

        lookups.record(&path, ok(), start);
        let reused = lookups
            .reuse(&path, start + Duration::from_secs(59))
            .expect("answer reused within 60 s");
        assert_eq!(reused.status, StatusCode::OK);
        assert!(reused.json::<StreamsResponse>().unwrap().data.is_empty());
        assert!(lookups.reuse("users/finalfantasyxiv", start).is_none());
        assert!(lookups.reuse(&path, start + PROXY_LOOKUP_REUSE).is_none());
    }

    #[test]
    fn rate_limit_pauses_every_login_until_retry_after() {
        let mut lookups = ProxyLookups::default();
        let start = Instant::now();
        lookups.record("users/a", ok(), start);

        let wait = lookups.pause(retry_after("17"), start);
        assert_eq!(wait, Duration::from_secs(17));
        for path in ["users/a", "streams/a", "streams/b"] {
            assert!(is_paused(&lookups, path, start + Duration::from_secs(16)));
        }
        assert!(!is_paused(&lookups, "streams/b", start + wait));
        assert!(lookups.reuse("streams/b", start + wait).is_none());
    }

    #[test]
    fn missing_or_unparseable_retry_after_pauses_sixty_seconds() {
        for header in [None, retry_after("soon"), retry_after("-1")] {
            let mut lookups = ProxyLookups::default();
            let start = Instant::now();
            assert_eq!(lookups.pause(header, start), PROXY_RATE_LIMIT_FALLBACK);
            assert!(is_paused(
                &lookups,
                "streams/a",
                start + Duration::from_secs(59)
            ));
            assert!(!is_paused(
                &lookups,
                "streams/a",
                start + Duration::from_secs(60)
            ));
        }
    }

    #[test]
    fn a_later_429_never_shortens_the_pause() {
        let mut lookups = ProxyLookups::default();
        let start = Instant::now();
        lookups.pause(Some(Duration::from_secs(60)), start);
        lookups.pause(Some(Duration::from_secs(1)), start + Duration::from_secs(1));
        assert!(is_paused(
            &lookups,
            "users/a",
            start + Duration::from_secs(59)
        ));

        let mut huge = ProxyLookups::default();
        huge.pause(Some(Duration::MAX), start);
        assert!(is_paused(&huge, "users/a", start + Duration::from_secs(59)));
    }

    #[test]
    fn a_huge_retry_after_pauses_at_most_five_minutes() {
        let mut lookups = ProxyLookups::default();
        let start = Instant::now();
        let wait = lookups.pause(retry_after("4000000000"), start);
        assert_eq!(wait, PROXY_RATE_LIMIT_MAX);
        assert!(is_paused(
            &lookups,
            "users/a",
            start + PROXY_RATE_LIMIT_MAX - Duration::from_secs(1)
        ));
        assert!(!is_paused(
            &lookups,
            "users/a",
            start + PROXY_RATE_LIMIT_MAX
        ));
    }

    #[test]
    fn answer_cache_is_bounded() {
        let mut lookups = ProxyLookups::default();
        let start = Instant::now();
        for index in 0..PROXY_LOOKUP_CACHE_MAX + 8 {
            let now = start + Duration::from_millis(index as u64);
            lookups.record(&format!("users/{index}"), ok(), now);
            assert!(lookups.answers.len() <= PROXY_LOOKUP_CACHE_MAX);
        }
        assert!(!lookups.answers.contains_key("users/0"));
        assert!(
            lookups
                .answers
                .contains_key(&format!("users/{}", PROXY_LOOKUP_CACHE_MAX + 7))
        );
    }
}

fn follow_cache_key(channel_login: &str, user_key: &str) -> String {
    format!(
        "{}:{}",
        channel_login.trim().trim_start_matches('#').to_lowercase(),
        user_key.trim().trim_start_matches('@').to_lowercase()
    )
}

fn subscription_cache_key(channel_login: &str, user_key: &str) -> String {
    format!(
        "{}:{}",
        channel_login.trim().trim_start_matches('#').to_lowercase(),
        user_key.trim().trim_start_matches('@').to_lowercase()
    )
}

/// Get cached channel-follow metadata by target login or user ID.
pub fn get_cached_user_follow(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> Option<TwitchUserFollowInfo> {
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty())
        && let Some(entry) = USER_FOLLOW_CACHE.get(&follow_cache_key(channel_login, user_id))
    {
        return Some(entry.value.clone());
    }

    let user_login = user_login.trim().trim_start_matches('@');
    if user_login.is_empty() {
        return None;
    }

    USER_FOLLOW_CACHE
        .get(&follow_cache_key(channel_login, user_login))
        .map(|entry| entry.value.clone())
}

pub fn get_user_follow_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> UserCardFetchStatus {
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty())
        && let Some(status) = USER_FOLLOW_STATUS.get(&follow_cache_key(channel_login, user_id))
    {
        return status.value().clone();
    }

    let user_login = user_login.trim().trim_start_matches('@');
    if user_login.is_empty() {
        return UserCardFetchStatus::idle();
    }

    USER_FOLLOW_STATUS
        .get(&follow_cache_key(channel_login, user_login))
        .map(|status| status.value().clone())
        .unwrap_or_else(UserCardFetchStatus::idle)
}

/// Queue a background Twitch channel-follow lookup if needed.
pub fn queue_fetch_user_follow(channel_login: &str, user_login: &str, user_id: Option<&str>) {
    queue_fetch_user_follow_inner(channel_login, user_login, user_id, false);
}

fn queue_fetch_user_follow_inner(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    force: bool,
) {
    let channel_login = channel_login.trim().trim_start_matches('#').to_lowercase();
    let user_login = user_login.trim().trim_start_matches('@').to_lowercase();
    let user_id = user_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    if channel_login.is_empty() || user_login.is_empty() {
        return;
    }
    let current_status = get_user_follow_status(&channel_login, &user_login, user_id.as_deref());
    if !force && !should_fetch_authenticated_status(&current_status) {
        return;
    }

    let in_flight_key = follow_cache_key(
        &channel_login,
        user_id.as_deref().unwrap_or(user_login.as_str()),
    );
    {
        let mut in_flight = USER_FOLLOW_FETCHES_IN_FLIGHT.lock_or_recover();
        if !in_flight.insert(in_flight_key.clone()) {
            return;
        }
    }
    cache_user_follow_status(
        &channel_login,
        &user_login,
        user_id.as_deref(),
        UserCardFetchStatus {
            stale: get_cached_user_follow(&channel_login, &user_login, user_id.as_deref())
                .is_some(),
            ..UserCardFetchStatus::loading()
        },
    );

    let mut request_auth = super::auth::capture_authenticated_resource_fence();
    crate::backend::bridge::get_runtime().spawn(async move {
        let mut result = fetch_user_follow(&channel_login, &user_login, user_id.as_deref()).await;
        let retry = match &result {
            Err(error) => {
                refreshed_after_unauthorized(
                    error,
                    request_auth.as_ref().map(|auth| auth.access_token()),
                )
                .await
            }
            Ok(_) => false,
        };
        if retry {
            request_auth = super::auth::capture_authenticated_resource_fence();
            result = fetch_user_follow(&channel_login, &user_login, user_id.as_deref()).await;
        }
        if let Err(error) = result
            && authenticated_request_context_is_current(request_auth.as_ref())
        {
            set_follow_failure_status(
                &channel_login,
                &user_login,
                user_id.as_deref(),
                classify_follow_or_subscription_error(&error, "follow"),
            );
        }
        USER_FOLLOW_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .remove(&in_flight_key);
    });
}

fn cache_user_follow(info: TwitchUserFollowInfo) {
    let channel_login = info.channel_login.clone();
    let user_id = info.user_id.clone();
    let user_login = info.user_login.clone();
    USER_FOLLOW_CACHE.insert(
        follow_cache_key(&channel_login, &user_id),
        CachedEntry::new(info.clone()),
    );
    USER_FOLLOW_CACHE.insert(
        follow_cache_key(&channel_login, &user_login),
        CachedEntry::new(info),
    );
    let ready = UserCardFetchStatus::ready();
    USER_FOLLOW_STATUS.insert(follow_cache_key(&channel_login, &user_id), ready.clone());
    USER_FOLLOW_STATUS.insert(follow_cache_key(&channel_login, &user_login), ready);
    publish_user_card_metadata_update(Some(&channel_login), &user_login);
}

/// Get cached channel-subscription metadata by target login or user ID.
pub fn get_cached_user_subscription(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> Option<TwitchUserSubscriptionInfo> {
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty())
        && let Some(entry) =
            USER_SUBSCRIPTION_CACHE.get(&subscription_cache_key(channel_login, user_id))
    {
        return Some(entry.value.clone());
    }

    let user_login = user_login.trim().trim_start_matches('@');
    if user_login.is_empty() {
        return None;
    }

    USER_SUBSCRIPTION_CACHE
        .get(&subscription_cache_key(channel_login, user_login))
        .map(|entry| entry.value.clone())
}

pub fn get_user_subscription_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> UserCardFetchStatus {
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty())
        && let Some(status) =
            USER_SUBSCRIPTION_STATUS.get(&subscription_cache_key(channel_login, user_id))
    {
        return status.value().clone();
    }

    let user_login = user_login.trim().trim_start_matches('@');
    if user_login.is_empty() {
        return UserCardFetchStatus::idle();
    }

    USER_SUBSCRIPTION_STATUS
        .get(&subscription_cache_key(channel_login, user_login))
        .map(|status| status.value().clone())
        .unwrap_or_else(UserCardFetchStatus::idle)
}

/// Queue a background Twitch channel-subscription lookup if needed.
pub fn queue_fetch_user_subscription(channel_login: &str, user_login: &str, user_id: Option<&str>) {
    queue_fetch_user_subscription_inner(channel_login, user_login, user_id, false);
}

fn queue_fetch_user_subscription_inner(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    force: bool,
) {
    let channel_login = channel_login.trim().trim_start_matches('#').to_lowercase();
    let user_login = user_login.trim().trim_start_matches('@').to_lowercase();
    let user_id = user_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    if channel_login.is_empty() || user_login.is_empty() {
        return;
    }
    let current_status =
        get_user_subscription_status(&channel_login, &user_login, user_id.as_deref());
    if !force && !should_fetch_authenticated_status(&current_status) {
        return;
    }

    let in_flight_key = subscription_cache_key(
        &channel_login,
        user_id.as_deref().unwrap_or(user_login.as_str()),
    );
    {
        let mut in_flight = USER_SUBSCRIPTION_FETCHES_IN_FLIGHT.lock_or_recover();
        if !in_flight.insert(in_flight_key.clone()) {
            return;
        }
    }
    cache_user_subscription_status(
        &channel_login,
        &user_login,
        user_id.as_deref(),
        UserCardFetchStatus {
            stale: get_cached_user_subscription(&channel_login, &user_login, user_id.as_deref())
                .is_some(),
            ..UserCardFetchStatus::loading()
        },
    );

    let mut request_auth = super::auth::capture_authenticated_resource_fence();
    crate::backend::bridge::get_runtime().spawn(async move {
        let mut result =
            fetch_user_subscription(&channel_login, &user_login, user_id.as_deref()).await;
        let retry = match &result {
            Err(error) => {
                refreshed_after_unauthorized(
                    error,
                    request_auth.as_ref().map(|auth| auth.access_token()),
                )
                .await
            }
            Ok(_) => false,
        };
        if retry {
            request_auth = super::auth::capture_authenticated_resource_fence();
            result = fetch_user_subscription(&channel_login, &user_login, user_id.as_deref()).await;
        }
        if let Err(error) = result
            && authenticated_request_context_is_current(request_auth.as_ref())
        {
            set_subscription_failure_status(
                &channel_login,
                &user_login,
                user_id.as_deref(),
                classify_follow_or_subscription_error(&error, "subscription"),
            );
        }
        USER_SUBSCRIPTION_FETCHES_IN_FLIGHT
            .lock_or_recover()
            .remove(&in_flight_key);
    });
}

fn cache_user_subscription(info: TwitchUserSubscriptionInfo) {
    let channel_login = info.channel_login.clone();
    let user_id = info.user_id.clone();
    let user_login = info.user_login.clone();
    USER_SUBSCRIPTION_CACHE.insert(
        subscription_cache_key(&channel_login, &user_id),
        CachedEntry::new(info.clone()),
    );
    USER_SUBSCRIPTION_CACHE.insert(
        subscription_cache_key(&channel_login, &user_login),
        CachedEntry::new(info),
    );
    let ready = UserCardFetchStatus::ready();
    USER_SUBSCRIPTION_STATUS.insert(
        subscription_cache_key(&channel_login, &user_id),
        ready.clone(),
    );
    USER_SUBSCRIPTION_STATUS.insert(subscription_cache_key(&channel_login, &user_login), ready);
    publish_user_card_metadata_update(Some(&channel_login), &user_login);
}

fn set_profile_status(username: &str, status: UserCardFetchStatus) {
    let finished = status.state != UserCardFetchState::Loading;
    USER_PROFILE_STATUS.insert(username.to_string(), status.clone());
    if let Some(entry) = USER_PROFILE_CACHE.get(username) {
        USER_PROFILE_STATUS.insert(entry.value.id.clone(), status);
    }
    if finished {
        publish_user_card_metadata_update(None, username);
    }
}

fn cache_user_follow_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    status: UserCardFetchStatus,
) {
    let finished = status.state != UserCardFetchState::Loading;
    USER_FOLLOW_STATUS.insert(follow_cache_key(channel_login, user_login), status.clone());
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty()) {
        USER_FOLLOW_STATUS.insert(follow_cache_key(channel_login, user_id), status);
    }
    if finished {
        publish_user_card_metadata_update(Some(channel_login), user_login);
    }
}

fn cache_user_subscription_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    status: UserCardFetchStatus,
) {
    let finished = status.state != UserCardFetchState::Loading;
    USER_SUBSCRIPTION_STATUS.insert(
        subscription_cache_key(channel_login, user_login),
        status.clone(),
    );
    if let Some(user_id) = user_id.filter(|id| !id.trim().is_empty()) {
        USER_SUBSCRIPTION_STATUS.insert(subscription_cache_key(channel_login, user_id), status);
    }
    if finished {
        publish_user_card_metadata_update(Some(channel_login), user_login);
    }
}

fn set_profile_failure_status(username: &str, status: UserCardFetchStatus) {
    set_profile_status(
        username,
        UserCardFetchStatus {
            stale: get_cached_user_profile(username).is_some(),
            ..status
        },
    );
}

fn set_follow_failure_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    status: UserCardFetchStatus,
) {
    cache_user_follow_status(
        channel_login,
        user_login,
        user_id,
        UserCardFetchStatus {
            stale: get_cached_user_follow(channel_login, user_login, user_id).is_some(),
            ..status
        },
    );
}

fn set_subscription_failure_status(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
    status: UserCardFetchStatus,
) {
    cache_user_subscription_status(
        channel_login,
        user_login,
        user_id,
        UserCardFetchStatus {
            stale: get_cached_user_subscription(channel_login, user_login, user_id).is_some(),
            ..status
        },
    );
}

fn should_fetch_status(status: &UserCardFetchStatus) -> bool {
    let ttl = match status.state {
        UserCardFetchState::Idle => return true,
        UserCardFetchState::Loading => USER_CARD_LOADING_TTL,
        UserCardFetchState::Ready => USER_CARD_SUCCESS_TTL,
        UserCardFetchState::Unavailable
        | UserCardFetchState::Forbidden
        | UserCardFetchState::AuthRequired
        | UserCardFetchState::RetryableError
        | UserCardFetchState::Error => USER_CARD_FAILURE_TTL,
    };

    status.updated_at.elapsed() >= ttl
}

fn should_fetch_authenticated_status(status: &UserCardFetchStatus) -> bool {
    if status.auth_generation != super::auth::credential_generation() {
        return true;
    }

    should_fetch_status(status)
}

/// True after a user-card lookup failed with a Helix 401 and the session was renewed,
/// so the caller retries once with the refreshed token.
async fn refreshed_after_unauthorized(error: &str, rejected_token: Option<&str>) -> bool {
    match rejected_token {
        Some(token) if error.contains("status 401") => {
            super::auth::recover_helix_unauthorized(token, "")
                .await
                .is_some()
        }
        _ => false,
    }
}

fn authenticated_request_context_is_current(
    request_auth: Option<&super::auth::AuthenticatedResourceFence>,
) -> bool {
    match request_auth {
        Some(auth) => auth.is_current(),
        None => super::auth::capture_authenticated_resource_fence().is_none(),
    }
}

fn classify_profile_error(error: &str) -> UserCardFetchStatus {
    classify_twitch_error(
        error,
        "Profile details need an authenticated Twitch account",
        "Profile details are not available with the current Twitch account",
        "Profile details are unavailable for this user",
        "Could not load Twitch profile details right now",
        "Profile details are temporarily unavailable",
    )
}

fn classify_follow_or_subscription_error(error: &str, label: &str) -> UserCardFetchStatus {
    let lower = error.to_ascii_lowercase();
    if lower.contains("no auth token") && crate::backend::bridge::is_authenticated() {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::RetryableError,
            format!("{label} details are temporarily unavailable while Twitch credentials refresh"),
            false,
        );
    }
    if lower.contains("status 401") && !lower.contains("no auth token") {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::Forbidden,
            format!("{label} details are not available with your permissions for this channel"),
            false,
        );
    }
    classify_twitch_error(
        error,
        &format!("{label} details need an authenticated Twitch account"),
        &format!("{label} details are not available with your permissions for this channel"),
        &format!("{label} details are unavailable for this user"),
        &format!("Could not load {label} details right now"),
        &format!("{label} details are temporarily unavailable"),
    )
}

fn classify_twitch_error(
    error: &str,
    auth_required_message: &str,
    forbidden_message: &str,
    unavailable_message: &str,
    generic_message: &str,
    retryable_message: &str,
) -> UserCardFetchStatus {
    let lower = error.to_ascii_lowercase();

    // A build without the proxy can only look users up through Helix, which needs a login.
    if lower.contains("no auth token")
        || lower.contains("status 401")
        || error.contains(API_PROXY_NOT_CONFIGURED)
    {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::AuthRequired,
            auth_required_message,
            false,
        );
    }
    if lower.contains("status 403") {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::Forbidden,
            forbidden_message,
            false,
        );
    }
    if lower.contains("status 404")
        || lower.contains("not found")
        || lower.contains("only viewable")
    {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::Unavailable,
            unavailable_message,
            false,
        );
    }
    if lower.contains("status 429")
        || lower.contains("status 500")
        || lower.contains("status 502")
        || lower.contains("status 503")
        || lower.contains("status 504")
        || lower.contains("timeout")
        || lower.contains("temporar")
        || lower.contains("connection")
        || lower.contains("account changed")
        || lower.contains("superseded")
    {
        return UserCardFetchStatus::with_message(
            UserCardFetchState::RetryableError,
            retryable_message,
            false,
        );
    }

    UserCardFetchStatus::with_message(UserCardFetchState::Error, generic_message, false)
}

/// Evict stale entries from all user-card caches.
///
/// Called periodically from the polling coordinator's slow maintenance loop.
/// - Value caches (profile/follow/subscription): retain entries younger than
///   `4 * USER_CARD_SUCCESS_TTL`.
/// - Status caches: retain entries younger than `4 * USER_CARD_SUCCESS_TTL`
///   (avoids ghost Loading states for evicted values).
/// - CHANNEL_ID_CACHE / CHANNEL_LOGIN_BY_ID: cleared if either exceeds
///   `CHANNEL_ID_CACHE_MAX`; they re-warm from IRC/Helix on demand.
pub fn sweep_user_card_caches() {
    let eviction_ttl = 4 * USER_CARD_SUCCESS_TTL;
    let now = Instant::now();

    let profile_before = USER_PROFILE_CACHE.len();
    USER_PROFILE_CACHE.retain(|_, entry| now.duration_since(entry.fetched_at) < eviction_ttl);
    let profile_evicted = profile_before.saturating_sub(USER_PROFILE_CACHE.len());

    let follow_before = USER_FOLLOW_CACHE.len();
    USER_FOLLOW_CACHE.retain(|_, entry| now.duration_since(entry.fetched_at) < eviction_ttl);
    let follow_evicted = follow_before.saturating_sub(USER_FOLLOW_CACHE.len());

    let sub_before = USER_SUBSCRIPTION_CACHE.len();
    USER_SUBSCRIPTION_CACHE.retain(|_, entry| now.duration_since(entry.fetched_at) < eviction_ttl);
    let sub_evicted = sub_before.saturating_sub(USER_SUBSCRIPTION_CACHE.len());

    let live_before = USER_LIVE_CACHE.len();
    USER_LIVE_CACHE.retain(|_, entry| {
        let age = now.duration_since(entry.fetched_at);
        age < USER_LIVE_EVICTION_TTL || (entry.value && age < USER_LIVE_EVICTION_TTL * 4)
    });
    let live_evicted = live_before.saturating_sub(USER_LIVE_CACHE.len());

    let live_attempt_before = USER_LIVE_LAST_ATTEMPT.len();
    USER_LIVE_LAST_ATTEMPT
        .retain(|_, attempted_at| now.duration_since(*attempted_at) < USER_LIVE_EVICTION_TTL);
    let live_attempt_evicted = live_attempt_before.saturating_sub(USER_LIVE_LAST_ATTEMPT.len());

    let live_observed_before = USER_LIVE_OBSERVED_USERS.len();
    USER_LIVE_OBSERVED_USERS
        .retain(|_, observed| now.duration_since(observed.observed_at) < USER_LIVE_EVICTION_TTL);
    let live_observed_evicted = live_observed_before.saturating_sub(USER_LIVE_OBSERVED_USERS.len());

    let profile_status_before = USER_PROFILE_STATUS.len();
    USER_PROFILE_STATUS.retain(|_, status| now.duration_since(status.updated_at) < eviction_ttl);
    let profile_status_evicted = profile_status_before.saturating_sub(USER_PROFILE_STATUS.len());

    let follow_status_before = USER_FOLLOW_STATUS.len();
    USER_FOLLOW_STATUS.retain(|_, status| now.duration_since(status.updated_at) < eviction_ttl);
    let follow_status_evicted = follow_status_before.saturating_sub(USER_FOLLOW_STATUS.len());

    let sub_status_before = USER_SUBSCRIPTION_STATUS.len();
    USER_SUBSCRIPTION_STATUS
        .retain(|_, status| now.duration_since(status.updated_at) < eviction_ttl);
    let sub_status_evicted = sub_status_before.saturating_sub(USER_SUBSCRIPTION_STATUS.len());

    let channel_id_evicted = if CHANNEL_ID_CACHE.len() > CHANNEL_ID_CACHE_MAX
        || CHANNEL_LOGIN_BY_ID.len() > CHANNEL_ID_CACHE_MAX
    {
        let before = CHANNEL_ID_CACHE.len() + CHANNEL_LOGIN_BY_ID.len();
        CHANNEL_ID_CACHE.clear();
        CHANNEL_LOGIN_BY_ID.clear();
        before
    } else {
        0
    };

    let total_evicted = profile_evicted
        + follow_evicted
        + sub_evicted
        + profile_status_evicted
        + follow_status_evicted
        + sub_status_evicted
        + live_evicted
        + live_attempt_evicted
        + live_observed_evicted
        + channel_id_evicted;

    if total_evicted > 0 {
        crate::log_cache!(
            debug,
            "sweep_user_card_caches: evicted {} entries \
             (profile={}/{}, follow={}/{}, sub={}/{}, live={}/{}/{}, channel_id={})",
            total_evicted,
            profile_evicted,
            profile_status_evicted,
            follow_evicted,
            follow_status_evicted,
            sub_evicted,
            sub_status_evicted,
            live_evicted,
            live_attempt_evicted,
            live_observed_evicted,
            channel_id_evicted,
        );
    }
}

/// Get cached channel login by channel ID (O(1) lookup, no network call)
pub fn get_cached_channel_login_by_id(channel_id: &str) -> Option<String> {
    CHANNEL_LOGIN_BY_ID.get(channel_id).map(|v| v.clone())
}

/// Get channel login by ID with caching (fast path: cache, slow path: Helix)
pub async fn get_channel_login_by_id_cached(channel_id: &str) -> Option<String> {
    if let Some(login) = get_cached_channel_login_by_id(channel_id) {
        return Some(login);
    }

    match get_channel_login_by_id(channel_id).await {
        Ok(login) => {
            cache_channel_id(&login, channel_id);
            Some(login)
        }
        Err(error) => {
            log_lookup_failure("channel login", channel_id, &error);
            None
        }
    }
}

/// Warn about a failed channel ID/login lookup, at most once per key per
/// [`LOOKUP_FAILURE_LOG_INTERVAL`]. Without the ID a channel gets no emotes
/// or badges, so the cause (status, transport error) must reach the log.
pub(crate) fn log_lookup_failure(kind: &str, key: &str, error: &str) {
    let log_key = format!("{kind}:{}", key.to_ascii_lowercase());
    let now = Instant::now();
    if !should_log_lookup_failure(&mut LOOKUP_FAILURE_LOGGED.lock_or_recover(), log_key, now) {
        return;
    }
    crate::log_net!(warn, "{kind} lookup for {key} failed: {error}");
}

fn should_log_lookup_failure(
    logged: &mut HashMap<String, Instant>,
    key: String,
    now: Instant,
) -> bool {
    if logged
        .get(&key)
        .is_some_and(|last| now.saturating_duration_since(*last) < LOOKUP_FAILURE_LOG_INTERVAL)
    {
        return false;
    }
    if !logged.contains_key(&key) && logged.len() >= LOOKUP_FAILURE_LOG_MAX {
        logged.retain(|_, last| now.saturating_duration_since(*last) < LOOKUP_FAILURE_LOG_INTERVAL);
        if logged.len() >= LOOKUP_FAILURE_LOG_MAX
            && let Some(oldest) = logged
                .iter()
                .min_by_key(|(_, last)| **last)
                .map(|(key, _)| key.clone())
        {
            logged.remove(&oldest);
        }
    }
    logged.insert(key, now);
    true
}

/// Get user ID with caching - checks cache first, then fetches from API
pub async fn get_user_id_cached(username: &str) -> Result<String, String> {
    let key = username.to_lowercase();

    if let Some(id) = CHANNEL_ID_CACHE.get(&key) {
        return Ok(id.clone());
    }

    let id = get_user_id(username).await?;

    cache_channel_id(username, &id);

    Ok(id)
}

/// Response from /users endpoint
#[derive(Deserialize, Debug)]
struct UsersResponse {
    data: Vec<UserData>,
}

#[derive(Deserialize, Debug)]
struct ChannelFollowersResponse {
    #[serde(default)]
    data: Vec<ChannelFollowerData>,
    #[serde(default)]
    total: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct BroadcasterSubscriptionsResponse {
    #[serde(default)]
    data: Vec<BroadcasterSubscriptionData>,
}

#[derive(Deserialize, Debug)]
struct ChannelFollowerData {
    user_id: String,
    user_login: String,
    followed_at: String,
}

#[derive(Deserialize, Debug)]
struct BroadcasterSubscriptionData {
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    plan_name: String,
    #[serde(default)]
    is_gift: bool,
    #[serde(default)]
    gifter_login: String,
    #[serde(default)]
    gifter_name: String,
}

#[derive(Deserialize, Debug)]
struct UserData {
    id: String,
    login: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    profile_image_url: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

impl UserData {
    fn into_profile(self) -> TwitchUserProfile {
        TwitchUserProfile {
            id: self.id,
            login: self.login,
            display_name: self.display_name,
            description: self.description,
            profile_image_url: self.profile_image_url,
            created_at: self.created_at,
        }
    }
}

/// Get user ID from username using the API proxy (no auth required)
pub async fn get_user_id(username: &str) -> Result<String, String> {
    let resp = proxy_lookup("users", username, false)
        .await
        .map_err(|e| format!("Failed to fetch user: {}", e.without_url()))?;

    if !resp.status.is_success() {
        if let Some(auth) = super::auth::capture_authenticated_resource_fence() {
            return get_user_id_helix(username, auth.access_token()).await;
        }
        if API_PROXY.is_none() {
            return Err(API_PROXY_NOT_CONFIGURED.to_string());
        }
        return Err(format!(
            "User not found: {} (status {})",
            username, resp.status
        ));
    }

    let data: UsersResponse = resp
        .json()
        .map_err(|e| format!("Failed to parse user response: {}", e))?;

    let Some(user) = data.data.first() else {
        if !CHANNEL_LOOKUP_MISSES.contains_key(&username.to_lowercase()) {
            record_channel_lookup_miss(username, ChannelLookupState::NotFound);
        }
        return Err(format!("User not found: {}", username));
    };
    Ok(user.id.clone())
}

/// Fetch Twitch profile data for a user and cache it.
pub async fn fetch_user_profile(username: &str) -> Result<TwitchUserProfile, String> {
    let username = username.trim().trim_start_matches('@').to_lowercase();
    if username.is_empty() {
        return Err("User profile lookup requires a username".to_string());
    }

    let resp = proxy_lookup("users", &username, false)
        .await
        .map_err(|e| format!("Failed to fetch user profile: {}", e.without_url()))?;

    if !resp.status.is_success() {
        if let Some(token) = get_token() {
            return fetch_user_profile_helix(&username, &token).await;
        }
        if API_PROXY.is_none() {
            return Err(API_PROXY_NOT_CONFIGURED.to_string());
        }
        return Err(format!(
            "User profile not found: {} (status {})",
            username, resp.status
        ));
    }

    let data: UsersResponse = resp
        .json()
        .map_err(|e| format!("Failed to parse user profile response: {}", e))?;

    let profile = data
        .data
        .into_iter()
        .next()
        .map(UserData::into_profile)
        .ok_or_else(|| format!("User profile not found: {}", username))?;
    cache_user_profile(profile.clone());
    Ok(profile)
}

/// Fetch channel-follow metadata and follower count for a user card.
pub async fn fetch_user_follow(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> Result<TwitchUserFollowInfo, String> {
    let channel_login = channel_login.trim().trim_start_matches('#').to_lowercase();
    let user_login = user_login.trim().trim_start_matches('@').to_lowercase();
    let user_id = user_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    if channel_login.is_empty() || user_login.is_empty() {
        return Err("Follow lookup requires channel and user logins".to_string());
    }

    let Some(auth) = super::auth::capture_authenticated_resource_fence() else {
        return Err("No auth token available for follow lookup".to_string());
    };
    let token = auth.access_token().to_string();

    let broadcaster_id = get_user_id_cached(&channel_login).await?;
    let target_user_id = if let Some(user_id) = user_id {
        user_id
    } else {
        get_user_id_cached(&user_login).await?
    };
    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during follow lookup".to_string());
    }

    let user_follower_count = if super::settings::get_show_follower_count() {
        let count_url = format!(
            "https://api.twitch.tv/helix/channels/followers?broadcaster_id={}&first=1",
            urlencoding::encode(&target_user_id)
        );
        let count_resp = http::send_logged(
            http::LogCategory::Net,
            TWITCH_HTTP_CLIENT.get(&count_url).helix_auth(&token),
        )
        .await
        .map_err(|e| format!("Failed to fetch follower count: {}", e))?;
        if !count_resp.status().is_success() {
            return Err(format!(
                "Follower count lookup failed for {} (status {})",
                user_login,
                count_resp.status()
            ));
        }
        count_resp
            .json::<ChannelFollowersResponse>()
            .await
            .map_err(|e| format!("Failed to parse follower count response: {}", e))?
            .total
    } else {
        None
    };

    let followed = if super::settings::get_show_follow_date()
        && crate::backend::irc::can_moderate_channel(&channel_login)
    {
        let relationship_url = format!(
            "https://api.twitch.tv/helix/channels/followers?broadcaster_id={}&user_id={}&first=1",
            urlencoding::encode(&broadcaster_id),
            urlencoding::encode(&target_user_id)
        );
        let resp = http::send_logged(
            http::LogCategory::Net,
            TWITCH_HTTP_CLIENT.get(&relationship_url).helix_auth(&token),
        )
        .await
        .map_err(|e| format!("Failed to fetch follow metadata: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!(
                "Follow lookup failed for {} in #{} (status {})",
                user_login,
                channel_login,
                resp.status()
            ));
        }
        resp.json::<ChannelFollowersResponse>()
            .await
            .map_err(|e| format!("Failed to parse follow metadata response: {}", e))?
            .data
            .into_iter()
            .next()
    } else {
        None
    };
    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during follow lookup".to_string());
    }
    let followed_at = followed.as_ref().map(|follow| follow.followed_at.clone());
    let resolved_user_login = followed
        .as_ref()
        .map(|follow| follow.user_login.clone())
        .filter(|login| !login.trim().is_empty())
        .unwrap_or(user_login);
    let resolved_user_id = followed
        .as_ref()
        .map(|follow| follow.user_id.clone())
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(target_user_id);

    let info = TwitchUserFollowInfo {
        channel_login,
        user_id: resolved_user_id,
        user_login: resolved_user_login,
        followed_at,
        follower_count: user_follower_count,
    };
    if auth
        .commit_if_current(|| cache_user_follow(info.clone()))
        .is_none()
    {
        return Err("Authenticated Twitch account changed during follow lookup".to_string());
    }
    Ok(info)
}

/// Fetch channel-subscription metadata for a user card.
pub async fn fetch_user_subscription(
    channel_login: &str,
    user_login: &str,
    user_id: Option<&str>,
) -> Result<TwitchUserSubscriptionInfo, String> {
    let channel_login = channel_login.trim().trim_start_matches('#').to_lowercase();
    let user_login = user_login.trim().trim_start_matches('@').to_lowercase();
    let user_id = user_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    if channel_login.is_empty() || user_login.is_empty() {
        return Err("Subscription lookup requires channel and user logins".to_string());
    }

    let Some(auth) = super::auth::capture_authenticated_resource_fence() else {
        return Err("No auth token available for subscription lookup".to_string());
    };
    let token = auth.access_token().to_string();

    let broadcaster_id = get_user_id_cached(&channel_login).await?;
    let target_user_id = if let Some(user_id) = user_id {
        user_id
    } else {
        get_user_id_cached(&user_login).await?
    };
    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during subscription lookup".to_string());
    }

    let self_login = super::settings::get_username_lowercase();
    let is_own_channel = !self_login.is_empty() && self_login == channel_login;
    let is_own_card = !self_login.is_empty() && self_login == user_login;

    let url = if is_own_channel {
        format!(
            "https://api.twitch.tv/helix/subscriptions?broadcaster_id={}&user_id={}&first=1",
            urlencoding::encode(&broadcaster_id),
            urlencoding::encode(&target_user_id)
        )
    } else if is_own_card {
        format!(
            "https://api.twitch.tv/helix/subscriptions/user?broadcaster_id={}&user_id={}",
            urlencoding::encode(&broadcaster_id),
            urlencoding::encode(&target_user_id)
        )
    } else {
        return Err(format!(
            "Subscription details for {} are only viewable for your own channel or your own subscriptions",
            user_login
        ));
    };

    let resp = http::send_logged(
        http::LogCategory::Net,
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(&token),
    )
    .await
    .map_err(|e| format!("Failed to fetch subscription metadata: {}", e))?;

    let response: BroadcasterSubscriptionsResponse = if is_own_card && resp.status().as_u16() == 404
    {
        BroadcasterSubscriptionsResponse { data: Vec::new() }
    } else if !resp.status().is_success() {
        return Err(format!(
            "Subscription lookup failed for {} in #{} (status {})",
            user_login,
            channel_login,
            resp.status()
        ));
    } else {
        resp.json()
            .await
            .map_err(|e| format!("Failed to parse subscription metadata response: {}", e))?
    };
    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during subscription lookup".to_string());
    }
    let subscription = response.data.into_iter().next();
    let resolved_user_login = subscription
        .as_ref()
        .and_then(|sub| non_empty_trimmed_string(&sub.user_login))
        .unwrap_or(user_login);
    let resolved_user_id = subscription
        .as_ref()
        .and_then(|sub| non_empty_trimmed_string(&sub.user_id))
        .unwrap_or(target_user_id);

    let info = TwitchUserSubscriptionInfo {
        channel_login,
        user_id: resolved_user_id,
        user_login: resolved_user_login,
        is_subscribed: subscription.is_some(),
        tier: subscription
            .as_ref()
            .and_then(|sub| non_empty_trimmed_string(&sub.tier)),
        plan_name: subscription
            .as_ref()
            .and_then(|sub| non_empty_trimmed_string(&sub.plan_name)),
        is_gift: subscription.as_ref().is_some_and(|sub| sub.is_gift),
        gifter_login: subscription
            .as_ref()
            .and_then(|sub| non_empty_trimmed_string(&sub.gifter_login)),
        gifter_name: subscription
            .as_ref()
            .and_then(|sub| non_empty_trimmed_string(&sub.gifter_name)),
    };
    if auth
        .commit_if_current(|| cache_user_subscription(info.clone()))
        .is_none()
    {
        return Err("Authenticated Twitch account changed during subscription lookup".to_string());
    }
    Ok(info)
}

fn non_empty_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

async fn fetch_user_profile_helix(
    username: &str,
    token: &str,
) -> Result<TwitchUserProfile, String> {
    let url = format!("https://api.twitch.tv/helix/users?login={}", username);

    let resp = http::send_logged(
        http::LogCategory::Net,
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token),
    )
    .await
    .map_err(|e| format!("Failed to fetch user profile: {}", e.without_url()))?;

    if !resp.status().is_success() {
        return Err(format!(
            "User profile lookup failed: {} (status {})",
            username,
            resp.status()
        ));
    }

    let data: UsersResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse user profile response: {}", e))?;

    let profile = data
        .data
        .into_iter()
        .next()
        .map(UserData::into_profile)
        .ok_or_else(|| format!("User profile not found: {}", username))?;
    cache_user_profile(profile.clone());
    Ok(profile)
}

/// Get user ID from username using Helix API (requires auth token)
pub async fn get_user_id_helix(username: &str, token: &str) -> Result<String, String> {
    let url = format!(
        "https://api.twitch.tv/helix/users?login={}",
        username.to_lowercase()
    );

    let resp = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|e| format!("Failed to fetch user: {}", e.without_url()))?;

    if !resp.status().is_success() {
        return Err(format!(
            "User not found: {} (status {})",
            username,
            resp.status()
        ));
    }

    let data: UsersResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse user response: {}", e))?;

    let Some(user) = data.data.first() else {
        if !CHANNEL_LOOKUP_MISSES.contains_key(&username.to_lowercase()) {
            record_channel_lookup_miss(username, ChannelLookupState::NotFound);
        }
        return Err(format!("User not found: {}", username));
    };
    Ok(user.id.clone())
}

/// Get channel login from user ID using Helix API (requires auth token)
pub async fn get_channel_login_by_id(channel_id: &str) -> Result<String, String> {
    let Some(token) = get_token() else {
        return Err("No auth token available".to_string());
    };

    let url = format!("https://api.twitch.tv/helix/users?id={}", channel_id);

    let resp = send_helix_idempotent(&token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|e| format!("Failed to fetch user by ID: {}", e.without_url()))?;

    if !resp.status().is_success() {
        return Err(format!(
            "User lookup by ID failed: {} (status {})",
            channel_id,
            resp.status()
        ));
    }

    let data: UsersResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse user-by-id response: {}", e))?;

    data.data
        .first()
        .map(|u| u.login.clone())
        .ok_or_else(|| format!("User ID not found: {}", channel_id))
}

/// Response from /streams endpoint
#[derive(Deserialize, Debug)]
struct StreamsResponse {
    data: Vec<StreamData>,
}

/// Response from /channels endpoint. Unlike /streams, this endpoint retains the
/// broadcaster's configured title and category while the channel is offline.
#[derive(Deserialize, Debug)]
struct ChannelsResponse {
    #[serde(default)]
    data: Vec<ChannelData>,
}

#[derive(Deserialize, Debug)]
struct ChannelData {
    #[serde(default)]
    title: String,
    #[serde(default)]
    game_name: String,
}

#[derive(Deserialize, Debug)]
struct StreamData {
    user_login: String,
    game_name: String,
    #[serde(rename = "type")]
    stream_type: String,
    title: String,
    viewer_count: u32,
    started_at: String,
    thumbnail_url: String,
}

/// Stream info result
pub struct StreamInfoResult {
    pub title: String,
    pub game_name: Option<String>,
    pub viewer_count: u32,
    pub started_at: i64,
    pub is_live: bool,
    /// Live stream thumbnail template supplied by Helix. Consumers must replace
    /// its `{width}` and `{height}` tokens before displaying it.
    pub thumbnail_url: Option<String>,
}

/// Get stream info from username using API proxy
pub async fn get_stream_info(username: &str) -> Result<Option<StreamInfoResult>, String> {
    let resp = proxy_lookup("streams", username, false)
        .await
        .map_err(|e| format!("Failed to fetch stream: {}", e))?;

    if !resp.status.is_success() {
        if let Some(token) = get_token() {
            return get_stream_info_helix(username, &token).await;
        }
        if API_PROXY.is_none() {
            return Err(API_PROXY_NOT_CONFIGURED.to_string());
        }
        return Err(format!(
            "Stream lookup failed: {} (status {})",
            username, resp.status
        ));
    }

    let data: StreamsResponse = resp
        .json()
        .map_err(|e| format!("Failed to parse stream response: {}", e))?;

    if let Some(stream) = data.data.first() {
        Ok(Some(StreamInfoResult {
            title: stream.title.clone(),
            game_name: if stream.game_name.is_empty() {
                None
            } else {
                Some(stream.game_name.clone())
            },
            viewer_count: stream.viewer_count,
            started_at: parse_iso8601_to_unix(&stream.started_at),
            is_live: stream.stream_type == "live",
            thumbnail_url: non_empty_trimmed_string(&stream.thumbnail_url),
        }))
    } else if let Some(token) = get_token() {
        get_offline_channel_info_helix(username, &token)
            .await
            .map(Some)
            .or(Ok(None))
    } else {
        Ok(None)
    }
}

/// Get stream info using Helix API (requires auth token)
pub async fn get_stream_info_helix(
    username: &str,
    token: &str,
) -> Result<Option<StreamInfoResult>, String> {
    crate::log_net!(debug, "Helix API call for: {}", username);

    let url = format!(
        "https://api.twitch.tv/helix/streams?user_login={}",
        username.to_lowercase()
    );

    let resp = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|e| format!("Failed to fetch stream: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Stream lookup failed: {} (status {})",
            username,
            resp.status()
        ));
    }

    let data: StreamsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse stream response: {}", e))?;

    if let Some(stream) = data.data.first() {
        Ok(Some(StreamInfoResult {
            title: stream.title.clone(),
            game_name: if stream.game_name.is_empty() {
                None
            } else {
                Some(stream.game_name.clone())
            },
            viewer_count: stream.viewer_count,
            started_at: parse_iso8601_to_unix(&stream.started_at),
            is_live: stream.stream_type == "live",
            thumbnail_url: non_empty_trimmed_string(&stream.thumbnail_url),
        }))
    } else {
        get_offline_channel_info_helix(username, token)
            .await
            .map(Some)
            .or(Ok(None))
    }
}

/// Fetch the title/category configured on an offline Twitch channel.
///
/// A successful empty `/streams` response is authoritative for offline state;
/// failure here only removes optional metadata and must not turn that state into
/// an availability error.
async fn get_offline_channel_info_helix(
    username: &str,
    token: &str,
) -> Result<StreamInfoResult, String> {
    let broadcaster_id = get_user_id_cached(username).await?;
    let url = format!(
        "https://api.twitch.tv/helix/channels?broadcaster_id={}",
        broadcaster_id
    );
    let resp = send_helix_idempotent(token, |token| {
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(token)
    })
    .await
    .map_err(|e| format!("Failed to fetch offline channel metadata: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Offline channel metadata lookup failed: {} (status {})",
            username,
            resp.status()
        ));
    }

    let data: ChannelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse offline channel metadata: {}", e))?;
    let channel = data
        .data
        .into_iter()
        .next()
        .ok_or_else(|| format!("Offline channel metadata not found: {}", username))?;

    Ok(StreamInfoResult {
        title: channel.title,
        game_name: if channel.game_name.trim().is_empty() {
            None
        } else {
            Some(channel.game_name)
        },
        viewer_count: 0,
        started_at: 0,
        is_live: false,
        thumbnail_url: None,
    })
}

/// Public metadata for a Twitch VOD. Twitch has no documented Echo proxy route
/// for VODs, so this result is fetched from Helix only when an authenticated
/// account resource is available.
#[derive(Clone, Debug)]
pub struct TwitchVideoMetadata {
    pub title: String,
    pub description: Option<String>,
    pub author_login: String,
    pub author_name: String,
    pub thumbnail_url: Option<String>,
    /// Helix's compact duration string, for example `1h2m3s`.
    pub duration: Option<String>,
}

#[derive(Deserialize, Debug)]
struct VideosResponse {
    #[serde(default)]
    data: Vec<VideoData>,
}

#[derive(Deserialize, Debug)]
struct VideoData {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    user_name: String,
    #[serde(default)]
    thumbnail_url: String,
    #[serde(default)]
    duration: String,
}

/// Fetch VOD metadata through Helix when the active Twitch account permits it.
///
/// `Ok(None)` is the normal anonymous-mode/no-token result and lets callers
/// fall back to Twitch oEmbed, Open Graph, or a static provider card. The
/// request is fenced so account changes cannot produce a stale authenticated
/// result for the caller.
pub async fn get_video_metadata(video_id: &str) -> Result<Option<TwitchVideoMetadata>, String> {
    let video_id = video_id.trim();
    if video_id.is_empty() || !video_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Twitch VOD lookup requires a numeric video ID".to_string());
    }

    let Some(auth) = super::auth::capture_authenticated_resource_fence() else {
        return Ok(None);
    };
    let url = format!("https://api.twitch.tv/helix/videos?id={video_id}");
    let response = http::send_logged(
        http::LogCategory::Net,
        TWITCH_HTTP_CLIENT.get(&url).helix_auth(auth.access_token()),
    )
    .await
    .map_err(|error| format!("Failed to fetch Twitch VOD metadata: {error}"))?;

    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during VOD lookup".to_string());
    }
    if !response.status().is_success() {
        return Err(format!(
            "Twitch VOD lookup failed for {video_id}: status {}",
            response.status()
        ));
    }

    let response: VideosResponse = response
        .json()
        .await
        .map_err(|error| format!("Failed to parse Twitch VOD metadata: {error}"))?;
    if !auth.is_current() {
        return Err("Authenticated Twitch account changed during VOD lookup".to_string());
    }

    Ok(response
        .data
        .into_iter()
        .next()
        .map(|video| TwitchVideoMetadata {
            title: video.title,
            description: non_empty_trimmed_string(&video.description),
            author_login: video.user_login,
            author_name: video.user_name,
            thumbnail_url: non_empty_trimmed_string(&video.thumbnail_url),
            duration: non_empty_trimmed_string(&video.duration),
        }))
}

/// Parse ISO 8601 timestamp to Unix timestamp
fn parse_iso8601_to_unix(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}
