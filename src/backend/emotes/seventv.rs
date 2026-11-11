//! 7TV REST API: response types, the global, channel, emote-set and personal
//! fetchers with their short-lived coalescing caches, and conversion into
//! Echo emotes.

use super::*;

static SEVENTV_USER_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_seventv_user_http_client));

fn build_seventv_user_http_client() -> reqwest::Client {
    match crate::backend::http::client_builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create 7TV user HTTP client: {e}");
            crate::backend::http::timed_client(Duration::from_secs(20))
        }
    }
}

static SEVENTV_EMOTE_SET_HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_seventv_emote_set_http_client));

fn build_seventv_emote_set_http_client() -> reqwest::Client {
    match crate::backend::http::client_builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4)
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::error!("Failed to create 7TV emote-set HTTP client: {e}");
            crate::backend::http::timed_client(Duration::from_secs(25))
        }
    }
}

/// Rate limiter for 7TV API requests (10 req/s - conservative, actual limit higher)
static SEVENTV_LIMITER: LazyLock<
    RateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>,
> = LazyLock::new(|| RateLimiter::direct(Quota::per_second(NonZeroU32::new(10).unwrap())));

#[derive(Clone)]
struct SevenTvGlobalFetchCacheEntry {
    fetched_at: Instant,
    emotes: SevenTvGlobalFetchResult,
}

pub(super) type SevenTvGlobalFetchResult = Arc<Vec<Arc<Emote>>>;

#[derive(Clone)]
pub(super) struct SevenTvChannelFetchCacheEntry {
    pub(super) fetched_at: Instant,
    pub(super) result: SevenTvChannelFetchResult,
}

#[derive(Clone)]
pub(crate) enum SevenTvExactSetFetchOutcome {
    Present {
        emotes: Vec<Arc<Emote>>,
        emote_set_id: Option<String>,
    },
    TerminallyAbsent {
        emote_set_id: String,
    },
}

impl SevenTvExactSetFetchOutcome {
    #[inline]
    pub(super) fn emote_count(&self) -> usize {
        match self {
            Self::Present { emotes, .. } => emotes.len(),
            Self::TerminallyAbsent { .. } => 0,
        }
    }
}

/// One 7TV channel fetch: the channel's emote set, and the EventAPI
/// subscription that keeps it live when the set id is known.
#[derive(Clone)]
pub(super) struct SevenTvChannelFetch {
    pub(super) outcome: SevenTvExactSetFetchOutcome,
    pub(super) subscription: Option<seventv_eventapi::SevenTvChannelSubscription>,
}
pub(super) type SevenTvChannelFetchResult = Arc<SevenTvChannelFetch>;

/// Short-lived 7TV fetch caches used to coalesce bursts from sync/deferred refresh paths.
static SEVENTV_GLOBAL_FETCH_CACHE: LazyLock<RwLock<Option<SevenTvGlobalFetchCacheEntry>>> =
    LazyLock::new(|| RwLock::new(None));
static SEVENTV_GLOBAL_FETCH_IN_FLIGHT: LazyLock<Mutex<Option<Arc<tokio::sync::Notify>>>> =
    LazyLock::new(|| Mutex::new(None));
pub(super) static SEVENTV_CHANNEL_FETCH_CACHE: LazyLock<
    DashMap<String, SevenTvChannelFetchCacheEntry>,
> = LazyLock::new(DashMap::new);
static SEVENTV_CHANNEL_FETCH_IN_FLIGHT: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
pub(super) struct SevenTvEmoteSet {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emotes: Vec<SevenTvEmote>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum SevenTvEmoteSetRefOrObject {
    Full(SevenTvEmoteSet),
    Id(String),
}

#[derive(Deserialize)]
pub(super) struct SevenTvEmote {
    id: String,
    name: String,
    data: SevenTvEmoteData,
}

#[derive(Deserialize)]
struct SevenTvEmoteData {
    name: Option<String>,
    host: SevenTvHost,
    owner: Option<SevenTvOwner>,
    #[serde(default)]
    animated: bool,
    #[serde(default)]
    pub flags: u32,
}

#[derive(Deserialize)]
struct SevenTvOwner {
    display_name: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct SevenTvHost {
    url: String,
    files: Vec<SevenTvFile>,
}

#[derive(Deserialize)]
pub(super) struct SevenTvFile {
    name: String,
    format: String,
    #[serde(default)]
    frame_count: u32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

#[derive(Deserialize)]
pub(super) struct SevenTvUser {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emote_set: Option<SevenTvEmoteSetRefOrObject>,
    #[serde(default)]
    emote_set_id: Option<String>,
    #[serde(default)]
    user: Option<SevenTvUserProfile>,
}

/// Reduced 7TV user shape for personal-set discovery. Unknown channel-set and
/// identity fields are intentionally ignored by serde.
#[derive(Deserialize)]
pub(super) struct SevenTvUserWithPersonalEmotes {
    user: Option<SevenTvUserProfile>,
}

/// Reference to an emote set in user.emote_sets array
#[derive(Deserialize)]
struct SevenTvEmoteSetRef {
    id: String,
    #[serde(default)]
    flags: u32,
}

/// User profile containing emote_sets array
#[derive(Deserialize)]
pub(super) struct SevenTvUserProfile {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    emote_sets: Vec<SevenTvEmoteSetRef>,
    #[serde(default)]
    connections: Vec<SevenTvConnection>,
}

#[derive(Deserialize)]
struct SevenTvConnection {
    #[serde(default)]
    id: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    emote_set_id: Option<String>,
}

/// Normalize 7TV host URL once (often schemeless like "//cdn.7tv.app/...").
/// Returns the base URL with trailing slash stripped for efficient appending.
pub(super) fn normalize_seventv_host(host_url: &str) -> String {
    let trimmed = host_url.trim();
    let base = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{}", trimmed)
    } else {
        format!("https://{}", trimmed)
    };
    base.trim_end_matches('/').to_string()
}

/// Build 7TV image URL from pre-normalized base (avoids repeated normalization).
#[inline]
pub(super) fn build_seventv_url_from_base(base: &str, file_name: &str) -> String {
    let file = file_name.trim_start_matches('/');
    let mut url = String::with_capacity(base.len() + 1 + file.len());
    url.push_str(base);
    url.push('/');
    url.push_str(file);
    url
}

/// Find 7TV resolution files (1x, 2x, 4x) from a host's file list.
/// Returns (file_1x, file_2x, file_4x) - each may be None if not found.
/// Prefers WebP (animated decode support), then PNG, then a supported partial match.
pub(super) fn find_seventv_resolution_files(
    files: &[SevenTvFile],
) -> (
    Option<&SevenTvFile>,
    Option<&SevenTvFile>,
    Option<&SevenTvFile>,
) {
    let find_res = |scale: &str| {
        files
            .iter()
            .find(|f| f.name == format!("{scale}.webp"))
            .or_else(|| files.iter().find(|f| f.name == format!("{scale}.png")))
            .or_else(|| {
                files.iter().find(|f| {
                    f.name.contains(scale)
                        && matches!(
                            f.format.to_ascii_uppercase().as_str(),
                            "WEBP" | "PNG" | "GIF"
                        )
                })
            })
    };
    (
        find_res("1x"),
        find_res("2x"),
        find_res("4x").or_else(|| find_res("3x")),
    )
}

#[inline]
pub(super) fn clone_cached_seventv_global(ttl: Duration) -> Option<SevenTvGlobalFetchResult> {
    let cache = SEVENTV_GLOBAL_FETCH_CACHE.read_or_recover();
    cache
        .as_ref()
        .filter(|entry| entry.fetched_at.elapsed() <= ttl)
        .map(|entry| Arc::clone(&entry.emotes))
}

#[inline]
pub(super) fn clone_cached_seventv_channel(
    cache_key: &str,
    ttl: Duration,
) -> Option<SevenTvChannelFetchResult> {
    SEVENTV_CHANNEL_FETCH_CACHE
        .get(cache_key)
        .filter(|entry| entry.fetched_at.elapsed() <= ttl)
        .map(|entry| Arc::clone(&entry.result))
}

pub(super) fn prune_seventv_channel_fetch_cache() {
    let max_ttl = Duration::from_secs(SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS);
    SEVENTV_CHANNEL_FETCH_CACHE.retain(|_, entry| entry.fetched_at.elapsed() <= max_ttl);
    let len = SEVENTV_CHANNEL_FETCH_CACHE.len();
    if len <= SEVENTV_CHANNEL_FETCH_CACHE_MAX_ENTRIES {
        return;
    }

    let overflow = len - SEVENTV_CHANNEL_FETCH_CACHE_MAX_ENTRIES;
    let keys_to_remove: Vec<String> = SEVENTV_CHANNEL_FETCH_CACHE
        .iter()
        .take(overflow)
        .map(|entry| entry.key().clone())
        .collect();
    for key in keys_to_remove {
        SEVENTV_CHANNEL_FETCH_CACHE.remove(&key);
    }
}

pub(super) fn invalidate_seventv_global_fetch_cache() {
    *SEVENTV_GLOBAL_FETCH_CACHE.write_or_recover() = None;
}

pub(super) fn invalidate_seventv_channel_fetch_cache(channel_login: &str) {
    let channel_key = normalize_channel_key(channel_login);
    let suffix = format!(":{}", channel_key);
    SEVENTV_CHANNEL_FETCH_CACHE.retain(|key, _| !key.ends_with(&suffix));
    SEVENTV_CHANNEL_SYNC_TIMEOUT_LAST_AT.remove(&channel_key);
}

pub(crate) fn invalidate_seventv_fetch_cache_for_event(
    channel_login: Option<&str>,
    is_global: bool,
) {
    if is_global {
        invalidate_seventv_global_fetch_cache();
    } else if let Some(channel) = channel_login {
        invalidate_seventv_channel_fetch_cache(channel);
    }
}

pub(super) async fn fetch_seventv_global() -> Result<SevenTvGlobalFetchResult, String> {
    struct GlobalFetchInFlightGuard {
        notify: Option<Arc<tokio::sync::Notify>>,
    }

    impl Drop for GlobalFetchInFlightGuard {
        fn drop(&mut self) {
            if let Some(notify) = self.notify.take() {
                let mut in_flight = SEVENTV_GLOBAL_FETCH_IN_FLIGHT.lock_or_recover();
                in_flight.take();
                notify.notify_waiters();
            }
        }
    }

    let ttl = Duration::from_secs(SEVENTV_GLOBAL_FETCH_CACHE_TTL_SECS);

    loop {
        if let Some(cached) = clone_cached_seventv_global(ttl) {
            record_emote_stage_duration(
                "emotes.7tv.global.cache_hit",
                Duration::ZERO,
                cached.len(),
            );
            return Ok(cached);
        }

        let mut leader_guard: Option<GlobalFetchInFlightGuard> = None;
        let existing_request = {
            let mut in_flight = SEVENTV_GLOBAL_FETCH_IN_FLIGHT.lock_or_recover();
            if let Some(existing) = in_flight.as_ref() {
                // Created under the lock: `notify_waiters` stores no permit, so
                // a future created after the leader finishes would never wake.
                Some(Arc::clone(existing).notified_owned())
            } else {
                let notify = Arc::new(tokio::sync::Notify::new());
                *in_flight = Some(Arc::clone(&notify));
                leader_guard = Some(GlobalFetchInFlightGuard {
                    notify: Some(notify),
                });
                None
            }
        };

        if let Some(wait_for) = existing_request {
            let wait_start = Instant::now();
            wait_for.await;
            record_emote_stage_timed("emotes.7tv.global.wait_inflight", wait_start, 0);
            continue;
        }

        let fetch_start = Instant::now();
        let result = fetch_seventv_global_raw().await.map(Arc::new);
        let item_count = result.as_ref().map(|emotes| emotes.len()).unwrap_or(0);
        record_emote_stage_timed("emotes.7tv.global.network", fetch_start, item_count);

        if let Ok(emotes) = &result {
            let cache_store_start = Instant::now();
            *SEVENTV_GLOBAL_FETCH_CACHE.write_or_recover() = Some(SevenTvGlobalFetchCacheEntry {
                fetched_at: Instant::now(),
                emotes: Arc::clone(emotes),
            });
            record_emote_stage_timed(
                "emotes.7tv.global.cache_store",
                cache_store_start,
                emotes.len(),
            );
        }
        drop(leader_guard);

        return result;
    }
}

async fn fetch_seventv_global_raw() -> Result<Vec<Arc<Emote>>, String> {
    SEVENTV_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &SEVENTV_EMOTE_SET_HTTP_CLIENT,
        api::seventv::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(format!(
            "7TV global emotes returned status: {}",
            resp.status()
        ));
    }
    let set: SevenTvEmoteSet = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(ref set_id) = set.id {
        *SEVENTV_GLOBAL_SET_ID.write_or_recover() = Some(set_id.clone());
        seventv_eventapi::subscribe_global_set(set_id);
    }
    Ok(convert_seventv_emotes(set.emotes, None)
        .into_iter()
        .map(Arc::new)
        .collect())
}

pub(crate) async fn fetch_seventv_emote_set_by_id(
    set_id: &str,
    channel_login: Option<&str>,
) -> Result<SevenTvExactSetFetchOutcome, String> {
    SEVENTV_LIMITER.until_ready().await;

    let set_url = api::seventv::emote_set(set_id);
    let set_resp = fetch_with_retry_str(
        &SEVENTV_EMOTE_SET_HTTP_CLIENT,
        &set_url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await?;
    let status = set_resp.status();
    if status.as_u16() == 410 {
        crate::backend::metrics::record_emote_event("7tv.set_fetch.terminal_absent");
        crate::log_emotes!(
            info,
            "7TV emote set {} is terminally absent (HTTP {})",
            set_id,
            status
        );
        return Ok(SevenTvExactSetFetchOutcome::TerminallyAbsent {
            emote_set_id: set_id.to_string(),
        });
    }
    if !status.is_success() {
        crate::backend::metrics::record_emote_event("7tv.set_fetch.http_preserved");
        let error = format!(
            "7TV emote set {} returned HTTP {}; preserving existing provider state",
            set_id, status
        );
        crate::log_emotes!(warn, "{}", error);
        return Err(error);
    }

    let set: SevenTvEmoteSet = set_resp.json().await.map_err(|e| e.to_string())?;
    let emotes = convert_seventv_emotes(set.emotes, channel_login)
        .into_iter()
        .map(Arc::new)
        .collect();
    Ok(SevenTvExactSetFetchOutcome::Present {
        emotes,
        emote_set_id: set.id.or_else(|| Some(set_id.to_string())),
    })
}

/// Fetches 7TV channel emotes and returns (emotes, emote_set_id)
/// The emote_set_id is used for tracking changes and EventAPI subscriptions
pub(super) async fn fetch_seventv_channel(
    channel_id: &str,
    channel_login: &str,
) -> Result<SevenTvChannelFetchResult, String> {
    struct ChannelFetchInFlightGuard {
        key: String,
        notify: Option<Arc<tokio::sync::Notify>>,
    }

    impl Drop for ChannelFetchInFlightGuard {
        fn drop(&mut self) {
            if let Some(notify) = self.notify.take() {
                let mut in_flight = SEVENTV_CHANNEL_FETCH_IN_FLIGHT.lock_or_recover();
                in_flight.remove(&self.key);
                notify.notify_waiters();
            }
        }
    }

    if SEVENTV_MANUAL_RELOAD_FAILED.contains(&normalize_channel_key(channel_login)) {
        return Err("7TV fetch already failed during this manual reload".to_string());
    }
    let eventapi_updates_live = seventv_eventapi::channel_updates_are_live(channel_login);
    let ttl = Duration::from_secs(if eventapi_updates_live {
        SEVENTV_CHANNEL_EVENTAPI_FETCH_CACHE_TTL_SECS
    } else {
        SEVENTV_CHANNEL_FETCH_CACHE_TTL_SECS
    });
    let cache_key = format!("{}:{}", channel_id, channel_key_cow(channel_login));

    loop {
        if let Some(cached) = clone_cached_seventv_channel(&cache_key, ttl) {
            record_emote_stage_duration(
                if eventapi_updates_live {
                    "emotes.7tv.channel.cache_hit.eventapi_live"
                } else {
                    "emotes.7tv.channel.cache_hit.fallback_polling"
                },
                Duration::ZERO,
                cached.outcome.emote_count(),
            );
            return Ok(cached);
        }

        let mut leader_guard: Option<ChannelFetchInFlightGuard> = None;
        let existing_request = {
            let mut in_flight = SEVENTV_CHANNEL_FETCH_IN_FLIGHT.lock_or_recover();
            if let Some(existing) = in_flight.get(&cache_key) {
                // Created under the lock; see the global fetch above.
                Some(Arc::clone(existing).notified_owned())
            } else {
                let notify = Arc::new(tokio::sync::Notify::new());
                in_flight.insert(cache_key.clone(), Arc::clone(&notify));
                leader_guard = Some(ChannelFetchInFlightGuard {
                    key: cache_key.clone(),
                    notify: Some(notify),
                });
                None
            }
        };

        if let Some(wait_for) = existing_request {
            let wait_start = Instant::now();
            wait_for.await;
            record_emote_stage_timed("emotes.7tv.channel.wait_inflight", wait_start, 0);
            continue;
        }

        let fetch_start = Instant::now();
        let result = fetch_seventv_channel_raw(channel_id, channel_login)
            .await
            .map(Arc::new);
        let item_count = result
            .as_ref()
            .map(|result| result.outcome.emote_count())
            .unwrap_or(0);
        record_emote_stage_timed("emotes.7tv.channel.network", fetch_start, item_count);

        if let Ok(shared_result) = &result {
            let cache_store_start = Instant::now();
            SEVENTV_CHANNEL_FETCH_CACHE.insert(
                cache_key.clone(),
                SevenTvChannelFetchCacheEntry {
                    fetched_at: Instant::now(),
                    result: Arc::clone(shared_result),
                },
            );
            prune_seventv_channel_fetch_cache();
            record_emote_stage_timed(
                "emotes.7tv.channel.cache_store",
                cache_store_start,
                shared_result.outcome.emote_count(),
            );
        }
        drop(leader_guard);

        return result;
    }
}

async fn fetch_seventv_channel_raw(
    channel_id: &str,
    channel_login: &str,
) -> Result<SevenTvChannelFetch, String> {
    SEVENTV_LIMITER.until_ready().await;

    let url = api::seventv::user(channel_id);
    let resp = fetch_with_retry_str(
        &SEVENTV_USER_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await
    .map_err(|e| {
        crate::log_emotes!(
            debug,
            "7TV channel emotes unavailable for {} ({}): {} - will retry on next refresh",
            channel_login,
            channel_id,
            e
        );
        e
    })?;

    if !resp.status().is_success() {
        return Ok(SevenTvChannelFetch {
            outcome: SevenTvExactSetFetchOutcome::Present {
                emotes: vec![],
                emote_set_id: None,
            },
            subscription: None,
        });
    }

    let user: SevenTvUser = resp.json().await.map_err(|e| e.to_string())?;
    let subscription_identity = seventv_channel_subscription_identity(&user, channel_id);

    match user.emote_set {
        Some(SevenTvEmoteSetRefOrObject::Full(set)) => {
            if set.emotes.is_empty()
                && let Some(set_id) = set.id.clone().or_else(|| user.emote_set_id.clone())
            {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} had empty emote_set object, fetching set {} separately",
                    channel_login,
                    set_id
                );
                let outcome = fetch_seventv_emote_set_by_id(&set_id, Some(channel_login)).await?;
                let subscription =
                    seventv_channel_subscription_from_identity(&subscription_identity, &set_id);
                return Ok(SevenTvChannelFetch {
                    outcome,
                    subscription,
                });
            }

            let emote_set_id = set.id.clone().or(user.emote_set_id.clone());
            let subscription = emote_set_id.as_deref().and_then(|set_id| {
                seventv_channel_subscription_from_identity(&subscription_identity, set_id)
            });
            let emotes = convert_seventv_emotes(set.emotes, Some(channel_login))
                .into_iter()
                .map(Arc::new)
                .collect();
            Ok(SevenTvChannelFetch {
                outcome: SevenTvExactSetFetchOutcome::Present {
                    emotes,
                    emote_set_id,
                },
                subscription,
            })
        }
        Some(SevenTvEmoteSetRefOrObject::Id(set_id)) => {
            crate::log_emotes!(
                debug,
                "7TV user payload for {} returned emote_set as id {}, fetching separately",
                channel_login,
                set_id
            );
            let outcome = fetch_seventv_emote_set_by_id(&set_id, Some(channel_login)).await?;
            let subscription =
                seventv_channel_subscription_from_identity(&subscription_identity, &set_id);
            Ok(SevenTvChannelFetch {
                outcome,
                subscription,
            })
        }
        None => {
            if let Some(set_id) = user.emote_set_id {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} returned emote_set_id {}, fetching separately",
                    channel_login,
                    set_id
                );
                let outcome = fetch_seventv_emote_set_by_id(&set_id, Some(channel_login)).await?;
                let subscription =
                    seventv_channel_subscription_from_identity(&subscription_identity, &set_id);
                Ok(SevenTvChannelFetch {
                    outcome,
                    subscription,
                })
            } else {
                crate::log_emotes!(
                    debug,
                    "7TV user payload for {} had no emote_set or emote_set_id",
                    channel_login
                );
                Ok(SevenTvChannelFetch {
                    outcome: SevenTvExactSetFetchOutcome::Present {
                        emotes: vec![],
                        emote_set_id: None,
                    },
                    subscription: None,
                })
            }
        }
    }
}

fn seventv_channel_subscription_identity(
    user: &SevenTvUser,
    channel_id: &str,
) -> Option<(String, usize)> {
    let profile = user.user.as_ref()?;
    let user_id = profile
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .or_else(|| user.id.as_deref().filter(|id| !id.is_empty()))?;
    let connection_index = seventv_twitch_connection_index(user, profile, channel_id)?;
    Some((user_id.to_string(), connection_index))
}

fn seventv_twitch_connection_index(
    user: &SevenTvUser,
    profile: &SevenTvUserProfile,
    channel_id: &str,
) -> Option<usize> {
    let twitch_connections: Vec<_> = profile
        .connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| connection.platform.eq_ignore_ascii_case("TWITCH"))
        .collect();

    if let Some((index, _)) = twitch_connections
        .iter()
        .find(|(_, connection)| connection.id == channel_id)
    {
        return Some(*index);
    }

    let target_set_id = user
        .emote_set
        .as_ref()
        .and_then(seventv_emote_set_ref_id)
        .or(user.emote_set_id.as_deref())
        .filter(|set_id| !set_id.is_empty());

    if let Some(target_set_id) = target_set_id
        && let Some((index, _)) = twitch_connections.iter().find(|(_, connection)| {
            connection
                .emote_set_id
                .as_deref()
                .is_some_and(|set_id| set_id == target_set_id)
        })
    {
        return Some(*index);
    }

    if twitch_connections.len() == 1 {
        return Some(twitch_connections[0].0);
    }

    None
}

fn seventv_emote_set_ref_id(value: &SevenTvEmoteSetRefOrObject) -> Option<&str> {
    match value {
        SevenTvEmoteSetRefOrObject::Full(set) => set.id.as_deref(),
        SevenTvEmoteSetRefOrObject::Id(set_id) if !set_id.is_empty() => Some(set_id.as_str()),
        SevenTvEmoteSetRefOrObject::Id(_) => None,
    }
}

fn seventv_channel_subscription_from_identity(
    identity: &Option<(String, usize)>,
    emote_set_id: &str,
) -> Option<seventv_eventapi::SevenTvChannelSubscription> {
    if emote_set_id.is_empty() {
        return None;
    }
    let (user_id, connection_index) = identity.as_ref()?;
    Some(seventv_eventapi::SevenTvChannelSubscription {
        user_id: user_id.clone(),
        emote_set_id: emote_set_id.to_string(),
        connection_index: *connection_index,
    })
}

/// Fetch 7TV personal emotes for a user
/// Personal emotes are a 7TV subscriber feature that allows up to 5 emotes to be used globally
/// They are stored in a separate emote set with flags == 4 (Personal flag)
pub(super) async fn fetch_seventv_personal_emotes(
    twitch_user_id: &str,
) -> Result<SevenTvExactSetFetchOutcome, String> {
    crate::log_emotes!(
        info,
        "Fetching 7TV personal emotes for Twitch user: {}",
        twitch_user_id
    );

    let url = api::seventv::user(twitch_user_id);
    let resp = fetch_with_retry_str(
        &SEVENTV_USER_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&SEVENTV_LIMITER),
    )
    .await
    .map_err(|e| {
        crate::log_emotes!(info, "7TV personal emotes unavailable: {}", e);
        e
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err("User not found on 7TV".to_string());
        }
        return Err(format!("7TV API returned status: {}", status));
    }

    let user_data: SevenTvUserWithPersonalEmotes = resp.json().await.map_err(|e| {
        crate::log_emotes!(info, "Failed to parse 7TV user response: {}", e);
        e.to_string()
    })?;

    let personal_set_id = user_data
        .user
        .and_then(|u| u.emote_sets.into_iter().find(|s| s.flags == 4))
        .map(|s| s.id);

    let Some(set_id) = personal_set_id else {
        crate::log_emotes!(
            info,
            "No personal emote set found for user {}",
            twitch_user_id
        );
        return Ok(SevenTvExactSetFetchOutcome::Present {
            emotes: vec![],
            emote_set_id: None,
        });
    };

    crate::log_emotes!(
        info,
        "Found personal emote set ID: {} for user {}",
        set_id,
        twitch_user_id
    );

    fetch_seventv_emote_set_by_id(&set_id, Some(PERSONAL_SEVENTV_CHANNEL)).await
}

fn convert_seventv_emotes(emotes: Vec<SevenTvEmote>, channel: Option<&str>) -> Vec<Emote> {
    emotes
        .into_iter()
        .filter_map(|emote| convert_seventv_emote(emote, channel))
        .collect()
}

pub(super) fn convert_seventv_emote(e: SevenTvEmote, channel: Option<&str>) -> Option<Emote> {
    let (file_1x, file_2x, file_4x) = find_seventv_resolution_files(&e.data.host.files);

    let default_file = file_1x
        .or(file_2x)
        .or(file_4x)
        .or_else(|| e.data.host.files.first())?;

    let original_name = e.data.name.filter(|orig| orig != &e.name);

    let creator_name = e
        .data
        .owner
        .and_then(|owner| owner.display_name.or(owner.username));

    let base_url = normalize_seventv_host(&e.data.host.url);
    let default_url = build_seventv_url_from_base(&base_url, &default_file.name);

    // Prefer the smallest available source for logical dimensions, but fall
    // through when that file has missing/invalid geometry. Keep width/height
    // paired: a partial provider dimension is not authoritative.
    let dimension_candidates = [
        file_1x.map(|file| (file, 1u32)),
        file_2x.map(|file| (file, 2u32)),
        file_4x.map(|file| {
            (
                file,
                if file.name.starts_with("3x.") || file.name.starts_with("3x_") {
                    3
                } else {
                    4
                },
            )
        }),
    ];
    let (width, height) = dimension_candidates
        .into_iter()
        .flatten()
        .chain(e.data.host.files.iter().filter_map(|file| {
            let (tier, suffix) = file.name.split_once('x')?;
            if !suffix.starts_with('.') && !suffix.starts_with('_') {
                return None;
            }
            let scale = match tier {
                "1" => 1,
                "2" => 2,
                "3" => 3,
                "4" => 4,
                _ => return None,
            };
            Some((file, scale))
        }))
        .find_map(|(file, scale)| {
            (file.width > 0 && file.height > 0).then(|| {
                (
                    (file.width as f32 / scale as f32).round().max(1.0) as u32,
                    (file.height as f32 / scale as f32).round().max(1.0) as u32,
                )
            })
        })
        .map_or((None, None), |(width, height)| (Some(width), Some(height)));
    let is_animated = e.data.animated
        || (e.data.flags & 1) != 0
        || default_file.frame_count > 1
        || default_file.format.eq_ignore_ascii_case("GIF");

    Some(Emote {
        id: Arc::from(e.id.clone()),
        name: Arc::from(e.name),
        url: Arc::from(default_url),
        provider: EmoteProvider::SevenTV,
        channel: channel.map(Arc::from),
        original_name: original_name.map(Arc::from),
        creator_name: creator_name.map(Arc::from),
        twitch_owner_login: None,
        twitch_owner_id: None,
        twitch_emote_type: None,
        url_1x: file_1x.map(|f| Arc::from(build_seventv_url_from_base(&base_url, &f.name))),
        url_2x: file_2x.map(|f| Arc::from(build_seventv_url_from_base(&base_url, &f.name))),
        url_4x: file_4x.map(|f| Arc::from(build_seventv_url_from_base(&base_url, &f.name))),
        zero_width: (e.data.flags & 256) != 0,
        is_animated,
        width,
        height,
    })
}
