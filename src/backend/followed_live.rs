//! Followed channels that are live, from Helix `GET /helix/streams/followed`.
//!
//! One account-fenced snapshot backs the "Live now" lists in the quick
//! switcher and the empty-split channel chooser. It is fetched only while
//! someone can see one of those lists: when a list opens, then at most every
//! [`SUCCESS_TTL`] while the switcher is open or a chooser's input has focus
//! in the active window. The one other fetch warms the list after sign-in or
//! an account switch. Nothing polls in the background, and at most one fetch
//! per account runs at a time.

use crate::backend::lock_utils::MutexExt;
use crate::backend::twitch_api::{HelixAuthExt, TWITCH_HTTP_CLIENT, send_helix_idempotent};
pub use echo_core::followed_live::{FollowedLiveStatus, FollowedLiveStream};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const SUCCESS_TTL: Duration = Duration::from_secs(60);
const TRANSIENT_BACKOFF: Duration = Duration::from_secs(45);
/// A token without `user:read:follows` stays refused until the user signs in
/// again, which starts a new account generation and clears this backoff.
const PERMISSION_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// After this long without a successful fetch, failures stop showing the
/// last good list and say the check failed instead.
const STALE_LIST_LIMIT: Duration = Duration::from_secs(10 * 60);
/// How long a signed-in session may lack a usable account identity before
/// "Checking who's live…" gives way to the unavailable line.
const IDENTITY_GRACE: Duration = Duration::from_secs(15);
/// A publish wakes the UI only if a visible list asked for data this recently.
const INTEREST_WINDOW_MS: u64 = 5_000;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 5;
const MAX_STREAMS: usize = PAGE_SIZE * MAX_PAGES;
const MAX_PAGE_BYTES: usize = 512 * 1024;

/// The list as the UI should show it right now.
#[derive(Clone)]
pub struct FollowedLiveSnapshot {
    pub status: FollowedLiveStatus,
    /// Most viewers first. Empty unless `status` is `Ready`.
    pub streams: Arc<Vec<FollowedLiveStream>>,
}

struct CachedFollowedLive {
    auth_generation: u64,
    /// The account the list belongs to. A token refresh starts a new
    /// generation for the same account, which keeps showing this list.
    owner_user_id: String,
    status: FollowedLiveStatus,
    streams: Arc<Vec<FollowedLiveStream>>,
    refresh_after: Instant,
    last_success: Option<Instant>,
}

struct IdentityMissing {
    auth_generation: u64,
    since: Instant,
    reported: bool,
}

static CACHE: Mutex<Option<CachedFollowedLive>> = Mutex::new(None);
/// Account generation of the fetch in flight, if any.
static IN_FLIGHT: Mutex<Option<u64>> = Mutex::new(None);
static IDENTITY_MISSING: Mutex<Option<IdentityMissing>> = Mutex::new(None);
static VERSION: AtomicU64 = AtomicU64::new(0);
static OBSERVED_AUTH_GENERATION: AtomicU64 = AtomicU64::new(u64::MAX);
static LAST_INTEREST_MS: AtomicU64 = AtomicU64::new(0);

/// Changes whenever [`snapshot`] may return something new, including after an
/// account change, so open surfaces can refresh their rows.
pub fn version() -> u64 {
    let auth_generation = crate::backend::auth::credential_generation();
    if OBSERVED_AUTH_GENERATION.swap(auth_generation, Ordering::AcqRel) != auth_generation {
        VERSION.fetch_add(1, Ordering::AcqRel) + 1
    } else {
        VERSION.load(Ordering::Acquire)
    }
}

/// The current account's last-good list without blocking on the network.
pub fn snapshot() -> FollowedLiveSnapshot {
    if !signed_in() {
        return FollowedLiveSnapshot {
            status: FollowedLiveStatus::SignedOut,
            streams: Arc::default(),
        };
    }
    let auth_generation = crate::backend::auth::credential_generation();
    let cached = CACHE.lock_or_recover().as_ref().map(|entry| {
        (
            entry.auth_generation,
            entry.owner_user_id.clone(),
            entry.status,
            entry.streams.clone(),
        )
    });
    if let Some((entry_generation, owner_user_id, status, streams)) = cached
        && (entry_generation == auth_generation
            || crate::backend::auth::is_active_authenticated_user_id(&owner_user_id))
    {
        return FollowedLiveSnapshot { status, streams };
    }
    let identity_missing = IDENTITY_MISSING
        .lock_or_recover()
        .as_ref()
        .is_some_and(|missing| missing.auth_generation == auth_generation && missing.reported);
    FollowedLiveSnapshot {
        status: if identity_missing {
            FollowedLiveStatus::Unavailable
        } else {
            FollowedLiveStatus::Loading
        },
        streams: Arc::default(),
    }
}

/// Fetch the list when the cached answer for this account has expired. Call
/// it only from a list that is on screen: it also marks that someone is
/// looking, so the result wakes the UI when it lands.
///
/// Cheap when nothing is due. Signed-out sessions and streamer mode never
/// fetch.
pub fn refresh_if_stale() {
    LAST_INTEREST_MS.store(crate::utils::unix_now_ms(), Ordering::Relaxed);
    refresh_if_due();
}

/// Fetch once after sign-in or an account switch, without waking the UI for
/// a result nobody is looking at yet.
pub(crate) fn warm_after_sign_in() {
    refresh_if_due();
}

/// Forget the list. Called with the auth commit lock held whenever the
/// account-scoped runtime is revoked (account switch, logout, anonymous mode).
pub(crate) fn clear_commit_held() {
    if CACHE.lock_or_recover().take().is_some() {
        VERSION.fetch_add(1, Ordering::AcqRel);
    }
}

fn refresh_if_due() {
    if !signed_in() || crate::backend::streamer_mode::is_streamer_mode_active_cached() {
        return;
    }
    if !refresh_is_due(crate::backend::auth::credential_generation()) {
        return;
    }
    let Some(resource) = crate::backend::auth::capture_authenticated_user_resource() else {
        note_identity_missing(crate::backend::auth::credential_generation());
        return;
    };
    *IDENTITY_MISSING.lock_or_recover() = None;
    let auth_generation = resource.generation();
    {
        // Deciding under the in-flight lock means a fetch that just published
        // cannot be followed by a duplicate from a caller that saw it as due.
        let mut in_flight = IN_FLIGHT.lock_or_recover();
        if *in_flight == Some(auth_generation) || !refresh_is_due(auth_generation) {
            return;
        }
        *in_flight = Some(auth_generation);
    }
    spawn_refresh(resource);
}

fn signed_in() -> bool {
    !crate::bench::ui_fixture::is_enabled()
        && !crate::backend::irc::is_anonymous_mode()
        && crate::backend::bridge::is_authenticated()
}

fn refresh_is_due(auth_generation: u64) -> bool {
    let now = Instant::now();
    CACHE
        .lock_or_recover()
        .as_ref()
        .is_none_or(|entry| entry.auth_generation != auth_generation || now >= entry.refresh_after)
}

/// Signed in, but the account identity is not resolved. After
/// [`IDENTITY_GRACE`] the snapshot reports the list as unavailable.
fn note_identity_missing(auth_generation: u64) {
    let mut missing = IDENTITY_MISSING.lock_or_recover();
    match missing.as_mut() {
        Some(state) if state.auth_generation == auth_generation => {
            if !state.reported && state.since.elapsed() >= IDENTITY_GRACE {
                state.reported = true;
                VERSION.fetch_add(1, Ordering::AcqRel);
            }
        }
        _ => {
            *missing = Some(IdentityMissing {
                auth_generation,
                since: Instant::now(),
                reported: false,
            });
        }
    }
}

fn someone_is_looking() -> bool {
    crate::utils::unix_now_ms().saturating_sub(LAST_INTEREST_MS.load(Ordering::Relaxed))
        < INTEREST_WINDOW_MS
}

struct InFlightGuard(u64);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut in_flight = IN_FLIGHT.lock_or_recover();
        if *in_flight == Some(self.0) {
            *in_flight = None;
        }
    }
}

fn spawn_refresh(resource: crate::backend::auth::AuthenticatedUserResource) {
    let auth_generation = resource.generation();
    crate::backend::bridge::RUNTIME.spawn(async move {
        let _in_flight = InFlightGuard(auth_generation);
        let outcome = match fetch_followed_live(&resource).await {
            Ok(Some(streams)) => Ok(streams),
            Ok(None) => return,
            Err(error) => Err(error),
        };
        let backoff = match &outcome {
            Ok(_) => SUCCESS_TTL,
            Err(FetchError::NeedsPermission) => {
                crate::log_net!(
                    debug,
                    "Followed live channels refused; the token lacks user:read:follows"
                );
                PERMISSION_BACKOFF
            }
            Err(FetchError::Transient(error)) => {
                crate::log_net!(debug, "Followed live channels refresh failed: {error}");
                TRANSIENT_BACKOFF
            }
        };

        let published = resource.commit_if_current(|| {
            let mut cache = CACHE.lock_or_recover();
            let previous = cache
                .as_ref()
                .filter(|entry| entry.owner_user_id == resource.user_id)
                .map(|entry| (entry.status, entry.streams.clone(), entry.last_success));
            let now = Instant::now();
            let (status, streams, last_success) = match outcome {
                Ok(streams) => (FollowedLiveStatus::Ready, Arc::new(streams), Some(now)),
                Err(FetchError::NeedsPermission) => {
                    (FollowedLiveStatus::NeedsPermission, Arc::default(), None)
                }
                Err(FetchError::Transient(_)) => {
                    let last_success = previous.as_ref().and_then(|previous| previous.2);
                    match &previous {
                        Some((FollowedLiveStatus::Ready, streams, Some(succeeded)))
                            if now.duration_since(*succeeded) < STALE_LIST_LIMIT =>
                        {
                            (FollowedLiveStatus::Ready, streams.clone(), last_success)
                        }
                        _ => (
                            FollowedLiveStatus::Unavailable,
                            Arc::default(),
                            last_success,
                        ),
                    }
                }
            };
            let changed = previous.is_none_or(|(previous_status, previous_streams, _)| {
                previous_status != status || previous_streams != streams
            });
            *cache = Some(CachedFollowedLive {
                auth_generation,
                owner_user_id: resource.user_id.clone(),
                status,
                streams,
                refresh_after: now + backoff,
                last_success,
            });
            changed
        });
        if published == Some(true) {
            VERSION.fetch_add(1, Ordering::AcqRel);
            if someone_is_looking() {
                crate::managers::frame_rate_controller::record_activity();
                crate::managers::frame_rate_controller::wakeup();
            }
        }
    });
}

enum FetchError {
    NeedsPermission,
    Transient(String),
}

#[derive(Deserialize)]
struct HelixFollowedStreamsResponse {
    #[serde(default)]
    data: Vec<HelixFollowedStream>,
    #[serde(default)]
    pagination: HelixPagination,
}

#[derive(Deserialize)]
struct HelixFollowedStream {
    #[serde(default)]
    user_login: String,
    #[serde(default)]
    user_name: String,
    #[serde(default)]
    game_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    viewer_count: u32,
    #[serde(default)]
    started_at: String,
}

#[derive(Default, Deserialize)]
struct HelixPagination {
    #[serde(default)]
    cursor: Option<String>,
}

/// Every live followed stream, up to [`MAX_STREAMS`]. `Ok(None)` means the
/// account changed mid-fetch and the result must be dropped.
async fn fetch_followed_live(
    resource: &crate::backend::auth::AuthenticatedUserResource,
) -> Result<Option<Vec<FollowedLiveStream>>, FetchError> {
    let mut streams = Vec::new();
    let mut seen_logins = HashSet::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_PAGES {
        if !resource.is_current() {
            return Ok(None);
        }
        let url = followed_streams_url(&resource.user_id, cursor.as_deref());
        let response = send_helix_idempotent(&resource.access_token, |token| {
            TWITCH_HTTP_CLIENT.get(url.clone()).helix_auth(token)
        })
        .await
        .map_err(|error| {
            FetchError::Transient(format!(
                "request failed: {}",
                crate::backend::http::error_kind(&error)
            ))
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(FetchError::NeedsPermission);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // `send_helix_idempotent` already renewed an expired session. A 401
            // that remains is a permission problem only when Twitch names a
            // missing scope; anything else is retried like other failures.
            let body = crate::backend::http::error_body(response).await;
            return Err(if is_missing_scope(&body) {
                FetchError::NeedsPermission
            } else {
                FetchError::Transient("Twitch returned 401".to_string())
            });
        }
        if !status.is_success() {
            return Err(FetchError::Transient(format!("Twitch returned {status}")));
        }

        let page: HelixFollowedStreamsResponse = read_bounded_json(response).await?;
        for stream in page.data {
            if streams.len() >= MAX_STREAMS {
                break;
            }
            let login = stream.user_login.trim().to_ascii_lowercase();
            if login.is_empty() || !seen_logins.insert(login.clone()) {
                continue;
            }
            let display_name = stream.user_name.trim();
            streams.push(FollowedLiveStream {
                display_name: if display_name.is_empty() {
                    login.clone()
                } else {
                    display_name.to_string()
                },
                login,
                title: stream.title.trim().to_string(),
                category: stream.game_name.trim().to_string(),
                viewer_count: stream.viewer_count,
                started_at: chrono::DateTime::parse_from_rfc3339(&stream.started_at)
                    .map(|started| started.timestamp())
                    .unwrap_or(0),
            });
        }

        // Viewer counts shift between pages, so a repeated cursor or a full
        // list ends the walk with what was already read.
        match page
            .pagination
            .cursor
            .filter(|next| !next.trim().is_empty())
        {
            Some(next) if streams.len() < MAX_STREAMS && seen_cursors.insert(next.clone()) => {
                cursor = Some(next);
            }
            _ => break,
        }
    }

    echo_core::followed_live::sort_by_viewers(&mut streams);
    Ok(Some(streams))
}

/// Whether a Helix 401 body names a missing scope, as in
/// `{"message":"Missing scope: user:read:follows"}`.
fn is_missing_scope(body: &str) -> bool {
    body.to_ascii_lowercase().contains("missing scope")
}

fn followed_streams_url(user_id: &str, after: Option<&str>) -> Url {
    let mut url = Url::parse("https://api.twitch.tv/helix/streams/followed")
        .expect("static Twitch followed-streams URL must be valid");
    let first = PAGE_SIZE.to_string();
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("user_id", user_id)
            .append_pair("first", &first);
        if let Some(after) = after {
            query.append_pair("after", after);
        }
    }
    url
}

async fn read_bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, FetchError> {
    let too_large =
        || FetchError::Transient(format!("response exceeded {} KiB", MAX_PAGE_BYTES / 1024));
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PAGE_BYTES as u64)
    {
        return Err(too_large());
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_PAGE_BYTES),
    );
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        FetchError::Transient(format!(
            "body read failed: {}",
            crate::backend::http::error_kind(&error)
        ))
    })? {
        if chunk.len() > MAX_PAGE_BYTES.saturating_sub(body.len()) {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    // serde's messages can quote response values; log only the category.
    serde_json::from_slice(&body).map_err(|error| {
        FetchError::Transient(format!("invalid response ({:?})", error.classify()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn followed_streams_url_pages_with_after_cursor() {
        let first = followed_streams_url("123", None);
        assert_eq!(
            first.as_str(),
            "https://api.twitch.tv/helix/streams/followed?user_id=123&first=100"
        );
        let next = followed_streams_url("123", Some("abc=="));
        assert_eq!(
            next.as_str(),
            "https://api.twitch.tv/helix/streams/followed?user_id=123&first=100&after=abc%3D%3D"
        );
    }

    #[test]
    fn followed_live_missing_scope_needs_the_scope_message() {
        assert!(is_missing_scope(
            r#"{"error":"Unauthorized","status":401,"message":"Missing scope: user:read:follows"}"#
        ));
        assert!(!is_missing_scope(
            r#"{"error":"Unauthorized","status":401,"message":"Invalid OAuth token"}"#
        ));
    }
}
