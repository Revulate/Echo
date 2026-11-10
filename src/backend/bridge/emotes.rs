//! Emote loading, caching, and categorization
//!
//! Handles emote metadata loading, image preloading, and emote picker categories.
//!
//! # Async Patterns Used
//!
//! This module uses two async patterns, each chosen for a specific boundary:
//!
//! ## 1. `RUNTIME.spawn()` - Detached async I/O
//! Used for owned, non-blocking provider work whose result is published or
//! logged inside the task. Dropping Tokio's join handle detaches the task while
//! the application-owned runtime remains alive.
//!
//! ## 2. Dedicated bounded runtimes - CPU/media isolation
//! Category rebuilding and image preloading retain their shared bounded pools;
//! they are process-owned resources, not runtimes created per request.

use super::{CachedEmoteCategories, EMOTE_CATEGORY_CACHE, Emote, EmoteCategory, RUNTIME};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

#[inline]
fn record_bridge_emote_stage(stage: &str, started_at: Instant, items: usize) {
    let elapsed = started_at.elapsed();
    crate::backend::metrics::record_emote_stage(stage, elapsed, items);
    crate::log_perf_emotes!(
        debug,
        stage = stage,
        elapsed_ms = elapsed.as_millis(),
        items,
        "Bridge emote stage"
    );
}

#[inline]
fn normalized_channel_key(channel: &str) -> Cow<'_, str> {
    if channel.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        Cow::Borrowed(channel)
    } else {
        Cow::Owned(channel.to_ascii_lowercase())
    }
}

fn categories_match_backend(
    existing: &[EmoteCategory],
    backend: &[crate::backend::emotes::EmoteCategory],
) -> bool {
    existing.len() == backend.len()
        && existing.iter().zip(backend).all(|(existing, backend)| {
            existing.emotes.len() == backend.emotes.len()
                && existing
                    .emotes
                    .iter()
                    .zip(&backend.emotes)
                    .all(|(existing, backend)| existing.matches_backend(backend))
        })
}

/// Get all emotes for a channel (including global)
pub fn get_all_emotes(channel: &str) -> Vec<Arc<Emote>> {
    crate::backend::emotes::EMOTE_CACHE
        .get_all_emotes(channel)
        .into_iter()
        .map(|emote| Arc::new(Emote::from(emote)))
        .collect()
}

/// Get categorized emotes for emote picker (NON-BLOCKING)
/// PERF: Returns Arc to avoid cloning 4000+ emotes on every emote picker open
/// The Arc makes cache hits O(1) instead of O(n) where n = number of emotes
///
/// This function NEVER blocks the main thread:
/// - Cache hit: returns immediately
/// - Cache miss: returns empty/stale data and triggers background refresh
/// - Caller should use check_for_emote_updates() to poll for new data
pub fn get_categorized_emotes(channel: &str) -> Arc<Vec<EmoteCategory>> {
    let started_at = Instant::now();
    let cache_key = normalized_channel_key(channel);

    let channel_version = crate::backend::emotes::get_emote_cache_version(channel);
    let global_version = crate::backend::emotes::get_global_emote_version();
    let user_version = crate::state::VersionRegistry::user_emote_version();
    let layout_version = crate::backend::settings::get_layout_settings_version();

    if let Some(entry) = EMOTE_CATEGORY_CACHE.get(cache_key.as_ref()) {
        if entry.channel_version == channel_version
            && entry.global_version == global_version
            && entry.user_version == user_version
            && entry.layout_version == layout_version
        {
            record_bridge_emote_stage(
                "bridge.emotes.categories.cache_hit",
                started_at,
                entry.categories.len(),
            );
            return Arc::clone(&entry.categories);
        }
        let stale_categories = Arc::clone(&entry.categories);
        trigger_category_refresh(cache_key.as_ref());
        record_bridge_emote_stage(
            "bridge.emotes.categories.cache_stale",
            started_at,
            stale_categories.len(),
        );
        return stale_categories;
    }

    trigger_category_refresh(cache_key.as_ref());
    record_bridge_emote_stage("bridge.emotes.categories.cache_miss", started_at, 0);
    Arc::new(vec![])
}

/// Trigger background refresh of emote categories (non-blocking)
///
/// Uses DashSet for lock-free tracking of in-progress refreshes.
/// RAII guard ensures cleanup even if thread panics.
fn trigger_category_refresh(cache_key: &str) {
    use crate::backend::lock_utils::DashSetKeyGuard;
    use dashmap::DashSet;
    use std::sync::LazyLock;

    static REFRESHING: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);
    static CATEGORY_REFRESH_RUNTIME: LazyLock<Option<tokio::runtime::Runtime>> =
        LazyLock::new(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("echo-emote-categories")
                .enable_all()
                .build()
                .map_err(|error| {
                    crate::log_emotes!(
                        error,
                        "Failed to create shared category refresh runtime: {}",
                        error
                    );
                    error
                })
                .ok()
        });

    let key = cache_key.to_string();

    if !REFRESHING.insert(key.clone()) {
        return;
    }

    let Some(runtime) = CATEGORY_REFRESH_RUNTIME.as_ref() else {
        REFRESHING.remove(&key);
        return;
    };

    let channel = key;
    runtime.spawn(async move {
        let refresh_start = Instant::now();
        let mut _guard = DashSetKeyGuard::new(&REFRESHING, channel.clone());

        let channel_version = crate::backend::emotes::get_emote_cache_version(&channel);
        let global_version = crate::backend::emotes::get_global_emote_version();
        let user_version = crate::state::VersionRegistry::user_emote_version();
        let layout_version = crate::backend::settings::get_layout_settings_version();

        let reusable_categories = EMOTE_CATEGORY_CACHE
            .get(&channel)
            .map(|cached| Arc::clone(&cached.categories));

        let fetch_start = Instant::now();
        let backend_categories = crate::backend::emotes::get_categorized_emotes(&channel).await;
        let categories_len = backend_categories.len();
        record_bridge_emote_stage(
            "bridge.emotes.categories.refresh_fetch",
            fetch_start,
            categories_len,
        );

        let reuse_check_start = Instant::now();
        let (arc_categories, categories_changed) = if reusable_categories
            .as_deref()
            .is_some_and(|existing| categories_match_backend(existing, &backend_categories))
        {
            record_bridge_emote_stage(
                "bridge.emotes.categories.refresh_snapshot_reused",
                reuse_check_start,
                categories_len,
            );
            (
                Arc::clone(reusable_categories.as_ref().expect("reusable snapshot checked")),
                false,
            )
        } else {
            let reusable_emotes: HashMap<(_, &str), &Arc<Emote>> = reusable_categories
                .iter()
                .flat_map(|categories| categories.iter())
                .flat_map(|category| category.emotes.iter())
                .map(|emote| ((emote.provider, emote.id.as_ref()), emote))
                .collect();
            (
                Arc::new(
                    backend_categories
                        .into_iter()
                        .map(|category| EmoteCategory {
                            emotes: category
                                .emotes
                                .into_iter()
                                .map(|emote| {
                            let key = (emote.provider, emote.id.as_ref());
                                    reusable_emotes
                                        .get(&key)
                                        .filter(|existing| existing.matches_backend(&emote))
                                        .map(|existing| Arc::clone(existing))
                                        .unwrap_or_else(|| Arc::new(emote.into()))
                                })
                                .collect(),
                        })
                        .collect(),
                ),
                true,
            )
        };

        EMOTE_CATEGORY_CACHE.insert(
            channel.clone(),
            CachedEmoteCategories {
                channel_version,
                global_version,
                user_version,
                layout_version,
                categories: arc_categories,
            },
        );

        crate::log_emotes!(
            debug,
            "Background category refresh complete for {} (ch_ver={}, gl_ver={}, user_ver={}, layout_ver={})",
            channel,
            channel_version,
            global_version,
            user_version,
            layout_version
        );
        record_bridge_emote_stage(
            "bridge.emotes.categories.refresh_total",
            refresh_start,
            categories_len,
        );

        _guard.remove_now();

        if categories_changed {
            crate::state::ui_invalidation::request(
                crate::state::ui_invalidation::UiInvalidation::ProviderMetadata,
            );
            crate::managers::frame_rate_controller::record_activity();
        }
    });
}

/// Load channel emotes (fire-and-forget)
///
/// Uses the retained backend runtime; provider limits and the emote loading
/// guard continue to bound and coalesce the detached I/O task.
pub fn load_channel_emotes(channel_id: &str, channel_login: &str) {
    let channel_id = channel_id.to_string();
    let channel_login = channel_login.to_string();
    RUNTIME.spawn(async move { load_channel_emotes_async(&channel_id, &channel_login).await });
}

async fn load_channel_emotes_async(channel_id: &str, channel_login: &str) {
    if let Err(e) = crate::backend::emotes::EMOTE_CACHE
        .load_channel_emotes(channel_id, channel_login)
        .await
    {
        crate::log_emotes!(warn, "Failed to load channel emotes: {}", e);
    }
}

/// Load channel emotes and badges for a channel
/// PERF: Now fully asynchronous to avoid blocking the main thread.
pub fn load_channel_assets(channel_login: &str) {
    let channel_login = channel_login.trim().to_lowercase();

    RUNTIME.spawn(async move {
        let Some(channel_id) = super::get_channel_id_async(&channel_login).await else {
            crate::log_emotes!(
                debug,
                "Channel ID not available for {}, emotes will load when ID is fetched",
                channel_login
            );
            return;
        };
        crate::log_emotes!(
            info,
            "Loading emotes and badges for channel: {} (ID: {})",
            channel_login,
            channel_id
        );
        tokio::join!(
            load_channel_emotes_async(&channel_id, &channel_login),
            load_channel_badges_async(&channel_id, &channel_login),
        );
    });
}

/// F5: force-reload one channel's emotes past every cache,
/// heal its live 7TV subscription, and report the outcome in that channel.
pub fn reload_channel_emotes(channel_login: &str) {
    use crate::backend::lock_utils::DashSetKeyGuard;
    use crate::backend::models::SystemMessageType;
    use dashmap::DashSet;
    use std::sync::LazyLock;

    /// Channels with a manual reload in flight; repeated F5 presses coalesce.
    static RELOADING: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);

    let channel_login = channel_login.trim().trim_start_matches('#').to_lowercase();
    if channel_login.is_empty() || crate::backend::whispers::is_whispers_channel(&channel_login) {
        return;
    }
    if !RELOADING.insert(channel_login.clone()) {
        return;
    }

    RUNTIME.spawn(async move {
        let _guard = DashSetKeyGuard::new(&RELOADING, channel_login.clone());

        let Some(channel_id) = super::get_channel_id_async(&channel_login).await else {
            crate::log_emotes!(
                warn,
                "Manual emote reload for {} skipped: channel id unavailable",
                channel_login
            );
            crate::backend::irc::add_system_message(
                &channel_login,
                "Failed to reload channel emotes: channel id is not known yet.",
                SystemMessageType::Error {
                    message: "channel id unavailable".to_string(),
                },
            );
            return;
        };
        crate::log_emotes!(
            info,
            "Manual emote reload for channel: {} (ID: {})",
            channel_login,
            channel_id
        );
        let ((status, ok), ()) = tokio::join!(
            crate::backend::emotes::manual_reload_channel_emotes(&channel_id, &channel_login),
            load_channel_badges_async(&channel_id, &channel_login),
        );
        let message_type = if ok {
            SystemMessageType::Info
        } else {
            SystemMessageType::Error {
                message: status.clone(),
            }
        };
        crate::backend::irc::add_system_message(&channel_login, &status, message_type);
    });
}

/// Refresh Twitch subscription/follower emotes and 7TV personal emotes for the active account.
pub fn refresh_authenticated_user_emotes_async() {
    RUNTIME.spawn(async {
        if crate::backend::auth::get_needs_reauth() {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "Re-authentication required".to_string(),
                detail: Some("Log in again before refreshing account-specific emotes.".to_string()),
            });
            return;
        }

        let Some(token) = crate::backend::settings::get_token() else {
            crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                message: "No Twitch account token".to_string(),
                detail: Some("Log in before refreshing account-specific emotes.".to_string()),
            });
            return;
        };

        let user_id = match crate::backend::auth::validate_token_with_user_id(&token).await {
            Ok(Some(user_id)) => user_id,
            Ok(None) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Unable to refresh account emotes".to_string(),
                    detail: Some(
                        "Twitch did not return a user id for the active token.".to_string(),
                    ),
                });
                return;
            }
            Err(error) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Unable to refresh account emotes".to_string(),
                    detail: Some(error),
                });
                return;
            }
        };

        match crate::backend::emotes::force_refresh_user_emotes(&user_id, &token).await {
            Ok(()) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowSuccessToast {
                    message: "Account emotes refreshed".to_string(),
                    detail: Some(
                        "Twitch account emotes and personal 7TV emotes were reloaded.".to_string(),
                    ),
                });
            }
            Err(error) => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowWarningToast {
                    message: "Unable to refresh account emotes".to_string(),
                    detail: Some(error),
                });
            }
        }
    });
}

/// Load assets (badges/emotes) for multiple channels with coordinated batching
/// Prevents rate limiting by loading up to 5 channels concurrently
pub fn load_channel_assets_batch(channels: Vec<String>) {
    if crate::benchmark_profile::network_lanes_suppressed() {
        crate::log_emotes!(
            debug,
            "Offline benchmark suppressed channel asset batch for {} channels",
            channels.len()
        );
        return;
    }
    RUNTIME.spawn(async move {
        let mut resolved = Vec::new();
        for ch in channels {
            let ch_lower = ch.trim().to_lowercase();
            if let Some(channel_id) = super::get_channel_id_async(&ch_lower).await {
                resolved.push((channel_id, ch_lower));
            } else {
                crate::log_emotes!(
                    debug,
                    "Channel ID not available for {}, skipping batch load",
                    ch_lower
                );
            }
        }

        if resolved.is_empty() {
            return;
        }

        crate::log_emotes!(info, "Batch loading assets for {} channels", resolved.len());

        crate::backend::metrics::record_emote_stage(
            "bridge.assets.channel_emotes.deferred_to_join",
            std::time::Duration::ZERO,
            resolved.len(),
        );
        let badge_results = crate::backend::badges::TWITCH_BADGE_CACHE
            .load_channel_badges_batch(resolved.clone())
            .await;
        let badge_success = badge_results.iter().filter(|r| r.is_ok()).count();

        crate::log_emotes!(
            info,
            "Batch asset load complete: emotes=deferred_to_join/{}, badges={}/{}",
            resolved.len(),
            badge_success,
            resolved.len()
        );
    });
}

/// Preload channel emotes BEFORE IRC connection for instant rendering.
/// This is the key function for eliminating emote "pop-in" on first message.
///
/// Flow:
/// 1. Resolve channel ID (from cache or Twitch API)
/// 2. Load emote metadata from 7TV/BTTV/FFZ
/// 3. Trigger image preloading
///
/// Call this when:
/// - A new tab is created with a channel
/// - User confirms a placeholder channel input
/// - App starts (for recently visited channels)
pub fn preload_channel_emotes_predictive(channel_login: &str) {
    let channel_login = channel_login.trim().to_lowercase();
    if channel_login.is_empty() {
        return;
    }
    if crate::benchmark_profile::network_lanes_suppressed() {
        crate::log_emotes!(
            debug,
            "Offline benchmark suppressed predictive emote preload for {}",
            channel_login
        );
        return;
    }
    if crate::benchmark_ui_fixture::is_fixture_channel(&channel_login) {
        crate::log_emotes!(
            debug,
            "Skipping provider preload for hermetic UI fixture channel {}",
            channel_login
        );
        return;
    }

    let channel = channel_login.clone();
    crate::log_emotes!(info, "Starting predictive emote preload for {}", channel);

    crate::backend::settings::track_channel_visit(&channel);

    RUNTIME.spawn(async move {
        let channel_id = if let Some(id) = super::get_channel_id_cached(&channel) {
            crate::log_emotes!(debug, "Channel ID cached: {} -> {}", channel, id);
            id
        } else {
            crate::log_emotes!(debug, "Fetching channel ID for {}", channel);
            match resolve_channel_id_async(&channel).await {
                Some(id) => {
                    crate::log_emotes!(info, "Resolved channel ID: {} -> {}", channel, id);
                    id
                }
                None => {
                    crate::log_emotes!(
                        debug,
                        "Could not resolve channel ID for {}, will load when IRC connects",
                        channel
                    );
                    return;
                }
            }
        };

        if let Err(e) = crate::backend::emotes::EMOTE_CACHE
            .load_channel_emotes(&channel_id, &channel)
            .await
        {
            crate::log_emotes!(warn, "Failed to load emotes for {}: {}", channel, e);
        } else {
            crate::log_emotes!(info, "Loaded emote metadata for {}", channel);
        }

        if let Err(e) = crate::backend::badges::TWITCH_BADGE_CACHE
            .load_channel_badges(&channel_id, &channel)
            .await
        {
            crate::log_cache!(debug, "Failed to load badges for {}: {}", channel, e);
        }

        crate::cache::loader::trigger_channel_preload(&channel);

        crate::log_emotes!(info, "Predictive preload complete for {}", channel);
    });
}

/// Resolve channel ID from Twitch API (async helper)
async fn resolve_channel_id_async(channel_login: &str) -> Option<String> {
    let token = crate::backend::settings::get_token()?;

    match crate::backend::twitch_api::get_user_id_helix(channel_login, &token).await {
        Ok(id) => {
            crate::backend::twitch_api::cache_channel_id(channel_login, &id);
            Some(id)
        }
        Err(e) => {
            crate::log_emotes!(debug, "Helix lookup failed for {}: {}", channel_login, e);
            None
        }
    }
}

/// Get badge URL
pub fn get_badge_url(badge_key: &str, channel_id: &str) -> Option<String> {
    crate::backend::badges::TWITCH_BADGE_CACHE.get_badge_url_sync(badge_key, channel_id)
}

/// Get external third-party badge keys for a Twitch user in a channel.
pub fn get_external_badge_keys(
    channel_login: &str,
    login: &str,
    user_id: Option<&str>,
) -> Vec<String> {
    crate::backend::badges::TWITCH_BADGE_CACHE.get_external_badge_keys_for_user_sync(
        channel_login,
        login,
        user_id,
    )
}

pub fn has_ffz_authority_badge(badge_key: &str, channel_login: &str) -> bool {
    crate::backend::badges::TWITCH_BADGE_CACHE
        .has_ffz_authority_badge_sync(badge_key, channel_login)
}

pub fn is_using_ffz_authority_badge(badge_key: &str, channel_login: &str) -> bool {
    crate::backend::badges::TWITCH_BADGE_CACHE
        .is_using_ffz_authority_badge_sync(badge_key, channel_login)
}

/// Queue an on-demand BTTV Pro badge lookup for a visible Twitch user.
pub fn queue_bttv_pro_badge_lookup(channel_login: &str, user_login: &str, user_id: Option<&str>) {
    crate::backend::badges::TWITCH_BADGE_CACHE.queue_bttv_pro_badge_lookup(
        channel_login,
        user_login,
        user_id,
    );
}

/// Load channel badges
pub fn load_channel_badges(channel_id: &str, channel_login: &str) {
    let channel_id = channel_id.to_string();
    let channel_login = channel_login.to_string();
    RUNTIME.spawn(async move { load_channel_badges_async(&channel_id, &channel_login).await });
}

async fn load_channel_badges_async(channel_id: &str, channel_login: &str) {
    match crate::backend::badges::TWITCH_BADGE_CACHE
        .load_channel_badges(channel_id, channel_login)
        .await
    {
        Ok(()) => {}
        Err(e) if e == crate::backend::constants::API_PROXY_NOT_CONFIGURED => {
            crate::log_cache!(debug, "Skipped channel badges for {}: {}", channel_login, e);
        }
        Err(e) => crate::log_cache!(
            warn,
            "Failed to load channel badges for {}: {}",
            channel_login,
            e
        ),
    }
}
