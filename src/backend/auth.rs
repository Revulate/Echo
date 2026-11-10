#![allow(clippy::await_holding_lock)]

use super::badges::TWITCH_BADGE_CACHE;
use super::constants::CLIENT_ID;
use super::emotes::EMOTE_CACHE;
use super::http;
use super::lock_utils::{MutexExt, RwLockExt};
use super::settings::{
    Account, SETTINGS, flush_settings_saves, get_refresh_token, get_token_expires_at,
    save_settings, upsert_account,
};
pub use echo_core::oauth_token::normalize_oauth_token;
use echo_core::oauth_token::sanitized_oauth_error;
use serde::{Deserialize, Deserializer};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

/// Full OAuth scopes for all features
const OAUTH_SCOPES: &[&str] = &[
    "chat:read",
    "chat:edit",
    "user:read:chat",
    "user:write:chat",
    "whispers:read",
    "whispers:edit",
    "user:read:whispers",
    super::constants::TWITCH_WHISPER_MANAGE_SCOPE,
    "channel:moderate",
    "moderation:read",
    "moderator:manage:automod",
    "moderator:manage:banned_users",
    "moderator:manage:chat_messages",
    "moderator:manage:chat_settings",
    "moderator:read:chatters",
    "moderator:read:followers",
    "moderator:read:shield_mode",
    "moderator:read:suspicious_users",
    "moderator:read:warnings",
    "moderator:manage:shield_mode",
    "moderator:manage:suspicious_users",
    "moderator:manage:warnings",
    "channel:manage:broadcast",
    "channel:manage:moderators",
    "channel:manage:raids",
    "channel:edit:commercial",
    "channel:read:vips",
    "channel:manage:vips",
    "channel:read:redemptions",
    "channel:manage:redemptions",
    "channel:read:polls",
    "channel:manage:polls",
    "channel:read:predictions",
    "channel:manage:predictions",
    "channel:read:hype_train",
    "channel:read:subscriptions",
    "user:read:blocked_users",
    "user:manage:blocked_users",
    "user:read:follows",
    "user:read:subscriptions",
    "user:read:emotes",
    "clips:edit",
    "bits:read",
];

pub(crate) fn oauth_scope_requested(scope: &str) -> bool {
    OAUTH_SCOPES.contains(&scope)
}

/// Minimum scopes needed before Echo should present an account as usable.
const CORE_OAUTH_SCOPES: &[&str] = &[
    "chat:read",
    "chat:edit",
    "user:read:chat",
    "user:write:chat",
    "user:read:emotes",
];

/// Cooldown tracking for token refresh
static LAST_REFRESH_FAILURE: AtomicI64 = AtomicI64::new(0);
const REFRESH_COOLDOWN_SECS: i64 = 60;

/// Flag to cancel login polling
static LOGIN_CANCELLED: AtomicBool = AtomicBool::new(false);
/// Monotonic generation counter for login attempts.
/// Each new login invalidates older poll tasks.
static LOGIN_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Marks the login generation whose credentials crossed the atomic commit
/// boundary. Cancellation after this point is too late to revoke a completed
/// OAuth exchange and must not strand the published account without its
/// user-scoped resource refresh.
static LOGIN_COMMITTED_GENERATION: AtomicU64 = AtomicU64::new(0);
static CURRENT_LOGIN_ATTEMPT: LazyLock<Mutex<Option<LoginAttempt>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Clone)]
pub struct LoginAttempt {
    generation: u64,
    state: Arc<RwLock<LoginAttemptState>>,
}

#[derive(Clone)]
pub enum LoginAttemptState {
    Loading,
    DeviceCode {
        verification_uri: String,
        user_code: String,
    },
    Succeeded {
        display_name: String,
    },
    Error(String),
    Cancelled,
    Superseded,
}

impl LoginAttempt {
    pub fn state(&self) -> LoginAttemptState {
        self.state.read_or_recover().clone()
    }

    pub fn cancel(&self) {
        cancel_login_generation(Some(self.generation));
    }
}

// Callers hold AUTH_CREDENTIAL_COMMIT_LOCK so state and attempt ownership agree.
fn update_login_attempt_state(generation: u64, state: LoginAttemptState) {
    if let Some(attempt) = CURRENT_LOGIN_ATTEMPT.lock_or_recover().as_ref()
        && attempt.generation == generation
    {
        *attempt.state.write_or_recover() = state;
    }
}

/// Serializes the synchronous publication boundary for every active-account or
/// credential mutation. Network work never holds this lock. In-flight token
/// refreshes snapshot `AUTH_CREDENTIAL_GENERATION` before their request and may
/// publish only while that generation is still current under this lock.
static AUTH_CREDENTIAL_COMMIT_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static AUTH_CREDENTIAL_GENERATION: AtomicU64 = AtomicU64::new(0);
static ACTIVE_AUTH_USER_ID: LazyLock<RwLock<Option<(u64, String)>>> =
    LazyLock::new(|| RwLock::new(None));

/// Flag to track if token refresh monitor is running
static REFRESH_MONITOR_RUNNING: AtomicBool = AtomicBool::new(false);
/// Serializes refresh-token use so startup, monitor, and auth-failure recovery
/// cannot race each other and invalidate a rotated refresh token.
static TOKEN_REFRESH_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
/// Orders asynchronous cache-token writes after the synchronous credential
/// commit. Generation-aware callers recheck after acquiring this lock, so an
/// older distributor can never become the final cache writer after a newer
/// account commit.
static TOKEN_DISTRIBUTION_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
static AUTH_INITIALIZE_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Flag to track if auth initialization has completed (token validated/refreshed)
static AUTH_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Time before expiry to trigger proactive refresh (10 minutes)
const PROACTIVE_REFRESH_BUFFER_SECS: i64 = 600;

#[derive(Clone, Default, PartialEq)]
pub struct AuthState {
    pub authenticated: bool,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub profile_image_url: Option<String>,
    pub device_code: Option<String>,
    pub verification_uri: Option<String>,
    pub user_code: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    pub needs_reauth: bool,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("authenticated", &self.authenticated)
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("profile_image_url", &self.profile_image_url)
            .field(
                "device_code",
                &self.device_code.as_ref().map(|_| crate::log::Secret(())),
            )
            .field("verification_uri", &self.verification_uri)
            .field("user_code", &self.user_code)
            .field("error", &self.error)
            .field("loading", &self.loading)
            .field("needs_reauth", &self.needs_reauth)
            .finish()
    }
}

pub static AUTH_STATE: LazyLock<RwLock<AuthState>> =
    LazyLock::new(|| RwLock::new(AuthState::default()));

/// Shared HTTP client for auth requests with proper timeouts
/// Prevents indefinite hangs on network issues
static AUTH_CLIENT: LazyLock<http::ClientCache> =
    LazyLock::new(|| http::ClientCache::new(build_auth_client));

fn build_auth_client() -> reqwest::Client {
    match http::client_builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create Auth HTTP client with timeouts: {e}");
            http::timed_client(Duration::from_secs(30))
        }
    }
}

static AUTH_EVENT_TX: OnceLock<tokio::sync::broadcast::Sender<AuthEvent>> = OnceLock::new();

/// Auth events for push-based status updates
#[derive(Clone, Debug)]
pub enum AuthEvent {
    /// User logged in successfully
    LoggedIn,
    /// User logged out
    LoggedOut,
    /// Switched to a different account
    AccountSwitched,
    /// Login flow started (device code polling)
    LoginStarted,
    /// Login flow cancelled
    LoginCancelled,
}

/// Initialize the auth event broadcast channel
fn init_auth_broadcast() {
    if AUTH_EVENT_TX.get().is_none() {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let _ = AUTH_EVENT_TX.set(tx);
    }
}

/// Broadcast an auth event to all subscribers
fn broadcast_auth_event(event: AuthEvent) {
    if let Some(tx) = AUTH_EVENT_TX.get() {
        let _ = tx.send(event);
    }
}

/// Cancel the current login flow by clearing device code state
pub fn cancel_login() {
    cancel_login_generation(None);
}

fn cancel_login_generation(expected_generation: Option<u64>) {
    let cancelled = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        let generation = LOGIN_GENERATION.load(Ordering::SeqCst);
        if expected_generation.is_some_and(|expected| expected != generation)
            || LOGIN_COMMITTED_GENERATION.load(Ordering::SeqCst) == generation
        {
            false
        } else {
            LOGIN_CANCELLED.store(true, Ordering::SeqCst);
            LOGIN_GENERATION.fetch_add(1, Ordering::SeqCst);
            update_login_attempt_state(generation, LoginAttemptState::Cancelled);
            let mut state = AUTH_STATE.write_or_recover();
            state.device_code = None;
            state.user_code = None;
            state.verification_uri = None;
            state.error = None;
            state.loading = false;
            true
        }
    };
    if !cancelled {
        crate::log_auth!(debug, "Ignoring stale or committed login cancellation");
        return;
    }

    crate::log_auth!(info, "Login cancelled");
    broadcast_auth_event(AuthEvent::LoginCancelled);
}

#[derive(Deserialize, Clone)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

impl std::fmt::Debug for DeviceCodeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCodeResponse")
            .field("device_code", &crate::log::Secret(()))
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TokenResponse {
    Success {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
        #[serde(default, deserialize_with = "deserialize_oauth_scopes")]
        scope: Vec<String>,
    },
    #[allow(dead_code, reason = "untagged serde discriminator for the error shape")]
    Error { error: String, message: String },
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success {
                refresh_token,
                expires_in,
                scope,
                ..
            } => f
                .debug_struct("Success")
                .field("access_token", &crate::log::Secret(()))
                .field(
                    "refresh_token",
                    &refresh_token.as_ref().map(|_| crate::log::Secret(())),
                )
                .field("expires_in", expires_in)
                .field("scope", scope)
                .finish(),
            Self::Error { error, message } => f
                .debug_struct("Error")
                .field("error", error)
                .field("message", message)
                .finish(),
        }
    }
}

#[derive(Deserialize, Debug)]
struct HelixUsersResponse {
    #[serde(default)]
    data: Vec<HelixUser>,
}

#[derive(Deserialize, Debug)]
struct HelixUser {
    #[serde(default)]
    id: String,
    #[serde(default)]
    login: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    profile_image_url: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ValidateTokenResponse {
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Identity and granted scopes returned by Twitch for a valid user access token.
///
/// EventSub uses this snapshot to select only subscription families the current
/// account can actually authorize, instead of probing every open channel with
/// privileged requests and learning through clustered 403 responses.
#[derive(Clone, Debug)]
pub struct TokenAuthorization {
    user_id: Option<String>,
    scopes: HashSet<String>,
}

impl TokenAuthorization {
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    pub fn has_any_scope(&self, required: &[&str]) -> bool {
        required.is_empty()
            || required
                .iter()
                .any(|scope| self.scopes.contains(&scope.to_ascii_lowercase()))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OAuthScopeField {
    List(Vec<String>),
    String(String),
}

fn deserialize_oauth_scopes<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(scope_field) = Option::<OAuthScopeField>::deserialize(deserializer)? else {
        return Ok(Vec::new());
    };

    Ok(match scope_field {
        OAuthScopeField::List(scopes) => scopes,
        OAuthScopeField::String(scopes) => scopes
            .split_whitespace()
            .filter(|scope| !scope.trim().is_empty())
            .map(ToString::to_string)
            .collect(),
    })
}

fn has_scope(granted_scopes: &[String], required: &str) -> bool {
    granted_scopes
        .iter()
        .any(|scope| scope.eq_ignore_ascii_case(required))
}

fn missing_core_oauth_scopes(granted_scopes: &[String]) -> Vec<&'static str> {
    CORE_OAUTH_SCOPES
        .iter()
        .copied()
        .filter(|required| !has_scope(granted_scopes, required))
        .collect()
}

fn missing_scope_error(missing_scopes: &[&str]) -> String {
    format!(
        "Login did not grant required Twitch scopes: {}. Log in again to grant chat and emote access.",
        missing_scopes.join(", ")
    )
}

fn require_core_oauth_scopes(granted_scopes: &[String]) -> Result<(), String> {
    let missing_scopes = missing_core_oauth_scopes(granted_scopes);
    if missing_scopes.is_empty() {
        Ok(())
    } else {
        Err(missing_scope_error(&missing_scopes))
    }
}

fn set_login_error(error: String) {
    crate::log_auth!(warn, "{}", error);
    let needs_reauth = super::settings::get_active_account().is_some()
        || AUTH_STATE.read_or_recover().needs_reauth;
    let mut state = AUTH_STATE.write_or_recover();
    state.authenticated = false;
    state.device_code = None;
    state.user_code = None;
    state.verification_uri = None;
    state.error = Some(error);
    state.loading = false;
    state.needs_reauth = needs_reauth;
    drop(state);
    super::eventsub::notify_auth_readiness_changed();
}

fn set_login_error_if_active(generation: u64, error: String) {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if is_login_attempt_active(generation) {
        update_login_attempt_state(generation, LoginAttemptState::Error(error.clone()));
        set_login_error(error);
    } else {
        crate::log_auth!(debug, "Discarding stale login error");
    }
}

pub fn begin_login() -> LoginAttempt {
    init_auth_broadcast();

    let attempt = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        LOGIN_CANCELLED.store(false, Ordering::SeqCst);
        let generation = LOGIN_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let attempt = LoginAttempt {
            generation,
            state: Arc::new(RwLock::new(LoginAttemptState::Loading)),
        };
        if let Some(previous) = CURRENT_LOGIN_ATTEMPT
            .lock_or_recover()
            .replace(attempt.clone())
        {
            let mut state = previous.state.write_or_recover();
            if matches!(
                *state,
                LoginAttemptState::Loading | LoginAttemptState::DeviceCode { .. }
            ) {
                *state = LoginAttemptState::Superseded;
            }
        }
        {
            let mut state = AUTH_STATE.write_or_recover();
            state.loading = true;
            state.error = None;
            state.device_code = None;
            state.user_code = None;
            state.verification_uri = None;
        }
        broadcast_auth_event(AuthEvent::LoginStarted);
        attempt
    };
    crate::log_auth!(
        debug,
        generation = attempt.generation,
        "Login attempt started"
    );
    attempt
}

pub async fn start_login() {
    let attempt = begin_login();
    start_login_attempt(attempt).await;
}

pub async fn start_login_attempt(attempt: LoginAttempt) {
    let generation = attempt.generation;
    if !is_login_attempt_active(generation) {
        return;
    }

    let client = AUTH_CLIENT.client();
    let scopes_str = OAUTH_SCOPES.join(" ");
    let params = [("client_id", CLIENT_ID), ("scopes", &scopes_str)];

    let resp = client
        .post("https://id.twitch.tv/oauth2/device")
        .form(&params)
        .send()
        .await;

    if !is_login_attempt_active(generation) {
        crate::log_auth!(debug, "Discarding stale device-code response");
        return;
    }

    match resp {
        Ok(response) => {
            if response.status().is_success() {
                let parsed = response.json::<DeviceCodeResponse>().await;
                let poll = {
                    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                    if !is_login_attempt_active(generation) {
                        crate::log_auth!(debug, "Discarding stale device-code payload");
                        return;
                    }
                    match parsed {
                        Ok(data) => {
                            let mut state = AUTH_STATE.write_or_recover();
                            state.device_code = Some(data.device_code.clone());
                            state.user_code = Some(data.user_code.clone());
                            state.verification_uri = Some(data.verification_uri.clone());
                            state.loading = false;
                            update_login_attempt_state(
                                generation,
                                LoginAttemptState::DeviceCode {
                                    verification_uri: data.verification_uri,
                                    user_code: data.user_code,
                                },
                            );
                            Some((data.device_code, data.interval, data.expires_in))
                        }
                        Err(error) => {
                            crate::log_auth!(
                                warn,
                                "Login: device-code response could not be parsed: {error}"
                            );
                            let mut state = AUTH_STATE.write_or_recover();
                            state.error = Some("Failed to parse device code response".to_string());
                            state.loading = false;
                            update_login_attempt_state(
                                generation,
                                LoginAttemptState::Error(
                                    "Failed to parse device code response".to_string(),
                                ),
                            );
                            None
                        }
                    }
                };
                if let Some((device_code, interval, expires_in)) = poll {
                    crate::backend::bridge::get_runtime().spawn(async move {
                        poll_for_token(device_code, interval, expires_in, generation).await;
                    });
                }
            } else {
                let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                if is_login_attempt_active(generation) {
                    crate::log_auth!(
                        warn,
                        "Login: device-code request failed with HTTP {}",
                        response.status()
                    );
                    let mut state = AUTH_STATE.write_or_recover();
                    state.error = Some(format!("Twitch API error: {}", response.status()));
                    state.loading = false;
                    update_login_attempt_state(
                        generation,
                        LoginAttemptState::Error(format!(
                            "Twitch API error: {}",
                            response.status()
                        )),
                    );
                }
            }
        }
        Err(e) => {
            let e = e.without_url();
            let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
            if is_login_attempt_active(generation) {
                crate::log_auth!(warn, "Login: device-code request failed: {e}");
                let mut state = AUTH_STATE.write_or_recover();
                state.error = Some(format!("Network error: {}", e));
                state.loading = false;
                update_login_attempt_state(
                    generation,
                    LoginAttemptState::Error(format!("Network error: {}", e)),
                );
            }
        }
    }
}

#[inline]
fn is_login_generation_active(generation: u64) -> bool {
    LOGIN_GENERATION.load(Ordering::SeqCst) == generation
}

#[inline]
fn is_login_attempt_active(generation: u64) -> bool {
    !LOGIN_CANCELLED.load(Ordering::SeqCst) && is_login_generation_active(generation)
}

#[inline]
pub(crate) fn credential_generation() -> u64 {
    AUTH_CREDENTIAL_GENERATION.load(Ordering::Acquire)
}

#[inline]
fn advance_credential_generation() -> u64 {
    AUTH_CREDENTIAL_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

fn advance_credential_generation_preserving_user_id() -> u64 {
    let generation = advance_credential_generation();
    if let Some((identity_generation, _)) = ACTIVE_AUTH_USER_ID.write_or_recover().as_mut() {
        *identity_generation = generation;
    }
    generation
}

fn revoke_account_scoped_runtime_commit_held() {
    *ACTIVE_AUTH_USER_ID.write_or_recover() = None;
    EMOTE_CACHE.revoke_user_emotes_residency();
    super::followed_live::clear_commit_held();
}

fn publish_authenticated_user_id_if_current(token: &str, user_id: Option<&str>) {
    let Some(user_id) = user_id.map(str::trim).filter(|user_id| !user_id.is_empty()) else {
        return;
    };
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if super::irc::is_anonymous_mode() || !active_access_token_matches(token) {
        return;
    }
    *ACTIVE_AUTH_USER_ID.write_or_recover() = Some((credential_generation(), user_id.to_string()));
}

/// Lock-free check for the tokenizer's per-message hot path; unlike
/// `active_authenticated_user_id` it never waits on a credential commit.
pub(crate) fn is_active_authenticated_user_id(user_id: &str) -> bool {
    !super::irc::is_anonymous_mode()
        && ACTIVE_AUTH_USER_ID
            .read_or_recover()
            .as_ref()
            .is_some_and(|(generation, id)| *generation == credential_generation() && id == user_id)
}

pub(crate) fn active_authenticated_user_id() -> Option<String> {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if super::irc::is_anonymous_mode() {
        return None;
    }
    ACTIVE_AUTH_USER_ID
        .read_or_recover()
        .as_ref()
        .filter(|(generation, _)| *generation == credential_generation())
        .map(|(_, user_id)| user_id.clone())
}

/// Account/token identity captured by one IRC socket. A NOTICE from an older
/// socket must not be allowed to invalidate credentials published afterward.
pub(crate) struct IrcCredentialFence {
    generation: u64,
    normalized_access_token: String,
}

pub(crate) fn capture_irc_credential_fence(access_token: &str) -> Option<IrcCredentialFence> {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    active_access_token_matches(access_token).then(|| IrcCredentialFence {
        generation: credential_generation(),
        normalized_access_token: normalize_oauth_token(access_token).to_string(),
    })
}

impl IrcCredentialFence {
    #[inline]
    pub(crate) fn generation_is_current(&self) -> bool {
        credential_generation() == self.generation
    }

    pub(crate) fn is_current(&self) -> bool {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        self.is_current_commit_held()
    }

    fn is_current_commit_held(&self) -> bool {
        credential_generation() == self.generation
            && active_access_token_matches(&self.normalized_access_token)
    }
}

/// Generation/token identity for account-scoped cache work. Callers may do
/// network or disk work without the auth mutex, then publish only through
/// `commit_if_current`, which shares the account-transition boundary.
#[derive(Clone)]
pub(crate) struct AuthenticatedResourceFence {
    generation: u64,
    normalized_access_token: String,
}

/// One generation-consistent account identity for authenticated network work.
/// The access token and user ID are captured under the same transition lock;
/// callers must still fence every post-await action and local publication.
#[derive(Clone)]
pub(crate) struct AuthenticatedUserResource {
    pub(crate) access_token: String,
    pub(crate) user_id: String,
    fence: AuthenticatedResourceFence,
}

pub(crate) fn capture_authenticated_resource_fence() -> Option<AuthenticatedResourceFence> {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if super::irc::is_anonymous_mode() {
        return None;
    }
    let token = super::settings::get_token()?;
    let normalized_access_token = normalize_oauth_token(&token).trim().to_string();
    if normalized_access_token.is_empty() {
        return None;
    }
    Some(AuthenticatedResourceFence {
        generation: credential_generation(),
        normalized_access_token,
    })
}

pub(crate) fn capture_authenticated_user_resource() -> Option<AuthenticatedUserResource> {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if super::irc::is_anonymous_mode() {
        return None;
    }

    let token = super::settings::get_token()?;
    let normalized_access_token = normalize_oauth_token(&token).trim().to_string();
    if normalized_access_token.is_empty() || !active_access_token_matches(&normalized_access_token)
    {
        return None;
    }

    let generation = credential_generation();
    let user_id = ACTIVE_AUTH_USER_ID
        .read_or_recover()
        .as_ref()
        .filter(|(identity_generation, user_id)| {
            *identity_generation == generation && !user_id.trim().is_empty()
        })
        .map(|(_, user_id)| user_id.clone())?;

    Some(AuthenticatedUserResource {
        access_token: normalized_access_token.clone(),
        user_id,
        fence: AuthenticatedResourceFence {
            generation,
            normalized_access_token,
        },
    })
}

impl AuthenticatedResourceFence {
    pub(crate) fn access_token(&self) -> &str {
        &self.normalized_access_token
    }

    pub(crate) fn is_current(&self) -> bool {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        credential_generation() == self.generation
            && !super::irc::is_anonymous_mode()
            && active_access_token_matches(&self.normalized_access_token)
    }

    pub(crate) fn matches_token(&self, token: &str) -> bool {
        self.normalized_access_token == normalize_oauth_token(token).trim()
    }

    pub(crate) fn commit_if_current<T>(&self, commit: impl FnOnce() -> T) -> Option<T> {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if credential_generation() != self.generation
            || super::irc::is_anonymous_mode()
            || !active_access_token_matches(&self.normalized_access_token)
        {
            return None;
        }
        Some(commit())
    }
}

impl AuthenticatedUserResource {
    pub(crate) fn generation(&self) -> u64 {
        self.fence.generation
    }

    pub(crate) fn is_current(&self) -> bool {
        self.fence.commit_if_current(|| ()).is_some()
    }

    pub(crate) fn commit_if_current<T>(&self, commit: impl FnOnce() -> T) -> Option<T> {
        self.fence.commit_if_current(commit)
    }
}

async fn poll_for_token(device_code: String, interval: u64, expires_in: u64, generation: u64) {
    let client = AUTH_CLIENT.client();
    let start_time = std::time::Instant::now();
    let mut current_interval = Duration::from_secs(interval);
    let mut network_errors = 0;
    const MAX_NETWORK_ERRORS: u32 = 3;
    let scopes_str = OAUTH_SCOPES.join(" ");

    loop {
        if LOGIN_CANCELLED.load(Ordering::SeqCst) || !is_login_generation_active(generation) {
            crate::log_auth!(info, "Polling cancelled by user");
            break;
        }

        if start_time.elapsed().as_secs() > expires_in {
            set_login_error_if_active(generation, "Device code expired".to_string());
            break;
        }

        sleep(current_interval).await;

        if LOGIN_CANCELLED.load(Ordering::SeqCst) || !is_login_generation_active(generation) {
            crate::log_auth!(info, "Polling cancelled by user");
            break;
        }

        let params = [
            ("client_id", CLIENT_ID),
            ("scopes", scopes_str.as_str()),
            ("device_code", device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];

        let resp = client
            .post("https://id.twitch.tv/oauth2/token")
            .form(&params)
            .send()
            .await;

        if !is_login_attempt_active(generation) {
            crate::log_auth!(debug, "Discarding stale device-token response");
            break;
        }

        match resp {
            Ok(response) => {
                network_errors = 0;

                if response.status().is_success() {
                    let token_response = response.json().await;
                    if !is_login_attempt_active(generation) {
                        crate::log_auth!(debug, "Discarding stale device-token payload");
                        break;
                    }
                    if let Ok(TokenResponse::Success {
                        access_token,
                        refresh_token,
                        expires_in: token_expires_in,
                        scope,
                        ..
                    }) = token_response
                    {
                        let scope_validation = if scope.is_empty() {
                            let validation = fetch_token_validation(&access_token).await;
                            if !is_login_attempt_active(generation) {
                                crate::log_auth!(debug, "Discarding stale login scope validation");
                                break;
                            }
                            match validation {
                                Ok(Some(validation)) => {
                                    require_core_oauth_scopes(&validation.scopes)
                                }
                                Ok(None) => Err(
                                    "Login token failed Twitch validation after device-code approval"
                                        .to_string(),
                                ),
                                Err(error) => Err(error),
                            }
                        } else {
                            require_core_oauth_scopes(&scope)
                        };

                        if let Err(error) = scope_validation {
                            set_login_error_if_active(generation, error);
                            break;
                        }

                        if !is_login_attempt_active(generation) {
                            crate::log_auth!(
                                debug,
                                "Discarding stale login success from superseded generation"
                            );
                            break;
                        }

                        let user_result = fetch_authenticated_user(&access_token).await;
                        if !is_login_attempt_active(generation) {
                            crate::log_auth!(debug, "Discarding stale login identity response");
                            break;
                        }
                        let user = match user_result {
                            Ok(user) => user,
                            Err(error) => {
                                set_login_error_if_active(generation, error);
                                break;
                            }
                        };

                        if !is_login_attempt_active(generation) {
                            crate::log_auth!(
                                debug,
                                "Discarding stale login identity from superseded generation"
                            );
                            break;
                        }

                        let token_expires_at = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64 + token_expires_in as i64)
                            .ok();

                        let account = Account {
                            username: user.login.clone(),
                            display_name: user.display_name.clone(),
                            profile_image_url: user.profile_image_url.clone(),
                            token: access_token.clone(),
                            refresh_token: refresh_token.clone(),
                            client_id: CLIENT_ID.to_string(),
                            is_active: true,
                            token_expires_at,
                        };

                        let (blocked_users_auth_epoch, committed_credential_generation) = {
                            let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                            if !is_login_attempt_active(generation) {
                                crate::log_auth!(
                                    debug,
                                    "Discarding stale login before publishing credentials"
                                );
                                break;
                            }

                            let blocked_users_auth_epoch =
                                super::blocked_users::begin_auth_transition(false);
                            revoke_account_scoped_runtime_commit_held();
                            {
                                let mut settings = SETTINGS.lock_or_recover();
                                for acc in &mut settings.accounts {
                                    acc.is_active = false;
                                }
                                settings.token = Some(access_token.clone());
                                settings.refresh_token = refresh_token;
                                settings.username = Some(user.login.clone());
                                settings.token_expires_at = token_expires_at;
                            }
                            upsert_account(account);
                            super::settings::set_anonymous_mode(false);
                            super::irc::reset_anonymous_mode();

                            {
                                let mut state = AUTH_STATE.write_or_recover();
                                state.authenticated = true;
                                state.username = Some(user.login.clone());
                                state.display_name = user.display_name.clone();
                                state.profile_image_url = user.profile_image_url.clone();
                                state.device_code = None;
                                state.user_code = None;
                                state.verification_uri = None;
                                state.error = None;
                                state.loading = false;
                                state.needs_reauth = false;
                            }
                            LOGIN_COMMITTED_GENERATION.store(generation, Ordering::SeqCst);
                            update_login_attempt_state(
                                generation,
                                LoginAttemptState::Succeeded {
                                    display_name: user
                                        .display_name
                                        .clone()
                                        .unwrap_or_else(|| user.login.clone()),
                                },
                            );
                            let committed_credential_generation = advance_credential_generation();
                            *ACTIVE_AUTH_USER_ID.write_or_recover() =
                                Some((committed_credential_generation, user.id.clone()));
                            crate::log_auth!(info, "Authenticated as {}", user.login);
                            broadcast_auth_event(AuthEvent::LoggedIn);
                            (blocked_users_auth_epoch, committed_credential_generation)
                        };

                        flush_settings_saves(Duration::from_secs(2));
                        clear_refresh_cooldown();
                        super::eventsub::reset_for_auth_change("login completed");

                        if !distribute_token_for_generation(
                            &access_token,
                            committed_credential_generation,
                        )
                        .await
                        {
                            crate::log_auth!(debug, "Discarding stale post-login resource refresh");
                            break;
                        }
                        spawn_user_scoped_resource_refresh(
                            access_token,
                            Some(user.id),
                            blocked_users_auth_epoch,
                            true,
                        );
                        break;
                    }
                    // A 2xx without a usable token will not improve by polling again.
                    let reason = match &token_response {
                        Ok(TokenResponse::Error { error, .. }) => {
                            format!("error {}", sanitized_oauth_error(error.clone()))
                        }
                        Err(error) => format!(
                            "unparseable body ({})",
                            crate::backend::http::error_kind(error)
                        ),
                        Ok(TokenResponse::Success { .. }) => "unexpected response".to_string(),
                    };
                    // set_login_error logs the message at warn.
                    set_login_error_if_active(
                        generation,
                        format!("Twitch returned an unusable login response: {reason}"),
                    );
                    break;
                } else {
                    let status = response.status();
                    let msg =
                        sanitized_oauth_error(crate::backend::http::error_body(response).await);
                    if !is_login_attempt_active(generation) {
                        crate::log_auth!(debug, "Discarding stale device-token error");
                        break;
                    }
                    match msg.as_str() {
                        "authorization_pending" => continue,
                        "slow_down" => {
                            current_interval += Duration::from_secs(5);
                            continue;
                        }
                        "expired_token" => {
                            set_login_error_if_active(
                                generation,
                                "Device code expired".to_string(),
                            );
                            break;
                        }
                        "access_denied" => {
                            set_login_error_if_active(
                                generation,
                                "Authorization denied".to_string(),
                            );
                            break;
                        }
                        _ => {
                            set_login_error_if_active(
                                generation,
                                format!("Auth error ({}): {}", status, msg),
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                network_errors += 1;
                crate::log_auth!(
                    warn,
                    "Network error during polling ({}/{}): {}",
                    network_errors,
                    MAX_NETWORK_ERRORS,
                    e
                );

                if network_errors >= MAX_NETWORK_ERRORS {
                    set_login_error_if_active(
                        generation,
                        format!("Network error after {} retries: {}", MAX_NETWORK_ERRORS, e),
                    );
                    break;
                }

                current_interval = current_interval
                    .saturating_mul(2)
                    .min(Duration::from_secs(30));
            }
        }
    }
}

/// Check if token refresh is on cooldown (to prevent spam after failure)
pub fn is_refresh_on_cooldown() -> bool {
    let last_failure = LAST_REFRESH_FAILURE.load(Ordering::Relaxed);
    if last_failure == 0 {
        return false;
    }
    let now = crate::utils::unix_now_secs() as i64;
    (now - last_failure) < REFRESH_COOLDOWN_SECS
}

fn record_refresh_failure() {
    let now = crate::utils::unix_now_secs() as i64;
    LAST_REFRESH_FAILURE.store(now, Ordering::Relaxed);
}

pub fn clear_refresh_cooldown() {
    LAST_REFRESH_FAILURE.store(0, Ordering::Relaxed);
}

fn active_account_missing_runtime_token(active_account: Option<&Account>) -> bool {
    active_account.is_some_and(|account| account.token.trim().is_empty())
}

fn sync_auth_state_from_active_account(authenticated: bool) {
    let active_account = super::settings::get_active_account();
    let username = active_account
        .as_ref()
        .map(|account| account.username.clone())
        .or_else(super::settings::get_username);
    let missing_runtime_token = active_account_missing_runtime_token(active_account.as_ref());
    let authenticated = authenticated && !missing_runtime_token;

    let mut state = AUTH_STATE.write_or_recover();
    state.authenticated = authenticated;
    state.username = username;
    state.display_name = active_account
        .as_ref()
        .and_then(|account| account.display_name.clone());
    state.profile_image_url = active_account
        .as_ref()
        .and_then(|account| account.profile_image_url.clone());
    if authenticated {
        state.needs_reauth = false;
    } else if missing_runtime_token {
        state.needs_reauth = true;
    }
    drop(state);
    super::eventsub::notify_auth_readiness_changed();
}

/// Mark the active session as needing a new sign-in, telling the user once per
/// transition instead of leaving them to discover it on the next failed send.
fn flag_needs_reauth() {
    let newly_flagged = {
        let mut state = AUTH_STATE.write_or_recover();
        !std::mem::replace(&mut state.needs_reauth, true)
    };
    if newly_flagged {
        super::push_ui_action(super::UiAction::ShowWarningToast {
            message: "Twitch sign-in expired".to_string(),
            detail: Some(
                "Echo could not renew the Twitch session. Log in again from Settings > General > Account to keep sending messages."
                    .to_string(),
            ),
        });
    }
    super::eventsub::notify_auth_readiness_changed();
}

/// Mark the active runtime session as unusable after Twitch rejects the IRC login.
///
/// The saved account identity is left intact so the UI can offer re-authentication
/// for the same account instead of silently switching to a blank anonymous state.
pub(crate) fn mark_runtime_auth_failed_if_current(
    fence: &IrcCredentialFence,
    reason: &str,
) -> bool {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    if !fence.is_current_commit_held() {
        return false;
    }
    crate::log_auth!(warn, "Runtime auth failed: {}", reason);
    revoke_account_scoped_runtime_commit_held();
    sync_auth_state_from_active_account(false);
    let mut state = AUTH_STATE.write_or_recover();
    state.needs_reauth = true;
    state.error = Some(reason.to_string());
    drop(state);
    super::blocked_users::reset_for_auth_change();
    super::irc::set_runtime_anonymous_mode(true);
    super::eventsub::reset_for_auth_change("runtime authentication failed");
    true
}

async fn fetch_authenticated_user(access_token: &str) -> Result<HelixUser, String> {
    let normalized_access_token = normalize_oauth_token(access_token);
    let response = AUTH_CLIENT
        .get("https://api.twitch.tv/helix/users")
        .header("Client-Id", CLIENT_ID)
        .header(
            "Authorization",
            format!("Bearer {}", normalized_access_token),
        )
        .send()
        .await
        .map_err(|error| format!("Failed to fetch Twitch account identity: {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = sanitized_oauth_error(crate::backend::http::error_body(response).await);
        return Err(format!(
            "Failed to fetch Twitch account identity ({status}): {body}"
        ));
    }

    let users = response
        .json::<HelixUsersResponse>()
        .await
        .map_err(|error| format!("Failed to parse Twitch account identity: {error}"))?;

    users
        .data
        .into_iter()
        .find(|user| !user.id.trim().is_empty() && !user.login.trim().is_empty())
        .ok_or_else(|| "Twitch account identity response did not include a user".to_string())
}

async fn refresh_user_scoped_resources(
    token: &str,
    user_id: Option<String>,
    blocked_users_auth_epoch: u64,
    refresh_emotes: bool,
) {
    if !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
        || super::settings::get_anonymous_mode()
        || super::irc::is_anonymous_mode()
    {
        return;
    }

    let user_id = match user_id {
        Some(user_id) if !user_id.trim().is_empty() => Some(user_id),
        _ => match validate_token_with_user_id(token).await {
            Ok(user_id) => user_id,
            Err(e) => {
                crate::log_emotes!(warn, "Failed to resolve logged-in user for emotes: {}", e);
                None
            }
        },
    };

    let Some(user_id) = user_id else {
        super::blocked_users::fail_auth_transition(
            blocked_users_auth_epoch,
            "The active Twitch token has no usable account identity.",
        );
        crate::log_emotes!(warn, "Skipping user-scoped refresh: token has no user id");
        return;
    };
    if !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
        || super::settings::get_anonymous_mode()
        || super::irc::is_anonymous_mode()
    {
        return;
    }
    super::followed_live::warm_after_sign_in();

    let session = match super::blocked_users::activate_auth_session(
        blocked_users_auth_epoch,
        token,
        &user_id,
    ) {
        Ok(session) => session,
        Err(error) => {
            if super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
                && !super::settings::get_anonymous_mode()
                && !super::irc::is_anonymous_mode()
            {
                super::blocked_users::fail_auth_transition(blocked_users_auth_epoch, error.clone());
                crate::log_auth!(warn, "Blocked-user session activation failed: {}", error);
            }
            return;
        }
    };
    let Some(blocked_ticket) = super::blocked_users::begin_refresh(&session) else {
        return;
    };

    let blocked_refresh = super::blocked_users::refresh(blocked_ticket);
    let blocked_result = if refresh_emotes {
        crate::log_emotes!(
            info,
            "Refreshing user-scoped emotes for authenticated user_id: {}",
            user_id
        );
        let emote_refresh = async {
            match EMOTE_CACHE.load_user_emotes(&user_id, token).await {
                Ok(()) => {
                    super::emotes::rebuild_user_emote_index().await;
                    crate::log_emotes!(info, "User-scoped emotes refreshed successfully");
                }
                Err(e) => {
                    crate::log_emotes!(warn, "Failed to refresh user-scoped emotes: {}", e);
                }
            }
        };
        let ((), blocked_result) = tokio::join!(emote_refresh, blocked_refresh);
        blocked_result
    } else {
        blocked_refresh.await
    };

    match blocked_result {
        Ok(super::blocked_users::RefreshOutcome::Applied(count)) => {
            crate::log_auth!(info, "Refreshed {} Twitch blocked user(s)", count);
        }
        Ok(super::blocked_users::RefreshOutcome::Superseded) => {
            crate::log_auth!(debug, "Discarded superseded Twitch blocked-user refresh");
        }
        Err(error) => {
            crate::log_auth!(warn, "Failed to refresh Twitch blocked users: {}", error);
        }
    }
}

fn spawn_user_scoped_resource_refresh(
    token: String,
    user_id: Option<String>,
    blocked_users_auth_epoch: u64,
    refresh_emotes: bool,
) {
    crate::backend::bridge::get_runtime().spawn(async move {
        refresh_user_scoped_resources(&token, user_id, blocked_users_auth_epoch, refresh_emotes)
            .await;
    });
}

/// Commit an anonymous/authenticated runtime-mode transition in the same total
/// order as login, logout, account switches, and token replacement.
///
/// The returned credential generation fences the asynchronous cache writer.
/// Leaving anonymous mode deliberately keeps the IRC runtime anonymous until
/// token distribution succeeds, so send readiness cannot briefly target the
/// old anonymous socket.
pub(crate) fn commit_runtime_mode_transition(anonymous: bool) -> (u64, u64) {
    let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
    revoke_account_scoped_runtime_commit_held();

    if anonymous {
        {
            let visibility_commit = super::blocked_users::visibility_commit_guard();
            super::settings::set_anonymous_mode(true);
            super::irc::set_runtime_anonymous_mode(true);
            drop(visibility_commit);
        }
        let blocked_users_auth_epoch = super::blocked_users::reset_for_auth_change();
        let committed_credential_generation = advance_credential_generation();
        super::eventsub::reset_for_auth_change("anonymous mode enabled");
        (blocked_users_auth_epoch, committed_credential_generation)
    } else {
        let blocked_users_auth_epoch = super::blocked_users::begin_auth_transition(false);
        {
            let visibility_commit = super::blocked_users::visibility_commit_guard();
            if !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch) {
                return (blocked_users_auth_epoch, advance_credential_generation());
            }
            super::settings::set_anonymous_mode(false);
            drop(visibility_commit);
        }
        (blocked_users_auth_epoch, advance_credential_generation())
    }
}

/// Restore token-backed runtime state after the user leaves anonymous mode.
pub async fn restore_authenticated_runtime_after_anonymous(
    blocked_users_auth_epoch: u64,
    expected_credential_generation: u64,
) -> Result<(), String> {
    let token = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if credential_generation() != expected_credential_generation
            || !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
        {
            return Err("Authenticated restore was superseded by a newer mode change".to_string());
        }
        super::settings::get_token().filter(|token| !token.trim().is_empty())
    };
    let token = match token {
        Some(token) => token,
        None => {
            if !clear_distributed_token_for_generation(expected_credential_generation).await {
                return Err(
                    "Authenticated restore was superseded by a newer account change".to_string(),
                );
            }
            let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
            if credential_generation() != expected_credential_generation
                || !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
            {
                return Err(
                    "Authenticated restore was superseded by a newer account change".to_string(),
                );
            }
            sync_auth_state_from_active_account(false);
            AUTH_STATE.write_or_recover().needs_reauth = true;
            super::blocked_users::finish_auth_transition_without_session(blocked_users_auth_epoch);
            super::eventsub::reset_for_auth_change("anonymous mode restore lacked a token");
            return Err(
                "No active Twitch token is available. Re-authenticate the account.".to_string(),
            );
        }
    };

    if !distribute_token_for_generation(&token, expected_credential_generation).await {
        return Err("Authenticated restore was superseded by an account change".to_string());
    }
    {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if credential_generation() != expected_credential_generation
            || !active_access_token_matches(&token)
            || !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
        {
            return Err("Authenticated restore was superseded by an account change".to_string());
        }
        {
            let visibility_commit = super::blocked_users::visibility_commit_guard();
            if credential_generation() != expected_credential_generation
                || !super::blocked_users::is_auth_epoch_current(blocked_users_auth_epoch)
            {
                return Err(
                    "Authenticated restore was superseded by a newer account change".to_string(),
                );
            }
            super::irc::reset_anonymous_mode();
            drop(visibility_commit);
        }
        sync_auth_state_from_active_account(true);
        super::eventsub::reset_for_auth_change("anonymous mode disabled");
        spawn_user_scoped_resource_refresh(token, None, blocked_users_auth_epoch, true);
        broadcast_auth_event(AuthEvent::AccountSwitched);
    }

    Ok(())
}

/// Validate an OAuth token against Twitch's validation endpoint
pub async fn validate_token(token: &str) -> Result<bool, String> {
    let Some(data) = fetch_token_validation(token).await? else {
        return Ok(false);
    };

    match require_core_oauth_scopes(&data.scopes) {
        Ok(()) => {
            publish_authenticated_user_id_if_current(token, data.user_id.as_deref());
            Ok(true)
        }
        Err(error) => {
            crate::log_auth!(warn, "{}", error);
            Ok(false)
        }
    }
}

/// Validate token and return user_id if valid
pub async fn validate_token_with_user_id(token: &str) -> Result<Option<String>, String> {
    let Some(data) = fetch_token_validation(token).await? else {
        return Ok(None);
    };

    if !has_scope(&data.scopes, "user:read:emotes") {
        return Ok(None);
    }

    publish_authenticated_user_id_if_current(token, data.user_id.as_deref());
    Ok(data.user_id)
}

/// Validate a token and return the authorization capabilities EventSub needs.
pub async fn validate_token_authorization(
    token: &str,
) -> Result<Option<TokenAuthorization>, String> {
    let Some(data) = fetch_token_validation(token).await? else {
        return Ok(None);
    };

    Ok(Some(TokenAuthorization {
        user_id: data.user_id,
        scopes: data
            .scopes
            .into_iter()
            .map(|scope| scope.to_ascii_lowercase())
            .collect(),
    }))
}

async fn fetch_token_validation(token: &str) -> Result<Option<ValidateTokenResponse>, String> {
    let resp = AUTH_CLIENT
        .get("https://id.twitch.tv/oauth2/validate")
        .header(
            "Authorization",
            format!("OAuth {}", normalize_oauth_token(token)),
        )
        .send()
        .await
        .map_err(|e| {
            crate::log_auth!(
                warn,
                "Token validation (/oauth2/validate) failed: {}",
                crate::backend::http::error_kind(&e)
            );
            format!("Token validation request failed: {}", e.without_url())
        })?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    if !status.is_success() {
        // Only a 401 means the token is invalid; 429/5xx are outages, and treating
        // them as "invalid" would force a refresh and possibly a needless re-login.
        // This is the one warning per validation outage; callers log at debug.
        crate::log_auth!(
            warn,
            "Token validation (/oauth2/validate) returned HTTP {}",
            status
        );
        return Err(format!("Token validation returned HTTP {}", status));
    }

    resp.json()
        .await
        .map(Some)
        .map_err(|e| format!("Failed to parse validate response: {}", e))
}

/// Refresh the access token using a refresh token
pub async fn refresh_access_token(
    refresh_token: &str,
) -> Result<(String, Option<String>, u64), String> {
    let client = AUTH_CLIENT.client();
    let params = [
        ("client_id", CLIENT_ID),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.trim()),
    ];

    let resp = client
        .post("https://id.twitch.tv/oauth2/token")
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Failed to refresh token: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = sanitized_oauth_error(crate::backend::http::error_body(resp).await);

        if body.contains("Invalid refresh token") || body.contains("invalid_grant") {
            return Err(format!("Permanent auth failure: {}", body));
        }

        return Err(format!("Token refresh failed ({}): {}", status, body));
    }

    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
        #[serde(default, deserialize_with = "deserialize_oauth_scopes")]
        scope: Vec<String>,
    }

    let data: RefreshResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse refresh response: {}", e))?;

    if data.scope.is_empty() {
        let Some(validation) = fetch_token_validation(&data.access_token).await? else {
            return Err("Refreshed token failed Twitch validation".to_string());
        };
        require_core_oauth_scopes(&validation.scopes)?;
    } else {
        require_core_oauth_scopes(&data.scope)?;
    }

    Ok((data.access_token, data.refresh_token, data.expires_in))
}

/// Refresh tokens and update settings/state. Returns the new access token.
pub async fn refresh_and_save_tokens() -> Result<String, String> {
    refresh_and_save_tokens_expected(None, None).await
}

/// Refresh only while `rejected_access_token` is still active. Concurrent 401s
/// for one token queue on the refresh lock; the first rotates it and the rest
/// receive the rotated token without another refresh or credential bump.
async fn refresh_and_save_tokens_replacing(rejected_access_token: &str) -> Result<String, String> {
    refresh_and_save_tokens_expected(None, Some(rejected_access_token)).await
}

/// Refresh only if the rejected IRC socket still owns the active credential.
/// The check occurs after the async refresh mutex is acquired and inside the
/// credential commit boundary, closing the socket-check/refresh TOCTOU gap.
pub(crate) async fn refresh_and_save_tokens_for_irc(
    fence: &IrcCredentialFence,
) -> Result<String, String> {
    refresh_and_save_tokens_expected(
        Some((fence.generation, fence.normalized_access_token.clone())),
        None,
    )
    .await
}

async fn refresh_and_save_tokens_expected(
    expected_irc_credential: Option<(u64, String)>,
    rejected_access_token: Option<&str>,
) -> Result<String, String> {
    let _refresh_guard = TOKEN_REFRESH_LOCK.lock().await;

    if let Some(rejected) = rejected_access_token
        && !active_access_token_matches(rejected)
    {
        return super::settings::get_token()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| "No active token after a concurrent refresh".to_string());
    }

    if is_refresh_on_cooldown() {
        return Err("Token refresh on cooldown".to_string());
    }

    let (source_credential_generation, refresh_token) = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if expected_irc_credential
            .as_ref()
            .is_some_and(|(generation, token)| {
                credential_generation() != *generation || !active_access_token_matches(token)
            })
        {
            return Err("IRC credential was superseded before token refresh".to_string());
        }
        let refresh_token = match get_refresh_token() {
            Some(rt) if !rt.is_empty() => rt,
            _ => {
                record_refresh_failure();
                flag_needs_reauth();
                return Err("No refresh token available".to_string());
            }
        };
        (credential_generation(), refresh_token)
    };

    match refresh_access_token(&refresh_token).await {
        Ok((new_access_token, new_refresh_token, expires_in)) => {
            let expires_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64 + expires_in as i64)
                .ok();

            let (blocked_users_auth_epoch, committed_credential_generation) = {
                let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                if credential_generation() != source_credential_generation
                    || !active_refresh_token_matches(&refresh_token)
                {
                    crate::log_auth!(
                        info,
                        "Discarding token refresh response for superseded credentials"
                    );
                    return Err("Token refresh superseded by an account change".to_string());
                }

                let blocked_users_auth_epoch = super::blocked_users::begin_auth_transition(true);
                {
                    let mut settings = SETTINGS.lock_or_recover();
                    settings.token = Some(new_access_token.clone());

                    match &new_refresh_token {
                        Some(new_rt) => {
                            settings.refresh_token = Some(new_rt.clone());
                        }
                        None => {
                            crate::log_auth!(
                                warn,
                                "Twitch didn't return new refresh token - keeping existing one"
                            );
                        }
                    }
                    settings.token_expires_at = expires_at;

                    for acc in &mut settings.accounts {
                        if acc.is_active {
                            acc.token = new_access_token.clone();
                            if let Some(ref new_rt) = new_refresh_token {
                                acc.refresh_token = Some(new_rt.clone());
                            }
                            acc.token_expires_at = expires_at;
                            break;
                        }
                    }
                }
                (
                    blocked_users_auth_epoch,
                    advance_credential_generation_preserving_user_id(),
                )
            };
            save_settings();
            crate::state::ui_invalidation::request(
                crate::state::ui_invalidation::UiInvalidation::TopBarState,
            );
            flush_settings_saves(Duration::from_secs(2));
            clear_refresh_cooldown();

            if !distribute_token_for_generation(&new_access_token, committed_credential_generation)
                .await
            {
                crate::log_auth!(debug, "Discarding stale post-refresh account publication");
                return Err("Token refresh superseded by an account change".to_string());
            }

            {
                let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                if credential_generation() != committed_credential_generation
                    || !active_access_token_matches(&new_access_token)
                {
                    crate::log_auth!(debug, "Discarding stale completed token refresh");
                    return Err("Token refresh superseded by an account change".to_string());
                }
                super::irc::reset_anonymous_mode();
                sync_auth_state_from_active_account(true);
            }
            spawn_user_scoped_resource_refresh(
                new_access_token.clone(),
                None,
                blocked_users_auth_epoch,
                true,
            );

            crate::log_auth!(info, "Token refreshed successfully");
            Ok(new_access_token)
        }
        Err(e) => {
            let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
            if credential_generation() != source_credential_generation
                || !active_refresh_token_matches(&refresh_token)
            {
                crate::log_auth!(
                    info,
                    "Discarding token refresh failure for superseded credentials"
                );
                return Err("Token refresh superseded by an account change".to_string());
            }
            record_refresh_failure();

            if e.contains("Permanent auth failure") || e.contains("invalid_grant") {
                flag_needs_reauth();
                super::blocked_users::reset_for_auth_change();
                super::eventsub::reset_for_auth_change("token refresh permanently failed");
            }

            Err(e)
        }
    }
}

/// Distribute token to all caches that need it
pub async fn distribute_token(token: &str) {
    let expected_generation = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if super::irc::is_anonymous_mode() {
            crate::log_auth!(debug, "Skipped token distribution while anonymous");
            return;
        }
        credential_generation()
    };
    if !distribute_token_for_generation(token, expected_generation).await {
        crate::log_auth!(debug, "Skipped stale token distribution");
    }
}

/// Distribute a token only while it still belongs to the credential generation
/// that committed it. The async distribution lock guarantees that a newer
/// account's writer follows any older in-progress writer and becomes final.
async fn distribute_token_for_generation(token: &str, expected_generation: u64) -> bool {
    let _distribution_guard = TOKEN_DISTRIBUTION_LOCK.lock().await;
    if credential_generation() != expected_generation || !active_access_token_matches(token) {
        return false;
    }

    TWITCH_BADGE_CACHE.set_token(token.to_string()).await;
    if credential_generation() != expected_generation || !active_access_token_matches(token) {
        return false;
    }

    EMOTE_CACHE.set_token(token.to_string()).await;
    let current =
        credential_generation() == expected_generation && active_access_token_matches(token);
    if current {
        crate::log_auth!(debug, "Token distributed to caches");
    }
    current
}

fn active_access_token_matches(token: &str) -> bool {
    super::settings::get_token()
        .map(|active| normalize_oauth_token(&active) == normalize_oauth_token(token))
        .unwrap_or(false)
}

fn active_refresh_token_matches(refresh_token: &str) -> bool {
    get_refresh_token().is_some_and(|active| active.trim() == refresh_token.trim())
}

pub(crate) async fn clear_distributed_token_for_generation(expected_generation: u64) -> bool {
    let _distribution_guard = TOKEN_DISTRIBUTION_LOCK.lock().await;
    if credential_generation() != expected_generation {
        return false;
    }

    TWITCH_BADGE_CACHE.clear_token().await;
    if credential_generation() != expected_generation {
        return false;
    }
    EMOTE_CACHE.clear_token().await;
    if credential_generation() != expected_generation {
        return false;
    }
    EMOTE_CACHE.clear_user_emotes().await;
    credential_generation() == expected_generation
}

/// Check if token is expiring soon (within PROACTIVE_REFRESH_BUFFER_SECS)
pub fn is_token_expiring_soon() -> bool {
    if let Some(expires_at) = get_token_expires_at() {
        let now = crate::utils::unix_now_secs() as i64;
        let time_remaining = expires_at - now;
        time_remaining < PROACTIVE_REFRESH_BUFFER_SECS && time_remaining > 0
    } else {
        false
    }
}

/// Check if token has already expired
pub fn is_token_expired() -> bool {
    if let Some(expires_at) = get_token_expires_at() {
        let now = crate::utils::unix_now_secs() as i64;
        now >= expires_at
    } else {
        false
    }
}

/// Start the background token refresh monitor.
/// Checks every 5 minutes and refreshes tokens 10 minutes before expiry.
/// Only one monitor runs at a time (idempotent).
/// This is an async function that runs forever - should be spawned in a background task.
pub async fn start_token_refresh_monitor_async() {
    if REFRESH_MONITOR_RUNNING.swap(true, Ordering::SeqCst) {
        crate::log_auth!(debug, "Token refresh monitor already running");
        return;
    }

    crate::log_auth!(info, "Token refresh monitor started");

    let mut last_validated = std::time::Instant::now();
    loop {
        sleep(Duration::from_secs(300)).await;

        let Some(token) = super::settings::get_token().filter(|token| !token.trim().is_empty())
        else {
            continue;
        };

        if is_token_expiring_soon() || is_token_expired() {
            crate::log_auth!(info, "Token expiring soon, attempting proactive refresh...");
            last_validated = std::time::Instant::now();

            match refresh_and_save_tokens().await {
                Ok(_) => {
                    crate::log_auth!(info, "Proactive token refresh succeeded");
                }
                Err(e) => {
                    crate::log_auth!(error, "Proactive token refresh failed: {}", e);
                }
            }
        } else if last_validated.elapsed() >= HOURLY_TOKEN_VALIDATION_INTERVAL
            && !super::irc::is_anonymous_mode()
        {
            last_validated = std::time::Instant::now();
            match fetch_token_validation(&token).await {
                Ok(Some(_)) => crate::log_auth!(debug, "Hourly token validation passed"),
                Ok(None) if active_access_token_matches(&token) => {
                    crate::log_auth!(
                        warn,
                        "Hourly token validation was rejected; refreshing the session"
                    );
                    if let Err(error) = refresh_and_save_tokens().await {
                        crate::log_auth!(warn, "Refresh after failed validation failed: {}", error);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    crate::log_auth!(debug, "Hourly token validation request failed: {}", error)
                }
            }
        }
    }
}

/// Twitch requires apps to validate long-lived sessions at least hourly.
const HOURLY_TOKEN_VALIDATION_INTERVAL: Duration = Duration::from_secs(3600);

/// Recover from a Helix 401 for `rejected_token`, returning the token to retry with.
///
/// Missing-scope rejections and tokens `/oauth2/validate` still accepts return `None`,
/// because a refresh cannot grant scopes or fix a permission error. When another
/// task already rotated the token, the current one is returned without refreshing.
pub(crate) async fn recover_helix_unauthorized(
    rejected_token: &str,
    detail: &str,
) -> Option<String> {
    if detail.to_ascii_lowercase().contains("scope") || super::irc::is_anonymous_mode() {
        return None;
    }
    if !active_access_token_matches(rejected_token) {
        return super::settings::get_token().filter(|token| !token.trim().is_empty());
    }
    if !matches!(fetch_token_validation(rejected_token).await, Ok(None)) {
        return None;
    }
    crate::log_auth!(
        info,
        "Helix rejected an expired token; refreshing once before retrying"
    );
    match refresh_and_save_tokens_replacing(rejected_token).await {
        Ok(token) => Some(token),
        Err(error) => {
            crate::log_auth!(warn, "Refresh after Helix 401 failed: {}", error);
            None
        }
    }
}

/// Publish the refreshed token's stable owner before the emote lane attempts
/// owner-matched disk hydration.
async fn publish_refreshed_startup_identity(token: &str) {
    match validate_token_with_user_id(token).await {
        Ok(Some(_)) => {
            crate::log_auth!(debug, "Published refreshed startup account identity");
        }
        Ok(None) => {
            crate::log_auth!(warn, "Refreshed startup token has no user-emote identity");
        }
        Err(error) => {
            crate::log_auth!(
                warn,
                "Could not resolve refreshed startup account identity: {}",
                error
            );
        }
    }
}

/// Run the startup validation branch for `token`: sign in on success, refresh
/// on rejection. `Err` means Twitch could not answer (transport, 429, 5xx);
/// the caller retries later instead of treating the token as invalid.
async fn apply_startup_validation(
    token: &str,
    blocked_users_startup_epoch: u64,
) -> Result<(), String> {
    if validate_token(token).await? {
        if !super::blocked_users::is_auth_epoch_current(blocked_users_startup_epoch) {
            crate::log_auth!(debug, "Startup validation superseded by an account change");
            return Ok(());
        }
        crate::log_auth!(info, "Token validated successfully");
        sync_auth_state_from_active_account(true);

        let mut token_rotation_owns_resource_refresh = false;
        if is_token_expiring_soon() {
            crate::log_auth!(info, "Token expiring soon, refreshing proactively...");
            match refresh_and_save_tokens().await {
                Ok(_) => token_rotation_owns_resource_refresh = true,
                Err(e) => {
                    crate::log_auth!(warn, "Proactive refresh on startup failed: {}", e);
                }
            }
        }
        if !token_rotation_owns_resource_refresh
            && super::blocked_users::is_auth_epoch_current(blocked_users_startup_epoch)
        {
            spawn_user_scoped_resource_refresh(
                token.to_string(),
                None,
                blocked_users_startup_epoch,
                false,
            );
        }
    } else {
        if !active_access_token_matches(token) {
            crate::log_auth!(debug, "Startup validation superseded by an account change");
            return Ok(());
        }
        crate::log_auth!(warn, "Token invalid, attempting refresh...");
        match refresh_and_save_tokens_replacing(token).await {
            Ok(refreshed_token) => {
                crate::log_auth!(info, "Token refresh after validation failure succeeded");
                publish_refreshed_startup_identity(&refreshed_token).await;
                sync_auth_state_from_active_account(true);
            }
            Err(e) if !active_access_token_matches(token) => {
                crate::log_auth!(
                    debug,
                    "Startup refresh failed after an account change: {}",
                    e
                );
            }
            Err(e) => {
                crate::log_auth!(
                    error,
                    "Token refresh after validation failure failed: {}",
                    e
                );
                AUTH_STATE.write_or_recover().needs_reauth = true;
                sync_auth_state_from_active_account(false);
                super::blocked_users::reset_for_auth_change();
            }
        }
    }
    Ok(())
}

/// Delay before startup-validation retry `attempt` (0-based): 30 s, 2 min,
/// 5 min, then hourly.
fn startup_validation_retry_delay(attempt: usize) -> Duration {
    const DELAYS: [Duration; 3] = [
        Duration::from_secs(30),
        Duration::from_secs(120),
        Duration::from_secs(300),
    ];
    DELAYS
        .get(attempt)
        .copied()
        .unwrap_or(HOURLY_TOKEN_VALIDATION_INTERVAL)
}

/// Keep retrying startup validation after an outage until Twitch answers,
/// the stored token changes, or the session goes anonymous.
fn spawn_startup_validation_retry(token: String, blocked_users_epoch: u64) {
    crate::backend::bridge::get_runtime().spawn(async move {
        for attempt in 0.. {
            let delay = startup_validation_retry_delay(attempt);
            crate::log_auth!(
                info,
                "Retrying startup token validation in {}s",
                delay.as_secs()
            );
            sleep(delay).await;
            if super::irc::is_anonymous_mode() || !active_access_token_matches(&token) {
                crate::log_auth!(debug, "Startup validation retry superseded");
                return;
            }
            match apply_startup_validation(&token, blocked_users_epoch).await {
                Ok(()) => return,
                Err(error) => {
                    crate::log_auth!(debug, "Startup token validation retry failed: {error}");
                }
            }
        }
    });
}

/// Initialize auth state on app startup.
/// Validates existing token and refreshes if needed.
/// Sets AUTH_INITIALIZED flag when complete so other systems know auth is ready.
pub async fn initialize_auth() {
    use super::settings::get_token;

    let _initialize_guard = AUTH_INITIALIZE_LOCK.lock().await;
    if AUTH_INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    let blocked_users_startup_epoch = super::blocked_users::begin_auth_transition(false);

    let token = match get_token() {
        Some(t) if !t.is_empty() => t,
        _ => {
            crate::log_auth!(info, "No token found on startup");
            sync_auth_state_from_active_account(false);
            super::blocked_users::finish_auth_transition_without_session(
                blocked_users_startup_epoch,
            );
            AUTH_INITIALIZED.store(true, Ordering::Release);
            return;
        }
    };

    crate::log_auth!(info, "Validating token on startup...");

    if is_token_expired() {
        crate::log_auth!(warn, "Token has expired, attempting refresh...");
        match refresh_and_save_tokens().await {
            Ok(refreshed_token) => {
                crate::log_auth!(info, "Token refresh on startup succeeded");
                publish_refreshed_startup_identity(&refreshed_token).await;
                sync_auth_state_from_active_account(true);
            }
            Err(e) => {
                crate::log_auth!(error, "Token refresh on startup failed: {}", e);
                AUTH_STATE.write_or_recover().needs_reauth = true;
                sync_auth_state_from_active_account(false);
                super::blocked_users::reset_for_auth_change();
            }
        }
        AUTH_INITIALIZED.store(true, Ordering::Release);
        return;
    }

    if let Err(e) = apply_startup_validation(&token, blocked_users_startup_epoch).await {
        crate::log_auth!(debug, "Startup token validation failed: {}", e);
        super::blocked_users::fail_auth_transition(
            blocked_users_startup_epoch,
            format!("Twitch blocked-user identity validation failed: {e}"),
        );
        // Refreshing does not need /oauth2/validate, so a token known to expire
        // soon is rotated now rather than after the outage.
        let refreshed = if is_token_expiring_soon() {
            match refresh_and_save_tokens().await {
                Ok(refreshed_token) => {
                    crate::log_auth!(info, "Proactive refresh after failed validation succeeded");
                    publish_refreshed_startup_identity(&refreshed_token).await;
                    sync_auth_state_from_active_account(true);
                    true
                }
                Err(error) => {
                    crate::log_auth!(
                        warn,
                        "Proactive refresh after failed validation failed: {}",
                        error
                    );
                    false
                }
            }
        } else {
            false
        };
        if !refreshed {
            spawn_startup_validation_retry(token, blocked_users_startup_epoch);
        }
    }

    AUTH_INITIALIZED.store(true, Ordering::Release);
    crate::log_auth!(debug, "Auth initialization complete");
}

/// Get whether re-authentication is needed
pub fn get_needs_reauth() -> bool {
    AUTH_STATE.read_or_recover().needs_reauth
}

/// Check if auth initialization has completed.
/// Other systems should wait for this before making authenticated API calls.
pub fn is_auth_initialized() -> bool {
    AUTH_INITIALIZED.load(Ordering::Acquire)
}

/// Wait until startup token validation or refresh has finished.
pub async fn wait_for_auth_initialization() {
    while !is_auth_initialized() {
        sleep(Duration::from_millis(50)).await;
    }
}

/// Trigger re-authentication flow (for expired tokens)
/// Starts a new login flow while preserving the current re-auth-required state
/// until Twitch grants a replacement token.
pub fn trigger_reauth() {
    crate::log_auth!(info, "Re-authentication triggered by user");

    crate::backend::bridge::get_runtime().spawn(async {
        start_login().await;
    });
}

/// Switch to a different account
pub async fn switch_account(username: &str) -> Result<(), String> {
    use super::settings::set_active_account;

    let (blocked_users_auth_epoch, acc, switched_credential_generation) = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if !super::settings::get_accounts()
            .iter()
            .any(|account| account.username == username)
        {
            return Err(format!("Account '{}' not found", username));
        }
        let blocked_users_auth_epoch = super::blocked_users::begin_auth_transition(false);
        revoke_account_scoped_runtime_commit_held();
        let acc = set_active_account(username)?;
        let has_runtime_token = !acc.token.trim().is_empty();
        {
            let mut state = AUTH_STATE.write_or_recover();
            state.authenticated = has_runtime_token;
            state.username = Some(acc.username.clone());
            state.display_name = acc.display_name.clone();
            state.profile_image_url = acc.profile_image_url.clone();
            state.error = (!has_runtime_token).then(|| {
                format!(
                    "Saved account @{username} has no runtime token. Re-authenticate this account."
                )
            });
            state.needs_reauth = !has_runtime_token;
        }
        if has_runtime_token {
            super::settings::set_anonymous_mode(false);
            super::irc::reset_anonymous_mode();
        }
        (
            blocked_users_auth_epoch,
            acc,
            advance_credential_generation(),
        )
    };

    if acc.token.trim().is_empty() {
        if !clear_distributed_token_for_generation(switched_credential_generation).await {
            return Err("Account switch superseded by a newer account change".to_string());
        }
        {
            let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
            if credential_generation() != switched_credential_generation {
                return Err("Account switch superseded by a newer account change".to_string());
            }
            super::blocked_users::finish_auth_transition_without_session(blocked_users_auth_epoch);
            super::eventsub::reset_for_auth_change("selected account has no runtime token");
            broadcast_auth_event(AuthEvent::AccountSwitched);
        }
        return Err(format!(
            "Saved account @{username} has no runtime token. Re-authenticate this account."
        ));
    }

    if !distribute_token_for_generation(&acc.token, switched_credential_generation).await {
        return Err("Account switch superseded by a newer account change".to_string());
    }

    {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if credential_generation() != switched_credential_generation
            || !active_access_token_matches(&acc.token)
        {
            return Err("Account switch superseded by a newer account change".to_string());
        }
        super::eventsub::reset_for_auth_change("account switched");
        spawn_user_scoped_resource_refresh(acc.token.clone(), None, blocked_users_auth_epoch, true);
        crate::log_auth!(info, "Switched to account: {}", username);
        broadcast_auth_event(AuthEvent::AccountSwitched);
    }
    Ok(())
}

/// Remove an account
pub async fn remove_account(username: &str) -> Result<(), String> {
    use super::settings::remove_account_from_settings;

    let (
        credentials_changed,
        blocked_users_auth_epoch,
        switched_to,
        replacement_account,
        accounts_empty,
        removal_credential_generation,
    ) = {
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        let accounts = super::settings::get_accounts();
        let removed_active_account = accounts
            .iter()
            .any(|account| account.is_active && account.username == username);
        let removes_last_account = accounts.len() == 1
            && accounts
                .first()
                .is_some_and(|account| account.username == username);
        let credentials_changed = removed_active_account || removes_last_account;
        let revocable_token = accounts
            .iter()
            .find(|account| account.username == username)
            .filter(|account| !account.token.trim().is_empty())
            .map(|account| {
                let client_id = if account.client_id.trim().is_empty() {
                    CLIENT_ID.to_string()
                } else {
                    account.client_id.trim().to_string()
                };
                (client_id, account.token.trim().to_string())
            });
        let blocked_users_auth_epoch =
            credentials_changed.then(|| super::blocked_users::begin_auth_transition(false));
        if credentials_changed {
            revoke_account_scoped_runtime_commit_held();
        }
        let switched_to = remove_account_from_settings(username)?;
        if let Some((client_id, token)) = revocable_token {
            spawn_token_revoke(username.to_string(), client_id, token);
        }
        let replacement_account = switched_to
            .as_ref()
            .and_then(|_| super::settings::get_active_account());
        let accounts_empty = super::settings::get_accounts().is_empty();
        if credentials_changed {
            let mut state = AUTH_STATE.write_or_recover();
            if let Some(acc) = replacement_account.as_ref() {
                let has_runtime_token = !acc.token.trim().is_empty();
                state.authenticated = has_runtime_token;
                state.username = Some(acc.username.clone());
                state.display_name = acc.display_name.clone();
                state.profile_image_url = acc.profile_image_url.clone();
                state.error = (!has_runtime_token).then(|| {
                    format!(
                        "Saved account @{} has no runtime token. Re-authenticate this account.",
                        acc.username
                    )
                });
                state.needs_reauth = !has_runtime_token;
            } else if accounts_empty {
                state.authenticated = false;
                state.username = None;
                state.display_name = None;
                state.profile_image_url = None;
                state.error = None;
                state.needs_reauth = false;
            }
        }
        let removal_credential_generation = if credentials_changed {
            advance_credential_generation()
        } else {
            credential_generation()
        };
        (
            credentials_changed,
            blocked_users_auth_epoch,
            switched_to,
            replacement_account,
            accounts_empty,
            removal_credential_generation,
        )
    };

    if let Some(new_username) = switched_to {
        if let Some(acc) = replacement_account {
            let has_runtime_token = !acc.token.trim().is_empty();
            if has_runtime_token {
                if !distribute_token_for_generation(&acc.token, removal_credential_generation).await
                {
                    return Ok(());
                }
            } else {
                if !clear_distributed_token_for_generation(removal_credential_generation).await {
                    return Ok(());
                }
            }
            {
                let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
                let still_current = credential_generation() == removal_credential_generation
                    && if has_runtime_token {
                        active_access_token_matches(&acc.token)
                    } else {
                        super::settings::get_token().is_none()
                    };
                if !still_current {
                    return Ok(());
                }
                if has_runtime_token {
                    if let Some(auth_epoch) = blocked_users_auth_epoch {
                        spawn_user_scoped_resource_refresh(
                            acc.token.clone(),
                            None,
                            auth_epoch,
                            true,
                        );
                    }
                } else if let Some(auth_epoch) = blocked_users_auth_epoch {
                    super::blocked_users::finish_auth_transition_without_session(auth_epoch);
                }
                super::eventsub::reset_for_auth_change("active account removed");
                crate::log_auth!(info, "Active account removed; switched to {new_username}");
                broadcast_auth_event(AuthEvent::AccountSwitched);
            }
        }
    } else if accounts_empty {
        if !clear_distributed_token_for_generation(removal_credential_generation).await {
            return Ok(());
        }
        let _commit_guard = AUTH_CREDENTIAL_COMMIT_LOCK.lock_or_recover();
        if credential_generation() != removal_credential_generation
            || !super::settings::get_accounts().is_empty()
        {
            return Ok(());
        }
        if !credentials_changed {
            super::blocked_users::reset_for_auth_change();
        } else if let Some(auth_epoch) = blocked_users_auth_epoch {
            super::blocked_users::finish_auth_transition_without_session(auth_epoch);
        }
        super::eventsub::reset_for_auth_change("last account removed");
        broadcast_auth_event(AuthEvent::LoggedOut);
    }

    crate::log_auth!(info, "Removed account: {}", username);
    Ok(())
}

/// Best-effort revoke of a removed account's access token, off the logout
/// path. Twitch's device-code (public client) refresh token cannot be revoked
/// separately; revoking the access token ends that grant.
fn spawn_token_revoke(username: String, client_id: String, token: String) {
    crate::backend::bridge::RUNTIME.spawn(async move {
        let client = http::timed_client(Duration::from_secs(10));
        let params = [("client_id", client_id.as_str()), ("token", token.as_str())];
        match client
            .post("https://id.twitch.tv/oauth2/revoke")
            .form(&params)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                crate::log_auth!(
                    info,
                    "Revoked the Twitch token of removed account {username}"
                );
            }
            Ok(response) => {
                crate::log_auth!(
                    warn,
                    "Twitch did not revoke the token of removed account {username}: HTTP {}",
                    response.status()
                );
            }
            Err(error) => {
                crate::log_auth!(
                    warn,
                    "Could not reach Twitch to revoke the token of removed account {username}: {}",
                    error.without_url()
                );
            }
        }
    });
}

/// Logout from current account (removes it)
pub async fn logout() -> Result<(), String> {
    let username = AUTH_STATE.read_or_recover().username.clone();
    if let Some(name) = username {
        remove_account(&name).await
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod startup_validation_retry_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn retry_delays_back_off_then_go_hourly() {
        assert_eq!(startup_validation_retry_delay(0), Duration::from_secs(30));
        assert_eq!(startup_validation_retry_delay(1), Duration::from_secs(120));
        assert_eq!(startup_validation_retry_delay(2), Duration::from_secs(300));
        assert_eq!(
            startup_validation_retry_delay(3),
            HOURLY_TOKEN_VALIDATION_INTERVAL
        );
        assert_eq!(
            startup_validation_retry_delay(50),
            HOURLY_TOKEN_VALIDATION_INTERVAL
        );
    }
}
