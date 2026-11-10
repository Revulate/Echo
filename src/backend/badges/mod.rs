//! Chat badges: Twitch global and channel badges (Helix, with the API proxy
//! and an embedded table as fallbacks), FFZ, BTTV and 7TV badges, and the
//! per-user decorations the renderer looks up for each row.

use dashmap::{DashMap, DashSet};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use tokio_tungstenite::tungstenite::Message;

use crate::backend::api_error::ApiError;
use crate::backend::constants::{API_PROXY, HELIX_BADGES_GLOBAL_ENDPOINT};
use crate::backend::http;
use crate::backend::twitch_api::{HelixAuthExt, send_helix_idempotent};
use echo_core::channel_identity::normalize_channel_key;

pub use super::badges_core::{
    BTTV_PRO_BADGE_KEY, NO_AUDIO_BADGE_KEY, NO_VIDEO_BADGE_KEY, badge_key_matches, badge_key_name,
    badge_name_matches, bttv_badge_cache_key, bttv_pro_badge_cache_key,
    embedded_twitch_badge_fallback_url, ffz_badge_cache_key, is_lead_moderator_badge_name,
    is_moderator_badge_key, normalize_badge_cache_key,
};

pub static TWITCH_BADGE_CACHE: LazyLock<TwitchBadgeCache> = LazyLock::new(TwitchBadgeCache::new);

/// Tracks channels currently being loaded to prevent duplicate concurrent fetches
static LOADING_CHANNELS: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);
/// Longest time a renderer may reserve badge slots for a set that has not
/// finished its first fetch attempt.
const BADGE_SET_PENDING_MAX: Duration = Duration::from_secs(30);
static GLOBAL_BADGES_ATTEMPTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static GLOBAL_BADGES_FIRST_QUERY: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static CHANNEL_BADGES_ATTEMPTED: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);
static CHANNEL_BADGES_FIRST_QUERY: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);

fn mark_channel_badges_attempted(channel_login: &str) {
    let key = normalize_channel_key(channel_login.trim());
    CHANNEL_BADGES_FIRST_QUERY.remove(&key);
    CHANNEL_BADGES_ATTEMPTED.insert(key);
}

/// True while the global badge set or this channel's set has not finished its
/// first fetch attempt, so rows can reserve badge slots instead of shifting.
/// A reservation never outlives `BADGE_SET_PENDING_MAX`; anonymous or
/// token-less sessions and the Whispers/Watching pseudo-channels, which never
/// load a set, never reserve.
pub fn is_badge_set_pending(channel_login: &str) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};

    const SESSION_RECHECK_MS: u64 = 1_000;
    static SESSION_CHECK_BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static SESSION_CHECK: AtomicU64 = AtomicU64::new(0);

    let channel = channel_login.trim().trim_start_matches('#');
    if channel.is_empty()
        || echo_core::channel_identity::is_whispers_channel(channel)
        || echo_core::channel_identity::is_watching_channel(channel)
    {
        return false;
    }
    let lowercase;
    let key = if channel.bytes().any(|byte| byte.is_ascii_uppercase()) {
        lowercase = channel.to_ascii_lowercase();
        lowercase.as_str()
    } else {
        channel
    };
    let global_attempted = GLOBAL_BADGES_ATTEMPTED.load(Ordering::Acquire);
    let global_pending = !global_attempted
        && GLOBAL_BADGES_FIRST_QUERY
            .get_or_init(Instant::now)
            .elapsed()
            < BADGE_SET_PENDING_MAX;
    let channel_first_query = if global_pending {
        None
    } else {
        if CHANNEL_BADGES_ATTEMPTED.contains(key) {
            return false;
        }
        let first_query = CHANNEL_BADGES_FIRST_QUERY.get(key).map(|entry| *entry);
        if first_query.is_some_and(|first| first.elapsed() >= BADGE_SET_PENDING_MAX) {
            return false;
        }
        first_query
    };

    let now_ms = SESSION_CHECK_BASE
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis() as u64
        + 1;
    let cached = SESSION_CHECK.load(Ordering::Relaxed);
    let has_session = if cached != 0 && now_ms.saturating_sub(cached >> 1) < SESSION_RECHECK_MS {
        cached & 1 == 1
    } else {
        let has_session = !crate::backend::settings::get_anonymous_mode()
            && crate::backend::settings::get_token().is_some();
        SESSION_CHECK.store((now_ms << 1) | u64::from(has_session), Ordering::Relaxed);
        has_session
    };
    if !has_session {
        return false;
    }

    global_pending
        || channel_first_query.is_some()
        || CHANNEL_BADGES_FIRST_QUERY
            .entry(key.to_owned())
            .or_insert_with(Instant::now)
            .elapsed()
            < BADGE_SET_PENDING_MAX
}

static BTTV_PRO_LOOKUP_LIMIT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));
const BTTV_PRO_SOCKET_URL: &str = "wss://sockets.betterttv.net/ws";
const BTTV_PRO_USER_TTL: Duration = Duration::from_secs(30 * 60);
const BTTV_PRO_SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

/// Cache entry with TTL tracking
struct CacheEntry {
    badges: HashMap<String, String>,
    fetched_at: Instant,
}

#[derive(Clone)]
struct BttvProUserCacheEntry {
    login: Arc<str>,
    badge_url: Option<Arc<str>>,
    fetched_at: Instant,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FfzChannelBadgeState {
    moderator_url: Option<Arc<str>>,
    vip_url: Option<Arc<str>>,
    badges_by_user_id: HashMap<String, smallvec::SmallVec<[u64; 2]>>,
}

/// Twitch badge cache with global and per-channel badges
/// Named TwitchBadgeCache to distinguish from potential future badge providers
pub struct TwitchBadgeCache {
    /// Global badges from Twitch
    global: RwLock<Option<CacheEntry>>,
    /// Single-flight guard for global Twitch badge fetches
    global_fetch_lock: tokio::sync::Mutex<()>,
    /// Per-channel badges (channel_id -> badges)
    channels: DashMap<String, CacheEntry>,
    /// External badge URL lookup ("ffz:supporter" / "bttv:developer" -> url)
    external_badge_urls: DashMap<String, Arc<str>>,
    /// External badge assignments keyed by lowercase Twitch login
    external_by_login: DashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
    /// FFZ global badge assignments keyed by Twitch user ID.
    external_by_user_id: DashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
    /// FFZ badge catalog lookup used by both global and room assignments.
    ffz_badge_keys_by_id: DashMap<u64, Arc<str>>,
    /// FFZ room-specific moderator, VIP, and user badge assignments.
    ffz_channel_badges: DashMap<String, FfzChannelBadgeState>,
    /// External/global third-party badge fetch time
    external_fetched_at: RwLock<Option<Instant>>,
    /// Single-flight guard for external (FFZ/BTTV) badge fetches
    external_fetch_lock: tokio::sync::Mutex<()>,
    /// On-demand BTTV Pro lookups keyed by Twitch user ID
    bttv_pro_users: DashMap<String, BttvProUserCacheEntry>,
    /// Prevent duplicate concurrent BTTV Pro lookups for the same Twitch user ID
    bttv_pro_loading: DashSet<String>,
    /// OAuth token for Helix API
    token: RwLock<Option<String>>,
    /// Cache TTL (default: 1 hour)
    ttl: Duration,
    /// HTTP Client
    client: http::ClientCache,
}

/// Rows laid out before a badge set loaded lack those badges; rebuild them.
fn notify_all_badge_decorations_changed() {
    crate::backend::observers::get_observer_registry()
        .notify_all(crate::backend::observers::ChannelEvent::BadgeDecorationsChanged);
}
fn build_badge_http_client() -> reqwest::Client {
    http::timed_client(Duration::from_secs(20))
}

impl TwitchBadgeCache {
    pub fn new() -> Self {
        Self {
            global: RwLock::new(None),
            global_fetch_lock: tokio::sync::Mutex::new(()),
            channels: DashMap::new(),
            external_badge_urls: DashMap::new(),
            external_by_login: DashMap::new(),
            external_by_user_id: DashMap::new(),
            ffz_badge_keys_by_id: DashMap::new(),
            ffz_channel_badges: DashMap::new(),
            external_fetched_at: RwLock::new(None),
            external_fetch_lock: tokio::sync::Mutex::new(()),
            bttv_pro_users: DashMap::new(),
            bttv_pro_loading: DashSet::new(),
            token: RwLock::new(None),
            ttl: Duration::from_secs(60 * 60),
            client: http::ClientCache::new(build_badge_http_client),
        }
    }
}

impl Default for TwitchBadgeCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TwitchBadgeCache {
    /// Set the OAuth token for API requests
    pub async fn set_token(&self, token: String) {
        *self.token.write().await = Some(token);
    }

    /// Clear the OAuth token (used when entering anonymous mode)
    pub async fn clear_token(&self) {
        *self.token.write().await = None;
        crate::log_cache!(debug, "Token cleared");
    }

    /// Resolve Helix credentials from the current account transition boundary.
    /// The distributed cache copy is diagnostic only: account startup/switch
    /// work may briefly lag it even though the authenticated account is already
    /// authoritative, and an older cached token must never outlive logout or
    /// Anonymous mode for badge requests.
    async fn current_authenticated_resource(
        &self,
    ) -> Option<crate::backend::auth::AuthenticatedResourceFence> {
        let distributed = self.token.read().await.clone();
        let auth = crate::backend::auth::capture_authenticated_resource_fence();
        match auth {
            Some(auth) => {
                if distributed.as_deref() != Some(auth.access_token()) {
                    crate::log_cache!(
                        debug,
                        "Badge token cache lagged the active Twitch account; using authoritative credentials"
                    );
                }
                Some(auth)
            }
            None => {
                if distributed.is_some() {
                    crate::log_cache!(
                        debug,
                        "Ignoring stale distributed badge token without an active authenticated account"
                    );
                }
                None
            }
        }
    }

    /// Get badge URL by set_id and version, checking channel-specific first then global (sync version - faster)
    pub fn get_badge_url_sync(&self, badge_key: &str, channel_id: &str) -> Option<String> {
        let cache_key = channel_id.to_lowercase();

        if let Some(url) = self.get_active_ffz_authority_badge_url_sync(badge_key, &cache_key) {
            return Some(url);
        }

        let normalized_key = normalize_badge_cache_key(badge_key);

        if let Some(entry) = self.channels.get(&cache_key)
            && let Some(url) = entry.badges.get(&normalized_key)
        {
            return Some(url.clone());
        }

        if let Ok(global) = self.global.try_read()
            && let Some(entry) = global.as_ref()
            && let Some(url) = entry.badges.get(&normalized_key)
        {
            return Some(url.clone());
        }

        if let Some(url) = self.external_badge_urls.get(&normalized_key) {
            return Some(url.to_string());
        }

        None
    }

    pub fn get_external_badge_keys_for_user_sync(
        &self,
        channel_login: &str,
        login: &str,
        user_id: Option<&str>,
    ) -> Vec<String> {
        let login = login.trim().trim_start_matches('@').to_ascii_lowercase();
        let user_id = user_id.map(str::trim).filter(|value| !value.is_empty());
        let mut resolved = Vec::with_capacity(4);

        if let Some(user_id) = user_id {
            if let Some(badges) = self.external_by_user_id.get(user_id) {
                extend_unique_badge_keys(&mut resolved, badges.iter());
            }

            let channel_login = normalize_channel_key(channel_login.trim());
            if let Some(channel) = self.ffz_channel_badges.get(&channel_login)
                && let Some(badge_ids) = channel.badges_by_user_id.get(user_id)
            {
                for badge_id in badge_ids {
                    if let Some(badge_key) = self.ffz_badge_keys_by_id.get(badge_id) {
                        push_unique_badge_key(&mut resolved, badge_key.as_ref());
                    }
                }
            }
        }

        if !login.is_empty()
            && let Some(badges) = self.external_by_login.get(&login)
        {
            extend_unique_badge_keys(&mut resolved, badges.iter());
        }

        resolved
    }

    pub fn publish_ffz_channel_badges(
        &self,
        channel_login: &str,
        moderator_url: Option<String>,
        vip_url: Option<String>,
        badges_by_user_id: HashMap<String, Vec<u64>>,
    ) {
        let channel_login = normalize_channel_key(channel_login.trim());
        if channel_login.is_empty() {
            return;
        }

        let next = FfzChannelBadgeState {
            moderator_url: moderator_url.map(Arc::from),
            vip_url: vip_url.map(Arc::from),
            badges_by_user_id: badges_by_user_id
                .into_iter()
                .filter_map(|(user_id, badge_ids)| {
                    let user_id = user_id.trim().to_string();
                    (!user_id.is_empty()).then(|| (user_id, badge_ids.into_iter().collect()))
                })
                .collect(),
        };
        let changed = self
            .ffz_channel_badges
            .get(&channel_login)
            .is_none_or(|current| current.value() != &next);
        if !changed {
            return;
        }
        self.ffz_channel_badges.insert(channel_login.clone(), next);

        crate::backend::observers::get_observer_registry().notify(
            &channel_login,
            crate::backend::observers::ChannelEvent::BadgeDecorationsChanged,
        );
    }

    pub fn has_ffz_channel_badge_state_sync(&self, channel_login: &str) -> bool {
        let channel_login = normalize_channel_key(channel_login.trim());
        !channel_login.is_empty() && self.ffz_channel_badges.contains_key(&channel_login)
    }

    pub fn has_ffz_authority_badge_sync(&self, badge_key: &str, channel_login: &str) -> bool {
        let Some(slot) = ffz_authority_slot(badge_key) else {
            return false;
        };
        let channel_login = normalize_channel_key(channel_login.trim());
        let Some(channel) = self.ffz_channel_badges.get(&channel_login) else {
            return false;
        };
        match slot {
            FfzAuthoritySlot::Moderator => channel.moderator_url.is_some(),
            FfzAuthoritySlot::Vip => channel.vip_url.is_some(),
        }
    }

    pub fn is_using_ffz_authority_badge_sync(&self, badge_key: &str, channel_login: &str) -> bool {
        self.has_ffz_authority_badge_sync(badge_key, channel_login)
            && crate::backend::settings::get_show_ffz_badges()
    }

    fn get_active_ffz_authority_badge_url_sync(
        &self,
        badge_key: &str,
        channel_login: &str,
    ) -> Option<String> {
        ffz_authority_slot(badge_key)?;
        crate::backend::settings::get_show_ffz_badges()
            .then(|| self.ffz_authority_badge_url_sync(badge_key, channel_login))
            .flatten()
    }

    fn ffz_authority_badge_url_sync(&self, badge_key: &str, channel_login: &str) -> Option<String> {
        let slot = ffz_authority_slot(badge_key)?;
        let channel_login = normalize_channel_key(channel_login.trim());
        let channel = self.ffz_channel_badges.get(&channel_login)?;
        let url = match slot {
            FfzAuthoritySlot::Moderator => channel.moderator_url.as_ref(),
            FfzAuthoritySlot::Vip => channel.vip_url.as_ref(),
        }?;
        Some(url.to_string())
    }

    pub fn queue_bttv_pro_badge_lookup(
        &self,
        channel_login: &str,
        user_login: &str,
        user_id: Option<&str>,
    ) {
        if !crate::backend::settings::get_show_bttv_pro_badges() {
            return;
        }

        let current_login = crate::backend::settings::get_username_lowercase();
        let target_login = user_login
            .trim()
            .trim_start_matches('@')
            .to_ascii_lowercase();
        if current_login.is_empty() || target_login != current_login {
            return;
        }

        let Some(user_id) = user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return;
        };

        if self
            .bttv_pro_users
            .get(&user_id)
            .is_some_and(|entry| entry.fetched_at.elapsed() <= BTTV_PRO_USER_TTL)
        {
            return;
        }

        if !self.bttv_pro_loading.insert(user_id.clone()) {
            return;
        }

        let channel_login = channel_login.to_string();
        let user_login = user_login.to_string();
        crate::backend::bridge::RUNTIME.spawn(async move {
            let changed = TWITCH_BADGE_CACHE
                .refresh_bttv_pro_badge(&channel_login, &user_login, &user_id)
                .await;
            TWITCH_BADGE_CACHE.bttv_pro_loading.remove(&user_id);

            if changed {
                crate::backend::observers::get_observer_registry().notify(
                    &channel_login,
                    crate::backend::observers::ChannelEvent::UserDecorationsChanged {
                        user_login: Arc::from(user_login.to_ascii_lowercase()),
                        user_id: Some(Arc::from(user_id)),
                    },
                );
            }
        });
    }

    /// Load global badges from Twitch
    pub async fn load_global_badges(&self) -> Result<(), String> {
        let result = self.load_global_badges_inner().await;
        GLOBAL_BADGES_ATTEMPTED.store(true, std::sync::atomic::Ordering::Release);
        result
    }

    async fn load_global_badges_inner(&self) -> Result<(), String> {
        if let Some(entry) = self.global.read().await.as_ref()
            && entry.fetched_at.elapsed() < self.ttl
        {
            return Ok(());
        }

        let _fetch_guard = self.global_fetch_lock.lock().await;

        if let Some(entry) = self.global.read().await.as_ref()
            && entry.fetched_at.elapsed() < self.ttl
        {
            return Ok(());
        }

        let mut wait_count = 0;
        while !crate::backend::auth::is_auth_initialized() && wait_count < 50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            wait_count += 1;
        }

        let auth = self.current_authenticated_resource().await;

        let badges = if let Some(auth) = auth.as_ref() {
            match self.fetch_global_badges(auth.access_token()).await {
                Ok(b) => {
                    crate::log_cache!(info, "Loaded global badges (Helix)");
                    b
                }
                Err(e) if e.is_auth_rejection() => {
                    crate::log_cache!(warn, "Helix auth failed ({}), falling back to CDN", e);
                    match self.fetch_global_badges_proxy().await {
                        Ok(b) => {
                            crate::log_cache!(info, "Loaded global badges (CDN fallback)");
                            b
                        }
                        Err(cdn_err) => {
                            crate::log_cache!(
                                warn,
                                "CDN also failed ({}), using embedded badges",
                                cdn_err
                            );
                            get_embedded_global_badges()
                        }
                    }
                }
                Err(e) => {
                    crate::log_cache!(warn, "Helix failed ({}), trying CDN", e);
                    match self.fetch_global_badges_proxy().await {
                        Ok(b) => b,
                        Err(_) => get_embedded_global_badges(),
                    }
                }
            }
        } else {
            match self.fetch_global_badges_proxy().await {
                Ok(b) => {
                    crate::log_cache!(info, "Loaded global badges (CDN)");
                    b
                }
                Err(e) if e.is_proxy_not_configured() => {
                    crate::log_cache!(debug, "{}, using embedded badges", e);
                    get_embedded_global_badges()
                }
                Err(e) => {
                    crate::log_cache!(warn, "CDN failed ({}), using embedded badges", e);
                    get_embedded_global_badges()
                }
            }
        };

        let entry = CacheEntry {
            badges,
            fetched_at: Instant::now(),
        };
        let mut global = self.global.write().await;
        let changed = global
            .as_ref()
            .is_none_or(|current| current.badges != entry.badges);
        if let Some(auth) = auth {
            auth.commit_if_current(|| *global = Some(entry))
                .ok_or_else(|| {
                    "Twitch account changed while publishing global badges".to_string()
                })?;
        } else {
            *global = Some(entry);
        }
        drop(global);
        if changed {
            notify_all_badge_decorations_changed();
        }

        Ok(())
    }

    /// Load third-party global badge assignments.
    ///
    /// Third-party badges display next to usernames. FFZ and BTTV both expose
    /// unauthenticated global badge assignment responses, so this keeps
    /// ingestion separate from Twitch Helix badge loading while preserving the
    /// same sync render lookup.
    pub async fn load_external_global_badges(&self) -> Result<(), String> {
        if !crate::backend::settings::get_show_ffz_badges()
            && !crate::backend::settings::get_show_ffz_bot_badges()
            && !crate::backend::settings::get_show_bttv_badges()
            && !crate::backend::settings::get_show_bttv_pro_badges()
        {
            return Ok(());
        }

        if let Some(fetched_at) = *self.external_fetched_at.read().await
            && fetched_at.elapsed() < self.ttl
        {
            return Ok(());
        }

        let _fetch_guard = self.external_fetch_lock.lock().await;

        if let Some(fetched_at) = *self.external_fetched_at.read().await
            && fetched_at.elapsed() < self.ttl
        {
            return Ok(());
        }

        let mut next_urls: HashMap<String, Arc<str>> = HashMap::new();
        let mut next_by_login: HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>> = HashMap::new();
        let mut next_by_user_id: HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>> =
            HashMap::new();
        let mut next_ffz_badge_keys_by_id: HashMap<u64, Arc<str>> = HashMap::new();
        let mut loaded_providers = Vec::new();
        let mut errors = Vec::new();

        match self.fetch_ffz_badges().await {
            Ok(response) => {
                ingest_ffz_badges(
                    response,
                    &mut next_urls,
                    &mut next_by_user_id,
                    &mut next_ffz_badge_keys_by_id,
                );
                loaded_providers.push("FFZ");
            }
            Err(error) => errors.push(error),
        }

        match self.fetch_bttv_badges().await {
            Ok(response) => {
                ingest_bttv_badges(response, &mut next_urls, &mut next_by_login);
                loaded_providers.push("BTTV");
            }
            Err(error) => errors.push(error),
        }

        self.reapply_cached_bttv_pro_badges(&mut next_urls, &mut next_by_login);

        if loaded_providers.is_empty() {
            return Err(errors.join("; "));
        }

        let mut changed = false;
        for (key, url) in &next_urls {
            let unchanged = self
                .external_badge_urls
                .get(key)
                .is_some_and(|current| current.value().as_ref() == url.as_ref());
            if !unchanged {
                changed = true;
                self.external_badge_urls
                    .insert(key.clone(), Arc::clone(url));
            }
        }
        let before = self.external_badge_urls.len();
        self.external_badge_urls
            .retain(|k, _| next_urls.contains_key(k.as_str()));
        changed |= self.external_badge_urls.len() != before;

        for (login, badges) in &next_by_login {
            let unchanged = self
                .external_by_login
                .get(login)
                .is_some_and(|current| current.value() == badges);
            if !unchanged {
                changed = true;
                self.external_by_login.insert(login.clone(), badges.clone());
            }
        }
        let before = self.external_by_login.len();
        self.external_by_login
            .retain(|k, _| next_by_login.contains_key(k.as_str()));
        changed |= self.external_by_login.len() != before;

        for (user_id, badges) in &next_by_user_id {
            let unchanged = self
                .external_by_user_id
                .get(user_id)
                .is_some_and(|current| current.value() == badges);
            if !unchanged {
                changed = true;
                self.external_by_user_id
                    .insert(user_id.clone(), badges.clone());
            }
        }
        let before = self.external_by_user_id.len();
        self.external_by_user_id
            .retain(|k, _| next_by_user_id.contains_key(k.as_str()));
        changed |= self.external_by_user_id.len() != before;

        for (badge_id, badge_key) in &next_ffz_badge_keys_by_id {
            let unchanged = self
                .ffz_badge_keys_by_id
                .get(badge_id)
                .is_some_and(|current| current.value().as_ref() == badge_key.as_ref());
            if !unchanged {
                self.ffz_badge_keys_by_id
                    .insert(*badge_id, Arc::clone(badge_key));
            }
        }
        self.ffz_badge_keys_by_id
            .retain(|badge_id, _| next_ffz_badge_keys_by_id.contains_key(badge_id));

        *self.external_fetched_at.write().await = Some(Instant::now());
        for error in errors {
            crate::log_cache!(warn, "External badge provider failed: {}", error);
        }
        crate::log_cache!(
            info,
            "Loaded external badges: providers={}, login_users={}, id_users={}, badges={}",
            loaded_providers.join(","),
            self.external_by_login.len(),
            self.external_by_user_id.len(),
            self.external_badge_urls.len()
        );
        if changed {
            notify_all_badge_decorations_changed();
        }

        Ok(())
    }

    /// Load channel-specific badges
    pub async fn load_channel_badges(
        &self,
        channel_id: &str,
        channel_login: &str,
    ) -> Result<(), ApiError> {
        let cache_key = channel_login.to_lowercase();

        if !LOADING_CHANNELS.insert(cache_key.clone()) {
            crate::log_cache!(debug, "{} already loading, skipping", channel_login);
            return Ok(());
        }

        let result = self
            .load_channel_badges_inner(channel_id, channel_login, &cache_key)
            .await;
        LOADING_CHANNELS.remove(&cache_key);
        mark_channel_badges_attempted(&cache_key);
        result
    }

    /// Inner implementation of load_channel_badges (called after dedup guard)
    async fn load_channel_badges_inner(
        &self,
        channel_id: &str,
        channel_login: &str,
        cache_key: &str,
    ) -> Result<(), ApiError> {
        if let Some(entry) = self.channels.get(cache_key)
            && entry.fetched_at.elapsed() < self.ttl
        {
            return Ok(());
        }

        let mut wait_count = 0;
        while !crate::backend::auth::is_auth_initialized() && wait_count < 50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            wait_count += 1;
        }

        let auth = self.current_authenticated_resource().await;
        let badges = if let Some(auth) = auth.as_ref() {
            match self
                .fetch_channel_badges(channel_id, auth.access_token())
                .await
            {
                Ok(b) => {
                    crate::log_cache!(
                        debug,
                        "Loaded badges for channel: {} (Helix)",
                        channel_login
                    );
                    b
                }
                Err(e) if e.is_auth_rejection() => {
                    crate::log_cache!(
                        warn,
                        "Helix auth failed for {}: {}, falling back to proxy",
                        channel_login,
                        e
                    );
                    let b = self.fetch_channel_badges_proxy(channel_id).await?;
                    crate::log_cache!(
                        debug,
                        "Loaded badges for channel: {} (Proxy fallback)",
                        channel_login
                    );
                    b
                }
                Err(e) => return Err(e),
            }
        } else {
            let b = self.fetch_channel_badges_proxy(channel_id).await?;
            crate::log_cache!(
                debug,
                "Loaded badges for channel: {} (Proxy)",
                channel_login
            );
            b
        };

        let changed = self
            .channels
            .get(cache_key)
            .is_none_or(|current| current.badges != badges);
        let publish = || {
            self.channels.insert(
                cache_key.to_string(),
                CacheEntry {
                    badges,
                    fetched_at: Instant::now(),
                },
            );
        };
        if let Some(auth) = auth {
            auth.commit_if_current(publish).ok_or_else(|| {
                ApiError::account_changed(format!(
                    "Twitch account changed while publishing badges for {}",
                    channel_login
                ))
            })?;
        } else {
            publish();
        }
        if changed {
            crate::backend::observers::get_observer_registry().notify(
                cache_key,
                crate::backend::observers::ChannelEvent::BadgeDecorationsChanged,
            );
        }

        Ok(())
    }

    /// Load channel badges by login name (looks up channel ID first)
    /// This is useful when joining a channel before receiving any messages
    pub async fn load_channel_badges_by_login(&self, channel_login: &str) -> Result<(), ApiError> {
        let cache_key = channel_login.to_lowercase();
        crate::log_cache!(
            debug,
            "load_channel_badges_by_login called for: {}",
            channel_login
        );

        if !LOADING_CHANNELS.insert(cache_key.clone()) {
            crate::log_cache!(
                debug,
                "{} already loading (by_login), skipping",
                channel_login
            );
            return Ok(());
        }

        let result = self
            .load_channel_badges_by_login_inner(channel_login, &cache_key)
            .await;
        LOADING_CHANNELS.remove(&cache_key);
        mark_channel_badges_attempted(&cache_key);
        result
    }

    /// Inner implementation of load_channel_badges_by_login (called after dedup guard)
    async fn load_channel_badges_by_login_inner(
        &self,
        channel_login: &str,
        cache_key: &str,
    ) -> Result<(), ApiError> {
        if let Some(entry) = self.channels.get(cache_key)
            && entry.fetched_at.elapsed() < self.ttl
        {
            crate::log_cache!(
                debug,
                "{} already cached ({} badges), skipping",
                channel_login,
                entry.badges.len()
            );
            return Ok(());
        }

        crate::log_cache!(debug, "Looking up user ID for: {}", channel_login);
        let channel_id = crate::backend::twitch_api::get_user_id_cached(cache_key).await?;
        crate::log_cache!(debug, "Got user ID {} for {}", channel_id, channel_login);

        self.load_channel_badges_inner(&channel_id, channel_login, cache_key)
            .await
    }

    /// Load badges for multiple channels with coordinated rate limiting
    /// Loads up to 5 channels concurrently to avoid overwhelming API
    pub async fn load_channel_badges_batch(
        &self,
        channels: Vec<(String, String)>,
    ) -> Vec<Result<(), ApiError>> {
        use futures_util::stream::{FuturesUnordered, StreamExt};

        let mut tasks = FuturesUnordered::new();
        let mut results = Vec::with_capacity(channels.len());

        for (channel_id, channel_login) in channels {
            let cache_self = self;
            tasks.push(async move {
                let result = cache_self
                    .load_channel_badges(&channel_id, &channel_login)
                    .await;
                (channel_login.clone(), result)
            });

            if tasks.len() >= 5
                && let Some((ch, res)) = tasks.next().await
            {
                crate::log_cache!(debug, "Batch loaded: {}", ch);
                results.push(res);
            }
        }

        while let Some((ch, res)) = tasks.next().await {
            crate::log_cache!(debug, "Batch loaded: {}", ch);
            results.push(res);
        }

        results
    }

    async fn fetch_global_badges(&self, token: &str) -> Result<HashMap<String, String>, ApiError> {
        let resp = send_helix_idempotent(token, |token| {
            self.client
                .get(HELIX_BADGES_GLOBAL_ENDPOINT)
                .helix_auth(token)
        })
        .await
        .map_err(|e| ApiError::transport(e, "Failed to fetch global badges"))?;

        if !resp.status().is_success() {
            return Err(ApiError::status(
                resp.status(),
                format!("Failed to fetch global badges: {}", resp.status()),
            ));
        }

        let response: BadgesResponse = resp
            .json()
            .await
            .map_err(|e| ApiError::other(e.to_string()))?;
        Ok(convert_badges(response))
    }

    async fn fetch_channel_badges(
        &self,
        channel_id: &str,
        token: &str,
    ) -> Result<HashMap<String, String>, ApiError> {
        let url = format!(
            "https://api.twitch.tv/helix/chat/badges?broadcaster_id={}",
            channel_id
        );
        let resp = send_helix_idempotent(token, |token| self.client.get(&url).helix_auth(token))
            .await
            .map_err(|e| ApiError::transport(e, "Failed to fetch channel badges"))?;

        if !resp.status().is_success() {
            return Err(ApiError::status(
                resp.status(),
                format!("Failed to fetch channel badges: {}", resp.status()),
            ));
        }

        let response: BadgesResponse = resp
            .json()
            .await
            .map_err(|e| ApiError::other(e.to_string()))?;
        Ok(convert_badges(response))
    }

    async fn fetch_global_badges_proxy(&self) -> Result<HashMap<String, String>, ApiError> {
        let (proxy_url, proxy_key) = API_PROXY.ok_or_else(ApiError::proxy_not_configured)?;
        let url = format!("{}/badges/global", proxy_url);
        crate::log_cache!(debug, "Fetching global badges from proxy: {}", url);

        let resp = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Cache,
            self.client.get(&url).header("X-Echo-Key", proxy_key),
        )
        .await
        .map_err(|e| ApiError::transport(e, "Failed to fetch global badges from proxy"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = crate::backend::http::error_body(resp).await;
            return Err(ApiError::status(
                status,
                format!(
                    "Failed to fetch global badges from proxy: {} - {}",
                    status, body
                ),
            ));
        }

        let response: BadgesResponse = resp.json().await.map_err(|e| {
            crate::log_cache!(warn, "Failed to parse proxy response: {}", e);
            ApiError::other(e.to_string())
        })?;

        let badges = convert_badges(response);
        crate::log_cache!(debug, "Proxy returned {} badge entries", badges.len());
        Ok(badges)
    }

    async fn fetch_channel_badges_proxy(
        &self,
        channel_id: &str,
    ) -> Result<HashMap<String, String>, ApiError> {
        let (proxy_url, proxy_key) = API_PROXY.ok_or_else(ApiError::proxy_not_configured)?;
        let url = format!("{}/badges/channel/{}", proxy_url, channel_id);
        crate::log_cache!(debug, "Fetching channel badges from proxy: {}", url);

        let resp = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Cache,
            self.client.get(&url).header("X-Echo-Key", proxy_key),
        )
        .await
        .map_err(|e| ApiError::transport(e, "Failed to fetch channel badges from proxy"))?;

        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                crate::log_cache!(debug, "No custom badges for channel {}", channel_id);
                return Ok(HashMap::new());
            }
            return Err(ApiError::status(
                resp.status(),
                format!(
                    "Failed to fetch channel badges from proxy: {}",
                    resp.status()
                ),
            ));
        }

        let response: BadgesResponse = resp.json().await.map_err(|e| {
            crate::log_cache!(warn, "Failed to parse channel proxy response: {}", e);
            ApiError::other(e.to_string())
        })?;

        let badges = convert_badges(response);
        crate::log_cache!(
            debug,
            "Proxy returned {} channel badge entries",
            badges.len()
        );
        Ok(badges)
    }

    async fn fetch_ffz_badges(&self) -> Result<FfzBadgesResponse, String> {
        self.fetch_provider_json("https://api.frankerfacez.com/v1/badges/ids", "FFZ")
            .await
    }

    async fn fetch_bttv_badges(&self) -> Result<Vec<BttvBadgeAssignment>, String> {
        self.fetch_provider_json("https://api.betterttv.net/3/cached/badges", "BTTV")
            .await
    }

    async fn fetch_provider_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        label: &str,
    ) -> Result<T, String> {
        let resp = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Cache,
            self.client.get(url),
        )
        .await
        .map_err(|e| format!("Failed to fetch {label} badges: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("Failed to fetch {label} badges: {}", resp.status()));
        }

        resp.json()
            .await
            .map_err(|e| format!("Failed to parse {label} badges: {e}"))
    }

    async fn refresh_bttv_pro_badge(
        &self,
        channel_login: &str,
        user_login: &str,
        user_id: &str,
    ) -> bool {
        let channel_login = channel_login
            .trim()
            .trim_start_matches('@')
            .to_ascii_lowercase();
        let user_login = user_login
            .trim()
            .trim_start_matches('@')
            .to_ascii_lowercase();
        if channel_login.is_empty() || user_login.is_empty() || user_id.trim().is_empty() {
            return false;
        }

        let previous = self
            .bttv_pro_users
            .get(user_id)
            .map(|entry| (entry.login.clone(), entry.badge_url.clone()));

        let fetched_badge = match self
            .fetch_bttv_pro_badge_socket(&channel_login, user_id)
            .await
        {
            Ok(badge_url) => badge_url,
            Err(error) => {
                crate::log_cache!(
                    debug,
                    "BTTV Pro badge lookup failed for {} ({} in #{}): {}",
                    user_login,
                    user_id,
                    channel_login,
                    error
                );
                None
            }
        };

        let previous_had_badge = previous
            .as_ref()
            .and_then(|(_, badge_url)| badge_url.as_ref())
            .is_some();
        let next_had_badge = fetched_badge.is_some();

        let mut changed = false;
        if let Some((previous_login, previous_badge_url)) = previous.as_ref()
            && previous_badge_url.is_some()
            && (previous_login.as_ref() != user_login || previous_badge_url != &fetched_badge)
        {
            changed |= self.remove_external_badge_assignment(previous_login, BTTV_PRO_BADGE_KEY);
        }

        if let Some(badge_url) = fetched_badge.as_ref() {
            changed |= self.set_external_badge_url(BTTV_PRO_BADGE_KEY, badge_url);
            changed |= self.add_external_badge_assignment(&user_login, BTTV_PRO_BADGE_KEY);
        } else {
            changed |= self.remove_external_badge_assignment(&user_login, BTTV_PRO_BADGE_KEY);
        }

        self.bttv_pro_users.insert(
            user_id.to_string(),
            BttvProUserCacheEntry {
                login: Arc::from(user_login.as_str()),
                badge_url: fetched_badge.clone(),
                fetched_at: Instant::now(),
            },
        );

        changed
            || previous_had_badge != next_had_badge
            || previous
                .as_ref()
                .is_some_and(|(previous_login, _)| previous_login.as_ref() != user_login)
    }

    async fn fetch_bttv_pro_badge_socket(
        &self,
        channel_login: &str,
        user_id: &str,
    ) -> Result<Option<Arc<str>>, String> {
        let channel_id = crate::backend::twitch_api::get_user_id_cached(channel_login).await?;
        let channel_name = format!("twitch:{channel_id}");
        let _permit = BTTV_PRO_LOOKUP_LIMIT
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "BTTV Pro lookup semaphore closed".to_string())?;

        let (mut socket, _) = crate::backend::ws::connect_websocket(BTTV_PRO_SOCKET_URL).await?;

        socket
            .send(Message::text(
                json!({
                    "name": "join_channel",
                    "data": { "name": channel_name }
                })
                .to_string(),
            ))
            .await
            .map_err(|error| format!("BTTV Pro join_channel send failed: {error}"))?;
        socket
            .send(Message::text(
                json!({
                    "name": "broadcast_me",
                    "data": {
                        "provider": "twitch",
                        "providerId": user_id,
                        "channel": channel_name,
                    }
                })
                .to_string(),
            ))
            .await
            .map_err(|error| format!("BTTV Pro broadcast_me send failed: {error}"))?;

        let target_channel = channel_name.clone();
        let target_user_id = user_id.to_string();
        tokio::time::timeout(BTTV_PRO_SOCKET_TIMEOUT, async move {
            while let Some(message) = socket.next().await {
                let message =
                    message.map_err(|error| format!("BTTV Pro socket receive failed: {error}"))?;
                match message {
                    Message::Text(text) => {
                        let event: BttvSocketEvent =
                            serde_json::from_str(&text).map_err(|error| {
                                format!("BTTV Pro socket JSON parse failed: {error}")
                            })?;
                        if event.name != "lookup_user" {
                            continue;
                        }

                        let lookup: BttvLookupUserEvent = serde_json::from_value(event.data)
                            .map_err(|error| {
                                format!("BTTV Pro lookup payload parse failed: {error}")
                            })?;
                        if lookup.provider_id != target_user_id || lookup.channel != target_channel
                        {
                            continue;
                        }

                        let badge_url = lookup.badge.and_then(|badge| {
                            let url = badge.url.trim();
                            (!url.is_empty()).then(|| Arc::<str>::from(url))
                        });
                        return Ok(badge_url);
                    }
                    Message::Ping(payload) => {
                        socket
                            .send(Message::Pong(payload))
                            .await
                            .map_err(|error| format!("BTTV Pro pong send failed: {error}"))?;
                    }
                    Message::Close(_) => {
                        return Err("BTTV Pro socket closed before lookup completed".to_string());
                    }
                    _ => {}
                }
            }

            Err("BTTV Pro socket ended before lookup completed".to_string())
        })
        .await
        .map_err(|_| "BTTV Pro socket lookup timed out".to_string())?
    }

    fn reapply_cached_bttv_pro_badges(
        &self,
        next_urls: &mut HashMap<String, Arc<str>>,
        next_by_login: &mut HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
    ) {
        for entry in self.bttv_pro_users.iter() {
            if entry.fetched_at.elapsed() > BTTV_PRO_USER_TTL {
                continue;
            }
            let Some(url) = entry.badge_url.as_ref() else {
                continue;
            };
            insert_external_badge(
                next_urls,
                next_by_login,
                bttv_pro_badge_cache_key(),
                url,
                [entry.login.to_string()].iter(),
            );
        }
    }

    fn set_external_badge_url(&self, key: &str, url: &Arc<str>) -> bool {
        let changed = self
            .external_badge_urls
            .get(key)
            .is_none_or(|existing| existing.as_ref() != url.as_ref());
        if changed {
            self.external_badge_urls
                .insert(key.to_string(), Arc::clone(url));
        }
        changed
    }

    fn add_external_badge_assignment(&self, login: &str, key: &str) -> bool {
        let login = login.trim().trim_start_matches('@').to_ascii_lowercase();
        if login.is_empty() {
            return false;
        }

        let key_arc: Arc<str> = Arc::from(key);
        let mut badges = self.external_by_login.entry(login).or_default();
        if badges.iter().any(|existing| existing.as_ref() == key) {
            return false;
        }
        badges.push(key_arc);
        true
    }

    fn remove_external_badge_assignment(&self, login: &str, key: &str) -> bool {
        let login = login.trim().trim_start_matches('@').to_ascii_lowercase();
        if login.is_empty() {
            return false;
        }

        let Some(mut badges) = self.external_by_login.get_mut(&login) else {
            return false;
        };

        let before = badges.len();
        badges.retain(|existing| existing.as_ref() != key);
        before != badges.len()
    }
}

#[derive(Deserialize)]
struct BadgesResponse {
    data: Vec<BadgeSet>,
}

#[derive(Deserialize)]
struct BadgeSet {
    set_id: String,
    versions: Vec<BadgeVersion>,
}

#[derive(Deserialize)]
struct BadgeVersion {
    id: String,
    image_url_4x: String,
}

#[derive(Deserialize)]
struct FfzBadgesResponse {
    badges: Vec<FfzBadge>,
    #[serde(default)]
    users: HashMap<String, Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct FfzBadge {
    id: u64,
    name: String,
    image: Option<String>,
    #[serde(default)]
    urls: HashMap<String, String>,
}

#[derive(Deserialize)]
struct BttvBadgeAssignment {
    name: String,
    badge: BttvBadge,
}

#[derive(Deserialize)]
struct BttvBadge {
    #[serde(rename = "type")]
    badge_type: u64,
    description: String,
    svg: String,
}

#[derive(Deserialize)]
struct BttvSocketEvent {
    name: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct BttvLookupUserEvent {
    #[serde(rename = "providerId")]
    provider_id: String,
    channel: String,
    badge: Option<BttvSocketBadge>,
}

#[derive(Deserialize)]
struct BttvSocketBadge {
    url: String,
}

/// Which FFZ room-authority artwork slot a badge key can replace.
enum FfzAuthoritySlot {
    Moderator,
    Vip,
}

/// Pure &str precondition for the FFZ authority probes: only moderator and VIP
/// badge keys can ever have room replacements, so every other badge must
/// reject before the channel-key allocation and map probe. Moderator keeps
/// precedence over the VIP name match, mirroring the original lookup order.
fn ffz_authority_slot(badge_key: &str) -> Option<FfzAuthoritySlot> {
    if is_moderator_badge_key(badge_key) {
        Some(FfzAuthoritySlot::Moderator)
    } else if badge_key_name(badge_key).eq_ignore_ascii_case("vip") {
        Some(FfzAuthoritySlot::Vip)
    } else {
        None
    }
}

fn convert_badges(response: BadgesResponse) -> HashMap<String, String> {
    let mut badges = HashMap::new();
    for set in response.data {
        for version in set.versions {
            let key = format!("{}:{}", set.set_id, version.id);
            badges.insert(key, version.image_url_4x);
        }
    }
    badges
}

fn ingest_ffz_badges(
    response: FfzBadgesResponse,
    next_urls: &mut HashMap<String, Arc<str>>,
    next_by_user_id: &mut HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
    next_badge_keys_by_id: &mut HashMap<u64, Arc<str>>,
) {
    for badge in response.badges {
        let key = ffz_badge_cache_key(&badge.name);
        let key_arc: Arc<str> = Arc::from(key.as_str());
        next_badge_keys_by_id.insert(badge.id, Arc::clone(&key_arc));

        let Some(url) = preferred_ffz_badge_url(&badge) else {
            continue;
        };
        next_urls.insert(key.clone(), Arc::from(url.as_str()));

        if let Some(users) = response.users.get(&badge.id.to_string()) {
            for user in users {
                let user_id = user
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| user.as_u64().map(|id| id.to_string()));
                let Some(user_id) = user_id.filter(|id| !id.is_empty()) else {
                    continue;
                };
                let badges = next_by_user_id.entry(user_id).or_default();
                if !badges.iter().any(|existing| existing.as_ref() == key) {
                    badges.push(Arc::clone(&key_arc));
                }
            }
        }
    }
}

fn extend_unique_badge_keys<'a>(
    resolved: &mut Vec<String>,
    badges: impl IntoIterator<Item = &'a Arc<str>>,
) {
    for badge in badges {
        push_unique_badge_key(resolved, badge.as_ref());
    }
}

fn push_unique_badge_key(resolved: &mut Vec<String>, badge_key: &str) {
    if !resolved.iter().any(|existing| existing == badge_key) {
        resolved.push(badge_key.to_string());
    }
}

fn ingest_bttv_badges(
    response: Vec<BttvBadgeAssignment>,
    next_urls: &mut HashMap<String, Arc<str>>,
    next_by_login: &mut HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
) {
    for assignment in response {
        let key = bttv_badge_cache_key(assignment.badge.badge_type, &assignment.badge.description);
        let url = normalize_external_badge_url(&assignment.badge.svg);
        insert_external_badge(
            next_urls,
            next_by_login,
            &key,
            &url,
            [assignment.name].iter(),
        );
    }
}

fn insert_external_badge<'a>(
    next_urls: &mut HashMap<String, Arc<str>>,
    next_by_login: &mut HashMap<String, smallvec::SmallVec<[Arc<str>; 2]>>,
    key: &str,
    url: &str,
    logins: impl IntoIterator<Item = &'a String>,
) {
    let key_arc: Arc<str> = Arc::from(key);
    let url_arc: Arc<str> = Arc::from(url);
    next_urls.insert(key.to_string(), Arc::clone(&url_arc));

    for login in logins {
        let login = login.trim().trim_start_matches('@').to_ascii_lowercase();
        if login.is_empty() {
            continue;
        }

        let badges = next_by_login.entry(login).or_default();
        if !badges.iter().any(|existing| existing.as_ref() == key) {
            badges.push(Arc::clone(&key_arc));
        }
    }
}

fn preferred_ffz_badge_url(badge: &FfzBadge) -> Option<String> {
    badge
        .urls
        .get("4")
        .or_else(|| badge.urls.get("2"))
        .or_else(|| badge.urls.get("1"))
        .or(badge.image.as_ref())
        .map(|url| normalize_external_badge_url(url))
}

fn normalize_external_badge_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

/// Embedded fallback badges for common Twitch badges.
/// These are loaded when network requests fail
const EMBEDDED_GLOBAL_BADGES: &[(&str, &str)] = &[
    (
        "broadcaster:1",
        "https://static-cdn.jtvnw.net/badges/v1/5527c58c-fb7d-422d-b71b-f309dcb85cc1/3",
    ),
    (
        "moderator:1",
        "https://static-cdn.jtvnw.net/badges/v1/3267646d-33f0-4b17-b3df-f923a41db1d0/3",
    ),
    (
        "vip:1",
        "https://static-cdn.jtvnw.net/badges/v1/b817aba4-fad8-49e2-b88a-7cc744f92f10/3",
    ),
    (
        "partner:1",
        "https://static-cdn.jtvnw.net/badges/v1/d12a2e27-16f6-41d0-ab77-b780518f00a3/3",
    ),
    (
        "staff:1",
        "https://static-cdn.jtvnw.net/badges/v1/d97c37bd-a6f5-4c38-8f57-4e4bef88af34/3",
    ),
    (
        "admin:1",
        "https://static-cdn.jtvnw.net/badges/v1/9ef7e029-4cdf-4d4d-a0d5-e2b3fb2571a1/3",
    ),
    (
        "global_mod:1",
        "https://static-cdn.jtvnw.net/badges/v1/9384c43e-4ce7-4e94-b2a1-b93656896eba/3",
    ),
    (
        "turbo:1",
        "https://static-cdn.jtvnw.net/badges/v1/bd444ec6-8f34-4bf9-91f4-af1e3428d80f/3",
    ),
    (
        "premium:1",
        "https://static-cdn.jtvnw.net/badges/v1/bbbe0db0-a598-423e-86d0-f9fb98ca1933/3",
    ),
    (
        "subscriber:0",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:1",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:3",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:6",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:12",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:24",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "subscriber:36",
        "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/3",
    ),
    (
        "founder:0",
        "https://static-cdn.jtvnw.net/badges/v1/511b78a9-ab37-472f-9569-457753bbe7d3/3",
    ),
    (
        "sub-gifter:1",
        "https://static-cdn.jtvnw.net/badges/v1/f1d8486f-eb2e-4553-b44f-4d614617f18f/3",
    ),
    (
        "sub-gifter:5",
        "https://static-cdn.jtvnw.net/badges/v1/3e638e02-b765-4070-81bd-a73d1ae34b7f/3",
    ),
    (
        "sub-gifter:10",
        "https://static-cdn.jtvnw.net/badges/v1/bffca343-9d7d-49b4-a1ca-90af2c6a1639/3",
    ),
    (
        "sub-gifter:25",
        "https://static-cdn.jtvnw.net/badges/v1/17e09e26-2528-4a04-9c7f-8f8578d77487/3",
    ),
    (
        "sub-gifter:50",
        "https://static-cdn.jtvnw.net/badges/v1/47308ed4-c979-4f3f-ad20-35a8ab76d85d/3",
    ),
    (
        "sub-gifter:100",
        "https://static-cdn.jtvnw.net/badges/v1/5056c366-7299-4b3c-a15a-a18573650bfb/3",
    ),
    (
        "sub-gifter:250",
        "https://static-cdn.jtvnw.net/badges/v1/df25dded-df81-408e-a2d3-40d48f0d529f/3",
    ),
    (
        "sub-gifter:500",
        "https://static-cdn.jtvnw.net/badges/v1/f440decb-7468-4bf9-8666-98ba74f6eab5/3",
    ),
    (
        "sub-gifter:1000",
        "https://static-cdn.jtvnw.net/badges/v1/b8c76744-c7e9-44be-90d0-08840a8f6e39/3",
    ),
    (
        "bits:1",
        "https://static-cdn.jtvnw.net/badges/v1/73b5c3fb-24f9-4a82-a852-2f475b59411c/3",
    ),
    (
        "bits:100",
        "https://static-cdn.jtvnw.net/badges/v1/09d93036-e7ce-431c-9a9e-7044297133f2/3",
    ),
    (
        "bits:1000",
        "https://static-cdn.jtvnw.net/badges/v1/0d85a29e-79ad-4c63-a285-3acd2c66f2ba/3",
    ),
    (
        "bits:5000",
        "https://static-cdn.jtvnw.net/badges/v1/57cd97fc-3e9e-4c6d-9d41-60147137234e/3",
    ),
    (
        "bits:10000",
        "https://static-cdn.jtvnw.net/badges/v1/68af213b-a771-4124-b6e3-9bb6d98aa732/3",
    ),
    (
        "bits:25000",
        "https://static-cdn.jtvnw.net/badges/v1/64ca5920-c663-4bd8-bfb1-751b4caea2dd/3",
    ),
    (
        "bits:50000",
        "https://static-cdn.jtvnw.net/badges/v1/62310ba7-9571-4b07-a244-d3916e3a798c/3",
    ),
    (
        "bits:75000",
        "https://static-cdn.jtvnw.net/badges/v1/ce491fa4-b82c-4f3d-a7f2-5823b69c289c/3",
    ),
    (
        "bits:100000",
        "https://static-cdn.jtvnw.net/badges/v1/96f0540f-aa63-49e1-a8b3-259ece3bd098/3",
    ),
    (
        "bits:200000",
        "https://static-cdn.jtvnw.net/badges/v1/4a0b90c4-e4ef-407f-84fe-36b14aebdfc6/3",
    ),
    (
        "bits:300000",
        "https://static-cdn.jtvnw.net/badges/v1/ac13372d-2e94-41d1-ae11-ecd677f69bb6/3",
    ),
    (
        "bits:400000",
        "https://static-cdn.jtvnw.net/badges/v1/a8f393af-76e6-4aa2-9dd0-7dcc1c34f036/3",
    ),
    (
        "bits:500000",
        "https://static-cdn.jtvnw.net/badges/v1/f6932b57-6a6e-4062-a770-dfbd9f4302e5/3",
    ),
    (
        "bits:600000",
        "https://static-cdn.jtvnw.net/badges/v1/4d908059-f91c-4aef-9f24-f9361ae86a12/3",
    ),
    (
        "bits:700000",
        "https://static-cdn.jtvnw.net/badges/v1/a1d2a824-f216-4b9f-9642-3de8ed370957/3",
    ),
    (
        "bits:800000",
        "https://static-cdn.jtvnw.net/badges/v1/5ec2ee3e-5633-4c2a-8e77-77473fe409e6/3",
    ),
    (
        "bits:900000",
        "https://static-cdn.jtvnw.net/badges/v1/088c58c6-7c38-45ba-8f73-63ef24189b84/3",
    ),
    (
        "bits:1000000",
        "https://static-cdn.jtvnw.net/badges/v1/494d1c8e-c3b2-4d88-8528-baff57c9bd3f/3",
    ),
    (
        "predictions:blue-1",
        "https://static-cdn.jtvnw.net/badges/v1/e33d8b46-f63b-4e67-996f-4f4266e66fcd/3",
    ),
    (
        "predictions:pink-1",
        "https://static-cdn.jtvnw.net/badges/v1/4b76d5f2-91cc-4400-adf2-908a1e6cfd1e/3",
    ),
    (
        "predictions:gray-1",
        "https://static-cdn.jtvnw.net/badges/v1/c2e85edf-efc2-4290-8112-d057d44d8c07/3",
    ),
    (
        "predictions:blue-2",
        "https://static-cdn.jtvnw.net/badges/v1/e33d8b46-f63b-4e67-996f-4f4266e66fcd/3",
    ),
    (
        "predictions:pink-2",
        "https://static-cdn.jtvnw.net/badges/v1/4b76d5f2-91cc-4400-adf2-908a1e6cfd1e/3",
    ),
    (
        "predictions:gray-2",
        "https://static-cdn.jtvnw.net/badges/v1/c2e85edf-efc2-4290-8112-d057d44d8c07/3",
    ),
    (
        "artist-badge:1",
        "https://static-cdn.jtvnw.net/badges/v1/4300a897-03dc-4e83-8c0e-c332fee7057f/3",
    ),
    (
        "hype-train:1",
        "https://static-cdn.jtvnw.net/badges/v1/fae4086c-3190-44d4-83c8-8ef0cbe1a515/3",
    ),
    (
        "hype-train:2",
        "https://static-cdn.jtvnw.net/badges/v1/9c8d038a-3a29-45ea-96d4-5031fb1a7a81/3",
    ),
];

fn get_embedded_global_badges() -> HashMap<String, String> {
    let mut badges: HashMap<String, String> = EMBEDDED_GLOBAL_BADGES
        .iter()
        .map(|(key, url)| (key.to_string(), url.to_string()))
        .collect();
    if let Some(url) = embedded_twitch_badge_fallback_url(NO_AUDIO_BADGE_KEY) {
        badges.insert(NO_AUDIO_BADGE_KEY.to_string(), url.to_string());
    }
    if let Some(url) = embedded_twitch_badge_fallback_url(NO_VIDEO_BADGE_KEY) {
        badges.insert(NO_VIDEO_BADGE_KEY.to_string(), url.to_string());
    }

    crate::log_cache!(info, "Using {} embedded fallback badges", badges.len());
    badges
}
