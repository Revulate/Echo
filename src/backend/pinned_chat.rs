//! Read-only Twitch pinned-chat retrieval.
//!
//! Twitch exposes at most one moderator-pinned message per channel through
//! `GET /helix/chat/pins`.  This module keeps a small, account-fenced cache so
//! rendering a split never performs Helix work or repeatedly probes a missing
//! moderator scope.

use crate::backend::constants::CLIENT_ID;
use crate::backend::lock_utils::MutexExt;
use dashmap::DashMap;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const SUCCESS_TTL: Duration = Duration::from_secs(60);
const FAILURE_BACKOFF: Duration = Duration::from_secs(5 * 60);
const TRANSIENT_BACKOFF: Duration = Duration::from_secs(45);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CACHED_CHANNELS: usize = 256;
const MAX_IN_FLIGHT_REFRESHES: usize = 8;
const MAX_PENDING_REFRESHES: usize = MAX_CACHED_CHANNELS;

/// A moderator-pinned Twitch chat message suitable for a compact chat header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedChatMessage {
    pub message_id: String,
    pub sender_login: String,
    pub sender_name: String,
    pub text: String,
    pub ends_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone)]
struct CachedPinnedChat {
    auth_generation: u64,
    message: Option<Arc<PinnedChatMessage>>,
    refresh_after: Instant,
}

static PINNED_CHAT_CACHE: LazyLock<DashMap<String, CachedPinnedChat>> = LazyLock::new(DashMap::new);
static PINNED_CHAT_REFRESHES: LazyLock<Mutex<PinnedChatRefreshQueue>> =
    LazyLock::new(|| Mutex::new(PinnedChatRefreshQueue::default()));
static PINNED_CHAT_VERSION: AtomicU64 = AtomicU64::new(0);
static PINNED_CHAT_AUTH_GENERATION: AtomicU64 = AtomicU64::new(u64::MAX);

type PinnedChatRefreshKey = (u64, String);

/// The cache is bounded, so the refresh scheduler must remain bounded too.
/// Pending entries retain only an account generation and normalized channel;
/// the current credential resource is recaptured when a slot becomes free.
#[derive(Default)]
struct PinnedChatRefreshQueue {
    in_flight: HashSet<PinnedChatRefreshKey>,
    pending: VecDeque<PinnedChatRefreshKey>,
    pending_keys: HashSet<PinnedChatRefreshKey>,
}

impl PinnedChatRefreshQueue {
    fn discard_stale_pending(&mut self, auth_generation: u64) {
        self.pending
            .retain(|(generation, _)| *generation == auth_generation);
        self.pending_keys
            .retain(|(generation, _)| *generation == auth_generation);
    }

    fn reserve_or_queue(&mut self, key: PinnedChatRefreshKey) -> bool {
        self.discard_stale_pending(key.0);
        if self.in_flight.contains(&key) || self.pending_keys.contains(&key) {
            return false;
        }

        if self.in_flight.len() < MAX_IN_FLIGHT_REFRESHES {
            self.in_flight.insert(key);
            return true;
        }

        if self.pending.len() < MAX_PENDING_REFRESHES {
            self.pending_keys.insert(key.clone());
            self.pending.push_back(key);
        }
        false
    }

    fn release_and_promote(
        &mut self,
        completed: PinnedChatRefreshKey,
    ) -> Option<PinnedChatRefreshKey> {
        self.in_flight.remove(&completed);
        let auth_generation = crate::backend::auth::credential_generation();
        self.discard_stale_pending(auth_generation);

        while let Some(key) = self.pending.pop_front() {
            self.pending_keys.remove(&key);
            if key.0 != auth_generation {
                continue;
            }
            self.in_flight.insert(key.clone());
            return Some(key);
        }
        None
    }
}

#[derive(Deserialize)]
struct HelixPinnedChatResponse {
    #[serde(default)]
    data: Vec<HelixPinnedChatMessage>,
}

#[derive(Deserialize)]
struct HelixPinnedChatMessage {
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    sender_login: String,
    #[serde(default)]
    sender_name: String,
    #[serde(default)]
    message: HelixPinnedChatBody,
    #[serde(default)]
    ends_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Default, Deserialize)]
struct HelixPinnedChatBody {
    #[serde(default)]
    text: String,
}

/// Version of the visible pin cache. Retained split headers use this to avoid
/// re-rendering for unrelated backend traffic while still waking when a pin
/// changes.
pub fn version() -> u64 {
    let auth_generation = crate::backend::auth::credential_generation();
    let observed_generation = PINNED_CHAT_AUTH_GENERATION.swap(auth_generation, Ordering::AcqRel);
    if observed_generation != auth_generation {
        PINNED_CHAT_VERSION.fetch_add(1, Ordering::AcqRel) + 1
    } else {
        PINNED_CHAT_VERSION.load(Ordering::Acquire)
    }
}

/// Return the last-good pinned message for a channel without blocking.
pub fn get_cached(channel: &str) -> Option<Arc<PinnedChatMessage>> {
    let channel = normalize_channel(channel)?;
    let auth_generation = crate::backend::auth::credential_generation();
    PINNED_CHAT_CACHE
        .get(&channel)
        .filter(|entry| entry.auth_generation == auth_generation)
        .and_then(|entry| entry.message.clone())
}

/// Queue an account-fenced refresh if the channel's cached result has expired.
///
/// Anonymous or unauthenticated sessions intentionally do nothing. A 401/403
/// is cached as a quiet absence for a longer backoff; viewing chat must not
/// produce error toasts or repeatedly probe unavailable moderation authority.
pub fn queue_refresh(channel: &str) {
    if crate::bench::ui_fixture::is_enabled() {
        return;
    }
    let Some(channel) = normalize_channel(channel) else {
        return;
    };
    if crate::backend::whispers::is_whispers_channel(&channel) {
        return;
    }
    let Some(resource) = crate::backend::auth::capture_authenticated_user_resource() else {
        return;
    };
    let auth_generation = resource.generation();

    if !refresh_is_due(&channel, auth_generation) {
        return;
    }
    let key = (auth_generation, channel.clone());
    if !PINNED_CHAT_REFRESHES
        .lock_or_recover()
        .reserve_or_queue(key)
    {
        return;
    }

    spawn_refresh(channel, resource);
}

fn refresh_is_due(channel: &str, auth_generation: u64) -> bool {
    let now = Instant::now();
    !PINNED_CHAT_CACHE
        .get(channel)
        .is_some_and(|entry| entry.auth_generation == auth_generation && now < entry.refresh_after)
}

fn spawn_refresh(channel: String, resource: crate::backend::auth::AuthenticatedUserResource) {
    let auth_generation = resource.generation();
    crate::backend::bridge::RUNTIME.spawn(async move {
        let _in_flight = PinnedChatInFlightGuard((auth_generation, channel.clone()));

        if !resource.is_current() {
            return;
        }

        let outcome = match crate::backend::twitch_api::get_cached_channel_id(&channel) {
            Some(id) => fetch_pinned_chat(&id, &resource.user_id, &resource.access_token).await,
            None => match crate::backend::twitch_api::get_user_id_helix(
                &channel,
                &resource.access_token,
            )
            .await
            {
                Ok(id) => {
                    crate::backend::twitch_api::cache_channel_id(&channel, &id);
                    fetch_pinned_chat(&id, &resource.user_id, &resource.access_token).await
                }
                Err(error) => Err(PinnedChatFetchError::Transient(format!(
                    "channel lookup failed: {error}"
                ))),
            },
        };
        let backoff = match &outcome {
            Ok(_) => SUCCESS_TTL,
            Err(PinnedChatFetchError::Unauthorized) => FAILURE_BACKOFF,
            Err(PinnedChatFetchError::Transient(error)) => {
                crate::log_net!(debug, "Pinned chat refresh failed for {channel}: {error}");
                TRANSIENT_BACKOFF
            }
        };

        let published = resource.commit_if_current(|| {
            let previous = PINNED_CHAT_CACHE
                .get(&channel)
                .filter(|entry| entry.auth_generation == auth_generation)
                .and_then(|entry| entry.message.clone());
            let message = match outcome {
                Ok(message) => message.map(Arc::new),
                Err(PinnedChatFetchError::Unauthorized) => None,
                Err(PinnedChatFetchError::Transient(_)) => previous,
            };
            let changed = PINNED_CHAT_CACHE
                .get(&channel)
                .filter(|entry| entry.auth_generation == auth_generation)
                .is_none_or(|previous| previous.message != message);
            PINNED_CHAT_CACHE.insert(
                channel.clone(),
                CachedPinnedChat {
                    auth_generation,
                    message,
                    refresh_after: Instant::now() + backoff,
                },
            );
            trim_cache();
            changed
        });
        if published == Some(true) {
            PINNED_CHAT_VERSION.fetch_add(1, Ordering::AcqRel);
            crate::managers::frame_rate_controller::record_activity();
            crate::managers::frame_rate_controller::wakeup();
        }
    });
}

struct PinnedChatInFlightGuard(PinnedChatRefreshKey);

impl Drop for PinnedChatInFlightGuard {
    fn drop(&mut self) {
        let mut completed = Some(self.0.clone());
        while let Some(next) = completed.take().and_then(|key| {
            PINNED_CHAT_REFRESHES
                .lock_or_recover()
                .release_and_promote(key)
        }) {
            let Some(resource) = crate::backend::auth::capture_authenticated_user_resource() else {
                completed = Some(next);
                continue;
            };
            if resource.generation() != next.0 || !refresh_is_due(&next.1, next.0) {
                completed = Some(next);
                continue;
            }

            spawn_refresh(next.1, resource);
            break;
        }
    }
}

fn normalize_channel(channel: &str) -> Option<String> {
    let channel = echo_core::channel_identity::normalize_channel_key(channel.trim());
    (!channel.is_empty()).then_some(channel)
}

fn trim_cache() {
    if PINNED_CHAT_CACHE.len() <= MAX_CACHED_CHANNELS {
        return;
    }
    let stale_key = PINNED_CHAT_CACHE
        .iter()
        .min_by_key(|entry| entry.refresh_after)
        .map(|entry| entry.key().clone());
    if let Some(key) = stale_key {
        PINNED_CHAT_CACHE.remove(&key);
    }
}

enum PinnedChatFetchError {
    Unauthorized,
    Transient(String),
}

async fn fetch_pinned_chat(
    broadcaster_id: &str,
    moderator_id: &str,
    access_token: &str,
) -> Result<Option<PinnedChatMessage>, PinnedChatFetchError> {
    let response = crate::backend::twitch_api::TWITCH_HTTP_CLIENT
        .get(format!(
            "https://api.twitch.tv/helix/chat/pins?broadcaster_id={}&moderator_id={}",
            urlencoding::encode(broadcaster_id),
            urlencoding::encode(moderator_id),
        ))
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Client-Id", CLIENT_ID)
        .send()
        .await
        .map_err(|error| PinnedChatFetchError::Transient(format!("request failed: {error}")))?;

    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err(PinnedChatFetchError::Unauthorized);
    }
    if !response.status().is_success() {
        return Err(PinnedChatFetchError::Transient(format!(
            "Twitch returned {}",
            response.status()
        )));
    }

    let payload: HelixPinnedChatResponse = read_bounded_json(response).await?;
    Ok(payload.data.into_iter().next().and_then(|message| {
        let message_id = message.message_id.trim().to_string();
        if message_id.is_empty() {
            return None;
        }
        let sender_login = message.sender_login.trim().to_string();
        let sender_name = message.sender_name.trim().to_string();
        Some(PinnedChatMessage {
            message_id,
            sender_name: if sender_name.is_empty() {
                sender_login.clone()
            } else {
                sender_name
            },
            sender_login,
            text: message.message.text.trim().to_string(),
            ends_at: message.ends_at.filter(|value| !value.trim().is_empty()),
            updated_at: message.updated_at.filter(|value| !value.trim().is_empty()),
        })
    }))
}

async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, PinnedChatFetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(PinnedChatFetchError::Transient(format!(
            "response exceeded {} KiB",
            MAX_RESPONSE_BYTES / 1024
        )));
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_RESPONSE_BYTES),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| PinnedChatFetchError::Transient(format!("body read failed: {error}")))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(PinnedChatFetchError::Transient(format!(
                "response exceeded {} KiB",
                MAX_RESPONSE_BYTES / 1024
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|error| PinnedChatFetchError::Transient(format!("invalid response: {error}")))
}
