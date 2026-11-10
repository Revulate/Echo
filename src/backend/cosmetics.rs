//! Runtime cache for 7TV user cosmetics.
//!
//! The parser/resolver lives in `cosmetics_core`; this module owns IO,
//! throttling, negative caching, and sync reads for hot render paths.

use super::cosmetics_core::{
    ResolvedUserCosmetics, SevenTvCosmeticsCatalog, SevenTvUserProfile,
    parse_cosmetics_catalog_json, parse_paints_v4_json, parse_user_profile_json,
    resolve_user_cosmetics, unknown_cosmetic_ids,
};
use super::http;
use dashmap::{DashMap, DashSet};
use futures_util::stream::{self, StreamExt};
use reqwest::StatusCode;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore};

const SEVENTV_USER_URL: &str = "https://7tv.io/v3/users/twitch";
const SEVENTV_GQL_URL: &str = "https://7tv.io/v3/gql";
const SEVENTV_GQL_V4_URL: &str = "https://7tv.io/v4/gql";
const USER_TTL: Duration = Duration::from_secs(30 * 60);
const USERS_MAX: usize = 2_000;
const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);
/// A profile selecting a paint or badge the cached catalog lacks (published
/// after the last fetch) refreshes the catalog early, at most this often.
const CATALOG_FORCED_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// 7TV re-announces entitlements whenever a chatter's presence refreshes, so
/// entitlement-driven refetches are spaced per user and budgeted globally.
const ENTITLEMENT_REFETCH_INTERVAL: Duration = Duration::from_secs(60);
const ENTITLEMENT_REFETCH_WINDOW: Duration = Duration::from_secs(60);
const ENTITLEMENT_REFETCHES_PER_WINDOW: u32 = 30;
/// Bound provider pressure across overlapping canvas scans while avoiding one
/// full provider round trip per sender in series. A canvas scan can contribute
/// up to 64 unique users, so serial fetches visibly trail chat.
const VISIBLE_USER_FETCH_CONCURRENCY: usize = 8;
static VISIBLE_USER_FETCH_PERMITS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(VISIBLE_USER_FETCH_CONCURRENCY));

pub static SEVENTV_COSMETICS_CACHE: LazyLock<SevenTvCosmeticsCache> =
    LazyLock::new(SevenTvCosmeticsCache::new);

struct UserCacheEntry {
    cosmetics: Option<ResolvedUserCosmetics>,
    /// The paint/badge ids the 7TV profile selects, kept even when the
    /// catalog cannot resolve them, so entitlement events compare against
    /// the profile rather than the resolution.
    selected_paint_id: Option<String>,
    selected_badge_id: Option<String>,
    fetched_at: Instant,
    /// `USER_TTL`, or `CATALOG_FORCED_REFRESH_INTERVAL` while the profile
    /// selects a paint/badge the catalog lacks that no forced refresh has
    /// confirmed missing yet, so a visible user retries once the next early
    /// catalog refresh can be claimed.
    ttl: Duration,
    last_used_ms: AtomicU64,
}

#[derive(Clone)]
struct CatalogCacheEntry {
    catalog: Arc<SevenTvCosmeticsCatalog>,
    fetched_at: Instant,
}

struct QueuedCosmeticUser {
    user_id: Arc<str>,
    username: Arc<str>,
    channels: HashSet<Arc<str>>,
}

#[derive(Clone)]
struct ObservedCosmeticUser {
    username: Arc<str>,
    channels: HashSet<Arc<str>>,
}

pub struct SevenTvCosmeticsCache {
    users: DashMap<Arc<str>, UserCacheEntry>,
    seventv_user_map: DashMap<String, HashSet<String>>,
    observed_users: DashMap<Arc<str>, ObservedCosmeticUser>,
    loading_users: DashSet<Arc<str>>,
    catalog: RwLock<Option<CatalogCacheEntry>>,
    catalog_fetch_lock: Mutex<()>,
    catalog_forced_refresh_at: parking_lot::Mutex<Option<Instant>>,
    /// Paint/badge ids a forced catalog refresh confirmed missing, with the
    /// confirmation time. They do not claim another forced refresh until
    /// `CATALOG_TTL` has passed.
    confirmed_missing_ids: DashMap<Box<str>, Instant>,
    entitlement_refetch_at: DashMap<Arc<str>, Instant>,
    /// Latest entitlement change per user that the rate limits or an
    /// in-flight load dropped; one delayed retry re-evaluates it.
    entitlement_retries: DashMap<Arc<str>, (CosmeticEntitlementKind, Box<str>, bool)>,
    /// `(window start, refetches in window)` across all users.
    entitlement_refetch_budget: parking_lot::Mutex<(Instant, u32)>,
    client: http::ClientCache,
}

/// Bounded so the catalog single-flight lock is never held across a stalled
/// request: a stalled read fails fast, while the total cap leaves room for the
/// full paint catalog over a slow proxy.
fn build_cosmetics_http_client() -> reqwest::Client {
    const TOTAL: std::time::Duration = std::time::Duration::from_secs(60);
    http::client_builder()
        .timeout(TOTAL)
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|error| {
            tracing::error!("Failed to create 7TV cosmetics HTTP client: {error}");
            http::timed_client(TOTAL)
        })
}

impl SevenTvCosmeticsCache {
    pub fn new() -> Self {
        Self {
            users: DashMap::new(),
            seventv_user_map: DashMap::new(),
            observed_users: DashMap::new(),
            loading_users: DashSet::new(),
            catalog: RwLock::new(None),
            catalog_fetch_lock: Mutex::new(()),
            catalog_forced_refresh_at: parking_lot::Mutex::new(None),
            confirmed_missing_ids: DashMap::new(),
            entitlement_refetch_at: DashMap::new(),
            entitlement_retries: DashMap::new(),
            entitlement_refetch_budget: parking_lot::Mutex::new((Instant::now(), 0)),
            client: http::ClientCache::new(build_cosmetics_http_client),
        }
    }
}

impl Default for SevenTvCosmeticsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SevenTvCosmeticsCache {
    /// Expired entries keep serving until the visibility-driven refetch
    /// replaces them, so a row never loses its paint or badge in between.
    pub fn get_cached_user(&self, twitch_user_id: &str) -> Option<ResolvedUserCosmetics> {
        let entry = self.users.get(twitch_user_id)?;
        entry
            .last_used_ms
            .store(crate::utils::unix_now_ms(), Ordering::Relaxed);
        entry.cosmetics.clone()
    }

    fn prune_least_recently_used_users(&self) {
        if self.users.len() < USERS_MAX {
            return;
        }
        let mut by_use: Vec<(u64, Arc<str>)> = self
            .users
            .iter()
            .map(|entry| {
                (
                    entry.last_used_ms.load(Ordering::Relaxed),
                    Arc::clone(entry.key()),
                )
            })
            .collect();
        let prune_count = (USERS_MAX / 10).min(by_use.len());
        if prune_count == 0 {
            return;
        }
        by_use.select_nth_unstable_by_key(prune_count - 1, |(last_used, _)| *last_used);
        for (_, key) in by_use.into_iter().take(prune_count) {
            self.users.remove(&key);
            super::emotes::personal::forget_user(&key);
        }
    }

    fn should_queue_user(&self, twitch_user_id: &str) -> bool {
        if twitch_user_id.trim().is_empty() {
            return false;
        }
        if self
            .users
            .get(twitch_user_id)
            .is_some_and(|entry| entry.fetched_at.elapsed() <= entry.ttl)
        {
            return false;
        }
        self.loading_users.insert(Arc::from(twitch_user_id))
    }

    fn record_observed_user(
        &self,
        user_id: Arc<str>,
        username: Arc<str>,
        channels: &HashSet<Arc<str>>,
    ) {
        const OBSERVED_USERS_MAX: usize = 2_000;
        if self.observed_users.len() >= OBSERVED_USERS_MAX {
            let prune_count = OBSERVED_USERS_MAX / 10;
            let to_remove: Vec<Arc<str>> = self
                .observed_users
                .iter()
                .take(prune_count)
                .map(|r| Arc::clone(r.key()))
                .collect();
            for key in to_remove {
                self.observed_users.remove(&key);
            }
        }
        self.observed_users
            .entry(Arc::clone(&user_id))
            .and_modify(|observed| {
                if observed.username.is_empty() && !username.is_empty() {
                    observed.username = Arc::clone(&username);
                }
                observed.channels.extend(channels.iter().cloned());
            })
            .or_insert_with(|| ObservedCosmeticUser {
                username,
                channels: channels.clone(),
            });
    }

    fn remember_seventv_mapping(&self, seventv_user_id: &str, twitch_user_id: &str) {
        if seventv_user_id.trim().is_empty() || twitch_user_id.trim().is_empty() {
            return;
        }

        const SEVENTV_USER_MAP_MAX: usize = 2_000;
        if self.seventv_user_map.len() >= SEVENTV_USER_MAP_MAX {
            let prune_count = SEVENTV_USER_MAP_MAX / 10;
            let to_remove: Vec<String> = self
                .seventv_user_map
                .iter()
                .take(prune_count)
                .map(|r| r.key().clone())
                .collect();
            for key in to_remove {
                self.seventv_user_map.remove(&key);
            }
        }

        let mut mapped = self
            .seventv_user_map
            .entry(seventv_user_id.to_string())
            .or_default();
        mapped.insert(twitch_user_id.to_string());
    }

    fn force_reload_requests_for_seventv_user(
        &self,
        seventv_user_id: &str,
    ) -> Vec<QueuedCosmeticUser> {
        let Some(twitch_ids) = self
            .seventv_user_map
            .get(seventv_user_id)
            .map(|entry| entry.iter().cloned().collect::<Vec<_>>())
        else {
            return Vec::new();
        };

        let mut queued = Vec::new();
        for twitch_user_id in twitch_ids {
            let Some(observed) = self
                .observed_users
                .get(twitch_user_id.as_str())
                .map(|entry| entry.clone())
            else {
                continue;
            };

            if self
                .loading_users
                .insert(Arc::from(twitch_user_id.as_str()))
            {
                queued.push(QueuedCosmeticUser {
                    user_id: Arc::from(twitch_user_id),
                    username: observed.username,
                    channels: observed.channels,
                });
            }
        }

        queued
    }

    async fn load_users(&self, users: Vec<QueuedCosmeticUser>) {
        let batch_size = users.len();
        let batch_started = Instant::now();
        let catalog = match self.load_catalog().await {
            Ok(catalog) => catalog,
            Err(error) => {
                crate::log_cache!(warn, "Failed to load 7TV cosmetics catalog: {}", error);
                for user in users {
                    self.loading_users.remove(user.user_id.as_ref());
                }
                return;
            }
        };

        for user in &users {
            self.record_observed_user(
                Arc::clone(&user.user_id),
                Arc::clone(&user.username),
                &user.channels,
            );
        }

        let mut pending = stream::iter(users.into_iter().map(|user| async move {
            let previous = self
                .users
                .get(&user.user_id)
                .and_then(|entry| entry.cosmetics.clone());
            let _permit = VISIBLE_USER_FETCH_PERMITS
                .acquire()
                .await
                .expect("visible-user cosmetics semaphore is never closed");
            let result = self.fetch_user_profile(&user.user_id).await;
            (user, previous, result)
        }))
        .buffer_unordered(VISIBLE_USER_FETCH_CONCURRENCY);

        let mut resolved_visible = 0usize;
        let mut unknown_cosmetics = Vec::new();
        while let Some((user, previous, result)) = pending.next().await {
            let (published_visible_state, unknown_profile) = match result {
                Ok(profile) => {
                    let cosmetics = profile
                        .as_ref()
                        .map(|profile| resolve_user_cosmetics(profile, &catalog));
                    if let Some(cosmetics) = cosmetics.as_ref() {
                        self.remember_seventv_mapping(&cosmetics.user_id, &user.user_id);
                    }
                    super::emotes::personal::note_user_personal_set(
                        &user.user_id,
                        cosmetics
                            .as_ref()
                            .and_then(|cosmetics| cosmetics.personal_emote_set_id.as_deref()),
                        user.channels.iter().cloned().collect(),
                    );
                    if cosmetics.as_ref().is_some_and(has_visible_cosmetics) {
                        resolved_visible += 1;
                    }
                    let needs_catalog_refresh = profile
                        .as_ref()
                        .is_some_and(|profile| self.has_unconfirmed_unknown_ids(profile, &catalog));
                    (
                        self.store_user_cosmetics(
                            &user.user_id,
                            previous,
                            cosmetics,
                            profile.as_ref(),
                            needs_catalog_refresh,
                        ),
                        profile.filter(|_| needs_catalog_refresh),
                    )
                }
                Err(error) => {
                    crate::log_cache!(
                        debug,
                        "Failed to load 7TV cosmetics for {} ({}): {}",
                        user.username,
                        user.user_id,
                        error
                    );
                    (None, None)
                }
            };
            self.loading_users.remove(user.user_id.as_ref());

            if let Some(next_visible) = published_visible_state {
                publish_user_decorations(&user, next_visible, batch_started);
            }
            if let Some(profile) = unknown_profile {
                unknown_cosmetics.push((user, profile));
            }
        }

        crate::log_cache!(
            debug,
            users = batch_size,
            visible = resolved_visible,
            concurrency = VISIBLE_USER_FETCH_CONCURRENCY,
            elapsed_ms = batch_started.elapsed().as_millis() as u64,
            "Loaded visible 7TV cosmetics batch"
        );

        if !unknown_cosmetics.is_empty() {
            self.resolve_against_refreshed_catalog(unknown_cosmetics, batch_started)
                .await;
        }
    }

    /// Cache freshly resolved cosmetics. Returns `Some(next_visible)` when the
    /// visible paint/badge state changed and the user's rows must relayout.
    fn store_user_cosmetics(
        &self,
        user_id: &Arc<str>,
        previous: Option<ResolvedUserCosmetics>,
        cosmetics: Option<ResolvedUserCosmetics>,
        profile: Option<&SevenTvUserProfile>,
        retry_after_catalog_refresh: bool,
    ) -> Option<bool> {
        let previous_visible = previous.as_ref().is_some_and(has_visible_cosmetics);
        let next_visible = cosmetics.as_ref().is_some_and(has_visible_cosmetics);
        let changed = previous != cosmetics;
        if !self.users.contains_key(user_id) {
            self.prune_least_recently_used_users();
        }
        self.users.insert(
            Arc::clone(user_id),
            UserCacheEntry {
                cosmetics,
                selected_paint_id: profile.and_then(|profile| profile.style.paint_id.clone()),
                selected_badge_id: profile.and_then(|profile| profile.style.badge_id.clone()),
                fetched_at: Instant::now(),
                ttl: if retry_after_catalog_refresh {
                    CATALOG_FORCED_REFRESH_INTERVAL
                } else {
                    USER_TTL
                },
                last_used_ms: AtomicU64::new(crate::utils::unix_now_ms()),
            },
        );
        (changed && (previous_visible || next_visible)).then_some(next_visible)
    }

    /// Re-resolve profiles whose paint or badge id is missing from the cached
    /// catalog. The catalog refetch bypasses `CATALOG_TTL` at most once per
    /// `CATALOG_FORCED_REFRESH_INTERVAL`; inside that interval the profiles
    /// re-resolve against the cached catalog, and users still unresolved keep
    /// the short entry TTL so a visible one retries after the interval.
    ///
    /// The claim and the forced fetch run under `catalog_fetch_lock`, so a
    /// batch that loses the claim waits for an in-flight refresh and then
    /// reads its result, never the catalog it replaces; a failed refresh does
    /// not make the waiters fetch again.
    async fn resolve_against_refreshed_catalog(
        &self,
        stale: Vec<(QueuedCosmeticUser, SevenTvUserProfile)>,
        batch_started: Instant,
    ) {
        let (catalog, forced) = {
            let _fetch_guard = self.catalog_fetch_lock.lock().await;
            if self.claim_forced_catalog_refresh() {
                crate::log_cache!(
                    debug,
                    users = stale.len(),
                    "Refreshing 7TV cosmetics catalog for unknown paint/badge ids"
                );
                (self.fetch_catalog_locked().await, true)
            } else {
                match self.cached_catalog().await {
                    Some(catalog) => (Ok(catalog), false),
                    None => (self.fetch_catalog_locked().await, false),
                }
            }
        };
        let catalog = match catalog {
            Ok(catalog) => catalog,
            Err(error) => {
                crate::log_cache!(warn, "Failed to refresh 7TV cosmetics catalog: {}", error);
                return;
            }
        };
        if forced {
            self.confirm_missing_ids(stale.iter().map(|(_, profile)| profile), &catalog);
        }

        for (user, profile) in stale {
            let previous = self
                .users
                .get(&user.user_id)
                .and_then(|entry| entry.cosmetics.clone());
            let cosmetics = Some(resolve_user_cosmetics(&profile, &catalog));
            let retry = self.has_unconfirmed_unknown_ids(&profile, &catalog);
            if let Some(next_visible) =
                self.store_user_cosmetics(&user.user_id, previous, cosmetics, Some(&profile), retry)
            {
                publish_user_decorations(&user, next_visible, batch_started);
            }
        }
    }

    /// Whether the profile selects a paint/badge id the catalog lacks that
    /// no forced refresh has confirmed missing within `CATALOG_TTL`.
    fn has_unconfirmed_unknown_ids(
        &self,
        profile: &SevenTvUserProfile,
        catalog: &SevenTvCosmeticsCatalog,
    ) -> bool {
        let now = Instant::now();
        unknown_cosmetic_ids(profile, catalog).any(|id| {
            !self
                .confirmed_missing_ids
                .get(id)
                .is_some_and(|at| now.duration_since(*at) < CATALOG_TTL)
        })
    }

    /// Record the ids a just-forced catalog refresh still lacks, so they stop
    /// claiming forced refreshes until the catalog would expire anyway.
    fn confirm_missing_ids<'a>(
        &self,
        profiles: impl Iterator<Item = &'a SevenTvUserProfile>,
        catalog: &SevenTvCosmeticsCatalog,
    ) {
        let now = Instant::now();
        self.confirmed_missing_ids
            .retain(|_, at| now.duration_since(*at) < CATALOG_TTL);
        for profile in profiles {
            for id in unknown_cosmetic_ids(profile, catalog) {
                if self.confirmed_missing_ids.len() >= USERS_MAX {
                    return;
                }
                self.confirmed_missing_ids.insert(Box::from(id), now);
            }
        }
    }

    /// Claim the rate-limited early catalog refresh.
    fn claim_forced_catalog_refresh(&self) -> bool {
        let now = Instant::now();
        let mut last = self.catalog_forced_refresh_at.lock();
        if last.is_some_and(|at| now.duration_since(at) < CATALOG_FORCED_REFRESH_INTERVAL) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// A refetch request for an entitlement event that disagrees with the
    /// paint/badge id an observed chatter's cached profile selects, or `None`
    /// when the event is a no-op or was deferred because the user is rate
    /// limited or already loading.
    fn entitlement_refetch_request(
        &self,
        twitch_user_id: &str,
        kind: CosmeticEntitlementKind,
        ref_id: &str,
        created: bool,
    ) -> Option<QueuedCosmeticUser> {
        let selected = {
            let entry = self.users.get(twitch_user_id)?;
            let selected_id = match kind {
                CosmeticEntitlementKind::Paint => entry.selected_paint_id.as_deref(),
                CosmeticEntitlementKind::Badge => entry.selected_badge_id.as_deref(),
            };
            selected_id == Some(ref_id)
        };
        if created == selected {
            return None;
        }
        let observed = self
            .observed_users
            .get(twitch_user_id)
            .map(|entry| entry.clone())?;
        if self.loading_users.contains(twitch_user_id)
            || !self.claim_entitlement_refetch(twitch_user_id)
            || !self.loading_users.insert(Arc::from(twitch_user_id))
        {
            self.defer_entitlement_retry(twitch_user_id, kind, ref_id, created);
            return None;
        }
        Some(QueuedCosmeticUser {
            user_id: Arc::from(twitch_user_id),
            username: observed.username,
            channels: observed.channels,
        })
    }

    /// Re-evaluate a dropped entitlement change once the per-user window,
    /// the global budget window and any in-flight load have had time to pass.
    /// At most one retry per user is pending; it reads the latest dropped
    /// event, and a load that already picked the change up makes it a no-op.
    fn defer_entitlement_retry(
        &self,
        twitch_user_id: &str,
        kind: CosmeticEntitlementKind,
        ref_id: &str,
        created: bool,
    ) {
        let user_id: Arc<str> = Arc::from(twitch_user_id);
        let pending = (kind, Box::from(ref_id), created);
        if self
            .entitlement_retries
            .insert(Arc::clone(&user_id), pending)
            .is_some()
        {
            return;
        }
        crate::backend::bridge::get_runtime().spawn(async move {
            tokio::time::sleep(ENTITLEMENT_REFETCH_INTERVAL).await;
            if let Some((_, (kind, ref_id, created))) =
                SEVENTV_COSMETICS_CACHE.entitlement_retries.remove(&user_id)
            {
                refresh_user_for_entitlement(&user_id, kind, &ref_id, created);
            }
        });
    }

    fn claim_entitlement_refetch(&self, twitch_user_id: &str) -> bool {
        let now = Instant::now();
        if self
            .entitlement_refetch_at
            .get(twitch_user_id)
            .is_some_and(|at| now.duration_since(*at) < ENTITLEMENT_REFETCH_INTERVAL)
        {
            return false;
        }
        {
            let mut budget = self.entitlement_refetch_budget.lock();
            if now.duration_since(budget.0) >= ENTITLEMENT_REFETCH_WINDOW {
                *budget = (now, 0);
            }
            if budget.1 >= ENTITLEMENT_REFETCHES_PER_WINDOW {
                return false;
            }
            budget.1 += 1;
        }
        if self.entitlement_refetch_at.len() >= USERS_MAX {
            self.entitlement_refetch_at
                .retain(|_, at| now.duration_since(*at) < ENTITLEMENT_REFETCH_INTERVAL);
        }
        self.entitlement_refetch_at
            .insert(Arc::from(twitch_user_id), now);
        true
    }

    async fn load_catalog(&self) -> Result<Arc<SevenTvCosmeticsCatalog>, String> {
        if let Some(catalog) = self.cached_catalog().await {
            return Ok(catalog);
        }

        let _fetch_guard = self.catalog_fetch_lock.lock().await;
        if let Some(catalog) = self.cached_catalog().await {
            return Ok(catalog);
        }
        self.fetch_catalog_locked().await
    }

    /// The cached catalog while it is within `CATALOG_TTL`.
    ///
    /// Shared rather than cloned: every queued user batch holds the catalog
    /// while it waits for a fetch permit, and a deep copy per batch held
    /// ~100 MB of paints at once during a busy-chat startup.
    async fn cached_catalog(&self) -> Option<Arc<SevenTvCosmeticsCatalog>> {
        self.catalog
            .read()
            .await
            .as_ref()
            .filter(|entry| entry.fetched_at.elapsed() <= CATALOG_TTL)
            .map(|entry| Arc::clone(&entry.catalog))
    }

    /// Fetch and cache the catalog. The caller holds `catalog_fetch_lock`.
    async fn fetch_catalog_locked(&self) -> Result<Arc<SevenTvCosmeticsCatalog>, String> {
        let query = r#"
            query EchoCosmeticsCatalog {
                cosmetics {
                    paints {
                        id
                        name
                        color
                        gradients {
                            angle
                            repeat
                            at
                            image_url
                            stops {
                                at
                                color
                            }
                        }
                        shadows {
                            x_offset
                            y_offset
                            radius
                            color
                        }
                        text {
                            weight
                            transform
                            variant
                            shadows {
                                x_offset
                                y_offset
                                radius
                                color
                            }
                            stroke {
                                color
                                width
                            }
                        }
                    }
                    badges {
                        id
                        name
                        tooltip
                        host {
                            url
                            files {
                                name
                                format
                                frame_count
                            }
                        }
                    }
                }
            }
        "#;

        let response = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Cache,
            self.client
                .post(SEVENTV_GQL_URL)
                .json(&serde_json::json!({ "query": query })),
        )
        .await
        .map_err(|error| format!("7TV cosmetics request failed: {error}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "7TV cosmetics request returned HTTP {}",
                response.status()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|error| format!("7TV cosmetics response read failed: {error}"))?;
        let mut catalog = parse_cosmetics_catalog_json(&body)?;

        match self.load_paints_v4().await {
            Ok(paints) if !paints.is_empty() => {
                for (id, mut paint) in paints {
                    if let Some(v3_paint) = catalog.paints.get(&id)
                        && paint.text == Default::default()
                    {
                        paint.text = v3_paint.text.clone();
                    }
                    catalog.paints.insert(id, paint);
                }
            }
            Ok(_) => {
                crate::log_cache!(warn, "7TV v4 paint catalog was empty; keeping v3 paints");
            }
            Err(error) => {
                crate::log_cache!(warn, "Failed to load 7TV v4 paint catalog: {}", error);
            }
        }

        let catalog = Arc::new(catalog);
        let mut cached = self.catalog.write().await;
        *cached = Some(CatalogCacheEntry {
            catalog: Arc::clone(&catalog),
            fetched_at: Instant::now(),
        });
        Ok(catalog)
    }

    async fn fetch_user_profile(
        &self,
        twitch_user_id: &str,
    ) -> Result<Option<SevenTvUserProfile>, String> {
        let url = format!("{SEVENTV_USER_URL}/{twitch_user_id}");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|error| format!("7TV user request failed: {error}"))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!(
                "7TV user request returned HTTP {}",
                response.status()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|error| format!("7TV user response read failed: {error}"))?;
        parse_user_profile_json(&body).map(Some)
    }

    async fn load_paints_v4(
        &self,
    ) -> Result<HashMap<String, super::cosmetics_core::SevenTvNamePaint>, String> {
        let query = r#"
            query EchoPaintsCatalogV4 {
                paints {
                    paints {
                        id
                        name
                        data {
                            layers {
                                opacity
                                ty {
                                    __typename
                                    ... on PaintLayerTypeSingleColor {
                                        color { r g b a }
                                    }
                                    ... on PaintLayerTypeLinearGradient {
                                        angle
                                        repeating
                                        stops { at color { r g b a } }
                                    }
                                    ... on PaintLayerTypeRadialGradient {
                                        repeating
                                        shape
                                        stops { at color { r g b a } }
                                    }
                                    ... on PaintLayerTypeImage {
                                        images { url scale frameCount }
                                    }
                                }
                            }
                            shadows {
                                color { r g b a }
                                offsetX
                                offsetY
                                blur
                            }
                        }
                    }
                }
            }
        "#;

        let response = crate::backend::http::send_logged(
            crate::backend::http::LogCategory::Cache,
            self.client
                .post(SEVENTV_GQL_V4_URL)
                .json(&serde_json::json!({ "query": query })),
        )
        .await
        .map_err(|error| format!("7TV v4 paints request failed: {error}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "7TV v4 paints request returned HTTP {}",
                response.status()
            ));
        }

        let body = response
            .text()
            .await
            .map_err(|error| format!("7TV v4 paints response read failed: {error}"))?;
        parse_paints_v4_json(&body)
    }
}

pub fn get_cached_user_cosmetics(twitch_user_id: &str) -> Option<ResolvedUserCosmetics> {
    SEVENTV_COSMETICS_CACHE.get_cached_user(twitch_user_id)
}

fn has_visible_cosmetics(cosmetics: &ResolvedUserCosmetics) -> bool {
    cosmetics.paint.is_some() || cosmetics.badge.is_some()
}

/// Tell the user's channels to relayout their rows for changed cosmetics.
fn publish_user_decorations(user: &QueuedCosmeticUser, next_visible: bool, batch_started: Instant) {
    crate::log_cache!(
        debug,
        visible = next_visible,
        channels = user.channels.len(),
        elapsed_ms = batch_started.elapsed().as_millis() as u64,
        "Published resolved 7TV user decorations"
    );
    for channel in &user.channels {
        crate::backend::observers::get_observer_registry().notify(
            channel.as_ref(),
            crate::backend::observers::ChannelEvent::UserDecorationsChanged {
                user_login: Arc::from(user.username.to_ascii_lowercase()),
                user_id: Some(Arc::clone(&user.user_id)),
            },
        );
    }
}

pub fn queue_visible_cosmetics_batch(users: Vec<(Arc<str>, Arc<str>, Arc<str>)>) {
    let mut requests: HashMap<Arc<str>, QueuedCosmeticUser> = HashMap::new();

    for (user_id, username, channel) in users {
        if user_id.trim().is_empty() {
            continue;
        }

        let channel_key = Arc::<str>::from(channel.to_ascii_lowercase());
        requests
            .entry(Arc::clone(&user_id))
            .and_modify(|request| {
                request.channels.insert(Arc::clone(&channel_key));
            })
            .or_insert_with(|| QueuedCosmeticUser {
                user_id,
                username,
                channels: HashSet::from([channel_key]),
            });
    }

    for request in requests.values() {
        SEVENTV_COSMETICS_CACHE.record_observed_user(
            Arc::clone(&request.user_id),
            Arc::clone(&request.username),
            &request.channels,
        );
    }

    let queued: Vec<QueuedCosmeticUser> = requests
        .into_values()
        .filter(|request| SEVENTV_COSMETICS_CACHE.should_queue_user(&request.user_id))
        .collect();

    if queued.is_empty() {
        return;
    }

    crate::backend::bridge::get_runtime().spawn(async move {
        SEVENTV_COSMETICS_CACHE.load_users(queued).await;
    });
}

pub fn refresh_live_seventv_user(seventv_user_id: &str) {
    let queued = SEVENTV_COSMETICS_CACHE.force_reload_requests_for_seventv_user(seventv_user_id);
    if queued.is_empty() {
        return;
    }

    crate::backend::bridge::get_runtime().spawn(async move {
        SEVENTV_COSMETICS_CACHE.load_users(queued).await;
    });
}

/// 7TV cosmetic kinds whose entitlement events can change a chatter's name
/// paint or badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CosmeticEntitlementKind {
    Paint,
    Badge,
}

/// Refetch an observed chatter's 7TV profile when an `entitlement.create` or
/// `entitlement.delete` for a paint or badge disagrees with the cached
/// profile, so a changed cosmetic shows without waiting for `USER_TTL`.
///
/// Presence refreshes re-announce unchanged entitlements, so the refetch runs
/// only when the event disagrees with the id the cached profile selects (a
/// created cosmetic that is not selected, or a deleted one that is; compared
/// against the raw profile id, so a paint the catalog cannot resolve yet does
/// not count as a change), at most once per user per
/// `ENTITLEMENT_REFETCH_INTERVAL`, within a global per-window budget, and never
/// alongside an in-flight load of the same user; a change dropped by those
/// limits gets one delayed retry. A profile whose new paint is
/// missing from the catalog then triggers the rate-limited catalog refresh.
pub fn refresh_user_for_entitlement(
    twitch_user_id: &str,
    kind: CosmeticEntitlementKind,
    ref_id: &str,
    created: bool,
) {
    let Some(request) =
        SEVENTV_COSMETICS_CACHE.entitlement_refetch_request(twitch_user_id, kind, ref_id, created)
    else {
        return;
    };
    crate::backend::metrics::record_emote_event("7tv.entitlement.cosmetics_refetch");
    crate::backend::bridge::get_runtime().spawn(async move {
        SEVENTV_COSMETICS_CACHE.load_users(vec![request]).await;
    });
}
