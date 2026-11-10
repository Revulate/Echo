//! Backend initialization and shutdown
//!
//! Call `initialize()` once at startup and `shutdown()` before exit.

use super::RUNTIME;
use crate::backend::lock_utils::RwLockExt;
use crate::utils::env_usize;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_LIMIT: usize = 0;
const STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_DELAY_MS: u64 = 8_000;
const STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_GAP_MS: u64 = 2_000;
static BACKEND_SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);
const EMOTE_CACHE_AUTOSAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(45);

/// Persist the emote-set cache shortly after sets change. It used to be written
/// only on a clean exit, so after a crash, forced close or power loss the next
/// launch painted every emote as text until the provider fetches returned.
fn spawn_emote_cache_autosave() {
    RUNTIME.spawn(async {
        let mut saved = crate::backend::emotes::emote_cache_persist_generation();
        loop {
            tokio::time::sleep(EMOTE_CACHE_AUTOSAVE_INTERVAL).await;
            if BACKEND_SHUTDOWN_STARTED.load(Ordering::Acquire) {
                return;
            }
            // Read before saving: a change that lands mid-save keeps it dirty.
            let current = crate::backend::emotes::emote_cache_persist_generation();
            if current == saved {
                continue;
            }
            match crate::backend::emotes::EMOTE_CACHE.save_to_disk().await {
                Ok(()) => saved = current,
                Err(error) => crate::log_emotes!(warn, "Emote cache autosave failed: {error}"),
            }
        }
    });
}

fn env_duration_ms(name: &str, default_ms: u64) -> Duration {
    let ms = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

fn normalize_recent_channels(recent_channels: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for channel in recent_channels {
        let channel = channel.trim().trim_start_matches('#').to_ascii_lowercase();
        if channel.is_empty() || channel == "whisper" {
            continue;
        }
        if seen.insert(channel.clone()) {
            normalized.push(channel);
        }
    }

    normalized
}

fn startup_emote_cache_channels() -> HashSet<String> {
    let tabs = match crate::config::load_tabs() {
        Ok(tabs) if !tabs.is_empty() => tabs,
        Ok(_) | Err(_) => return HashSet::new(),
    };

    let active_idx = tabs.iter().position(|tab| tab.is_selected).unwrap_or(0);
    let mut scoped = HashSet::new();

    for (idx, tab) in tabs.iter().enumerate() {
        let offset = (idx as i32) - (active_idx as i32);
        if offset.abs() > 1 {
            continue;
        }

        for channel in normalize_recent_channels(tab.channels()) {
            scoped.insert(channel);
        }
    }

    scoped
}

fn start_recent_channel_emote_preload(recent_channels: Vec<String>) {
    let channels = normalize_recent_channels(recent_channels);
    if channels.is_empty() {
        return;
    }

    let limit = env_usize(
        "ECHO_STARTUP_RECENT_EMOTE_PRELOAD_LIMIT",
        STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_LIMIT,
    );
    if limit == 0 {
        crate::backend::metrics::record_emote_event("startup.recent_preload.disabled");
        crate::log_emotes!(
            info,
            "Startup recent emote preload disabled by ECHO_STARTUP_RECENT_EMOTE_PRELOAD_LIMIT=0"
        );
        return;
    }

    let total = channels.len();
    let selected = channels.into_iter().take(limit).collect::<Vec<_>>();
    let skipped_by_limit = total.saturating_sub(selected.len());
    let initial_delay = env_duration_ms(
        "ECHO_STARTUP_RECENT_EMOTE_PRELOAD_DELAY_MS",
        STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_DELAY_MS,
    );
    let gap = env_duration_ms(
        "ECHO_STARTUP_RECENT_EMOTE_PRELOAD_GAP_MS",
        STARTUP_RECENT_EMOTE_PRELOAD_DEFAULT_GAP_MS,
    );

    crate::backend::metrics::record_emote_event("startup.recent_preload.scheduled");
    if skipped_by_limit > 0 {
        crate::backend::metrics::record_emote_stage(
            "startup.recent_preload.skipped_limit",
            Duration::ZERO,
            skipped_by_limit,
        );
    }
    crate::log_emotes!(
        info,
        "Scheduling startup recent emote preload: selected={}, skipped_by_limit={}, delay_ms={}, gap_ms={}, channels={:?}",
        selected.len(),
        skipped_by_limit,
        initial_delay.as_millis(),
        gap.as_millis(),
        selected
    );

    RUNTIME.spawn(async move {
        if !initial_delay.is_zero() {
            tokio::time::sleep(initial_delay).await;
        }
        preload_recent_channel_emotes_sequential(selected, gap).await;
    });
}

async fn preload_recent_channel_emotes_sequential(channels: Vec<String>, gap: Duration) {
    let total_start = Instant::now();
    let total = channels.len();
    let mut loaded = 0usize;
    let mut skipped_fresh = 0usize;
    let mut skipped_unresolved = 0usize;
    let mut failed = 0usize;

    for (index, channel_login) in channels.iter().enumerate() {
        if !crate::backend::emotes::EMOTE_CACHE.channel_emotes_need_refresh(channel_login) {
            skipped_fresh += 1;
            let cached_count =
                crate::backend::emotes::EMOTE_CACHE.channel_emote_count(channel_login);
            crate::backend::metrics::record_emote_stage(
                "startup.recent_preload.skip_fresh",
                Duration::ZERO,
                cached_count,
            );
            crate::log_emotes!(
                debug,
                "Skipping startup recent emote preload for {}: cache still fresh ({} emotes)",
                channel_login,
                cached_count
            );
        } else if let Some(channel_id) = resolve_recent_preload_channel_id(channel_login).await {
            let channel_start = Instant::now();
            match crate::backend::emotes::EMOTE_CACHE
                .load_channel_emotes(&channel_id, channel_login)
                .await
            {
                Ok(()) => {
                    loaded += 1;
                    let count =
                        crate::backend::emotes::EMOTE_CACHE.channel_emote_count(channel_login);
                    crate::backend::metrics::record_emote_stage(
                        "startup.recent_preload.channel",
                        channel_start.elapsed(),
                        count,
                    );
                    crate::log_emotes!(
                        debug,
                        "Startup recent emote preload complete for {} ({} emotes)",
                        channel_login,
                        count
                    );
                }
                Err(e) => {
                    failed += 1;
                    crate::backend::metrics::record_emote_event("startup.recent_preload.failed");
                    crate::backend::metrics::record_emote_stage(
                        "startup.recent_preload.channel_failed",
                        channel_start.elapsed(),
                        0,
                    );
                    crate::log_emotes!(
                        debug,
                        "Startup recent emote preload failed for {}: {}",
                        channel_login,
                        e
                    );
                }
            }
        } else {
            skipped_unresolved += 1;
            crate::backend::metrics::record_emote_event("startup.recent_preload.skip_unresolved");
            crate::log_emotes!(
                debug,
                "Skipping startup recent emote preload for {}: channel id unavailable",
                channel_login
            );
        }

        if index + 1 < total && !gap.is_zero() {
            tokio::time::sleep(gap).await;
        }
    }

    let elapsed = total_start.elapsed();
    crate::backend::metrics::record_emote_stage("startup.recent_preload.total", elapsed, total);
    crate::log_emotes!(
        info,
        "Startup recent emote preload summary: total={}, loaded={}, skipped_fresh={}, skipped_unresolved={}, failed={}, elapsed_ms={}",
        total,
        loaded,
        skipped_fresh,
        skipped_unresolved,
        failed,
        elapsed.as_millis()
    );
}

async fn resolve_recent_preload_channel_id(channel_login: &str) -> Option<String> {
    if let Some(id) = super::get_channel_id_cached(channel_login) {
        return Some(id);
    }

    let Some(token) = crate::backend::settings::get_token() else {
        crate::backend::metrics::record_emote_event("startup.recent_preload.skip_no_token");
        return None;
    };

    match crate::backend::twitch_api::get_user_id_helix(channel_login, &token).await {
        Ok(id) => {
            crate::backend::twitch_api::cache_channel_id(channel_login, &id);
            Some(id)
        }
        Err(e) => {
            crate::log_emotes!(
                debug,
                "Failed to resolve channel id for startup recent emote preload {}: {}",
                channel_login,
                e
            );
            None
        }
    }
}

async fn set_emote_cache_path() {
    let Some(echo_dir) = crate::benchmark_profile::config_root() else {
        crate::log_ui!(
            warn,
            "Could not determine config directory, emote cache will not be persisted"
        );
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&echo_dir) {
        crate::log_ui!(warn, "Failed to create config directory: {}", e);
    }
    let emote_cache_path = echo_dir.join("emote_cache.postcard");
    crate::log_ui!(info, "Set emote cache path: {:?}", emote_cache_path);
    crate::backend::emotes::EMOTE_CACHE
        .set_cache_path(emote_cache_path)
        .await;
}

/// Retokenize every resident chat row on a blocking thread. Only rows whose
/// emote spans change are replaced.
fn spawn_resident_row_retokenize() {
    tokio::task::spawn_blocking(|| {
        crate::backend::irc::retokenize_all_channels_if_current(|| true)
    });
}

/// Initialize the backend (call once at startup)
pub fn initialize() {
    crate::log_ui!(info, "Initializing Echo backend...");
    let offline_scenario = crate::benchmark_profile::network_lanes_suppressed();

    crate::backend::slots::resolver::install_resolver_ports();

    crate::backend::irc::load_user_color_cache();
    crate::backend::irc::load_user_color_overrides();
    crate::startup_metrics::record("user_colors_loaded");

    crate::backend::settings::init_settings_channel();
    crate::backend::activity_persistence::start();
    if !offline_scenario {
        crate::backend::browser_watching::initialize();
        crate::backend::updates::start_automatic_check();
    }

    if !offline_scenario && let Some(_token) = crate::backend::settings::get_token() {
        if crate::backend::settings::get_anonymous_mode() {
            crate::backend::irc::set_runtime_anonymous_mode(true);
        } else {
            crate::backend::irc::reset_anonymous_mode();
        }

        let mut state = crate::backend::auth::AUTH_STATE.write_or_recover();
        state.authenticated = true;
        state.username = crate::backend::settings::get_username();
        if let Some(account) = crate::backend::settings::get_active_account() {
            state.display_name = account.display_name;
            state.profile_image_url = account.profile_image_url;
        }
        drop(state);
        crate::backend::eventsub::notify_auth_readiness_changed();
    }
    crate::startup_metrics::record("settings_loaded");

    RUNTIME.spawn(async move {
        let startup_emote_channels = startup_emote_cache_channels();
        if !offline_scenario {
            for channel in startup_emote_channels.iter().cloned() {
                tokio::spawn(crate::backend::irc::spawn_history_load(channel));
            }
        }

        // Cached history is tokenized against the emote catalogs, and its
        // merge waits until they are ready. The global and channel sets need
        // no account, so they hydrate from disk while the token is validated,
        // and a disk global set marks the catalogs ready at once. History then
        // shows without waiting on the token check, the global refresh or the
        // user-emote fetch.
        let hydrate_shared_catalogs = {
            let channels = startup_emote_channels.clone();
            async move {
                set_emote_cache_path().await;
                let (global, _, channel_count) = crate::backend::emotes::EMOTE_CACHE
                    .load_from_disk_scoped(
                        Some(channels),
                        crate::backend::emotes::DiskCacheParts::Shared,
                    )
                    .await;
                crate::log_emotes!(
                    info,
                    "Loaded shared emote cache: global={}, channels={}",
                    global,
                    channel_count
                );
                if global {
                    crate::backend::emotes::mark_initial_catalogs_ready();
                    crate::startup_metrics::record("initial_catalogs_ready");
                    Some(crate::backend::emotes::get_global_emote_version())
                } else {
                    None
                }
            }
        };
        let global_version_at_ready = if offline_scenario {
            hydrate_shared_catalogs.await
        } else {
            let (version, ()) = tokio::join!(
                hydrate_shared_catalogs,
                crate::backend::auth::initialize_auth()
            );
            version
        };

        let eventapi_replay_only = !offline_scenario
            && crate::backend::emotes::seventv_eventapi::start_eventapi_replay_from_env();

        if !offline_scenario {
            crate::backend::irc::start_watchdog();
        }

        if !offline_scenario && let Some(token) = crate::backend::settings::get_token() {
            crate::backend::auth::distribute_token(&token).await;
        }

        let twitch_lane = async {
            if offline_scenario {
                return;
            }
            if let Err(e) = crate::backend::badges::TWITCH_BADGE_CACHE
                .load_global_badges()
                .await
            {
                crate::log_cache!(warn, "Failed to load global badges: {}", e);
            }
            if let Err(e) = crate::backend::badges::TWITCH_BADGE_CACHE
                .load_external_global_badges()
                .await
            {
                crate::log_cache!(warn, "Failed to load external badges: {}", e);
            }

            if crate::backend::settings::get_token().is_some() {
                crate::log_net!(info, "Starting EventSub connection...");
                crate::backend::eventsub::init_eventsub();
            }
        };

        let emote_lane = async {
            // User emotes hydrate after auth initialization, which publishes the
            // validated owner id they are fenced on. Autosave starts only after
            // this, so it never writes a cache missing the disk user set.
            let startup_scope_count = startup_emote_channels.len();
            let (_, user, _) = crate::backend::emotes::EMOTE_CACHE
                .load_from_disk_scoped(
                    Some(startup_emote_channels),
                    crate::backend::emotes::DiskCacheParts::User,
                )
                .await;
            crate::log_emotes!(
                info,
                "Loaded user emote cache: user={}, startup_scope={}",
                user,
                startup_scope_count
            );

            spawn_emote_cache_autosave();

            crate::backend::chat_replay::start_chat_replay_from_env();

            if !offline_scenario
                && crate::backend::emotes::EMOTE_CACHE
                    .global_needs_refresh()
                    .await
            {
                crate::log_emotes!(info, "Loading global emotes...");
                if let Err(e) = crate::backend::emotes::EMOTE_CACHE
                    .load_global_emotes()
                    .await
                {
                    crate::log_emotes!(warn, "Failed to load global emotes: {}", e);
                } else {
                    crate::log_emotes!(info, "Global emotes loaded successfully");
                    // History shown from the disk set was tokenized against it;
                    // if the refreshed set differs, retokenize resident rows.
                    if global_version_at_ready.is_some_and(|version| {
                        version != crate::backend::emotes::get_global_emote_version()
                    }) {
                        spawn_resident_row_retokenize();
                    }
                }
            } else {
                crate::backend::metrics::record_emote_event("startup.global_emotes.cache_hit");
                crate::log_emotes!(debug, "Global emotes cache fresh; skipping startup refresh");
            }
            if global_version_at_ready.is_none() {
                crate::backend::emotes::mark_initial_catalogs_ready();
                crate::startup_metrics::record("initial_catalogs_ready");
            }

            if !offline_scenario
                && !crate::backend::irc::is_anonymous_mode()
                && let Some(token) = crate::backend::settings::get_token()
            {
                // Startup validation already published this token's user id;
                // validate again only when it could not.
                let validated = match crate::backend::auth::active_authenticated_user_id() {
                    Some(user_id) => Ok(Some(user_id)),
                    None => crate::backend::auth::validate_token_with_user_id(&token).await,
                };
                match validated {
                    Ok(Some(user_id)) => {
                        if crate::backend::irc::is_anonymous_mode() {
                            crate::log_emotes!(
                                debug,
                                "Skipping startup user-emote load after anonymous-mode transition"
                            );
                        } else {
                            crate::log_emotes!(
                                info,
                                "Loading user subscription emotes for user_id: {}",
                                user_id
                            );
                            if let Err(e) = crate::backend::emotes::EMOTE_CACHE
                                .load_user_emotes(&user_id, &token)
                                .await
                            {
                                crate::log_emotes!(warn, "Failed to load user emotes: {}", e);
                            } else {
                                crate::log_emotes!(
                                    info,
                                    "User subscription emotes loaded successfully"
                                );
                                crate::backend::emotes::rebuild_user_emote_index().await;
                            }
                        }
                    }
                    Ok(None) => {
                        crate::log_emotes!(warn, "Token validation returned no user_id");
                    }
                    Err(e) => {
                        crate::log_emotes!(warn, "Failed to validate token for user emotes: {}", e);
                    }
                }
            }

            if !offline_scenario {
                let recent_channels = crate::backend::settings::get_recent_channels();
                start_recent_channel_emote_preload(recent_channels);

                crate::backend::emotes::refresh::start_refresh_scheduler();

                if !eventapi_replay_only {
                    crate::backend::emotes::seventv_eventapi::start_seventv_event_api();
                }
            }
        };

        tokio::join!(twitch_lane, emote_lane);
    });

    crate::log_ui!(info, "Backend initialization complete");
}

/// Shutdown the backend (call before exit)
pub fn shutdown() {
    if BACKEND_SHUTDOWN_STARTED.swap(true, Ordering::SeqCst) {
        crate::log_ui!(
            debug,
            "Backend shutdown already started; skipping duplicate request"
        );
        return;
    }

    let shutdown_start = Instant::now();
    crate::log_ui!(info, "Backend shutdown started");

    crate::backend::browser_watching::shutdown();

    let settings_start = Instant::now();
    crate::backend::settings::save_settings();
    crate::backend::settings::flush_settings_saves(std::time::Duration::from_secs(3));
    crate::state::emote_preferences::flush_preferences_saves(std::time::Duration::from_secs(3));
    crate::log_ui!(
        info,
        elapsed_ms = settings_start.elapsed().as_millis(),
        "Backend shutdown: settings saved"
    );

    let eventapi_start = Instant::now();
    crate::backend::emotes::seventv_eventapi::shutdown();
    crate::log_ui!(
        info,
        elapsed_ms = eventapi_start.elapsed().as_millis(),
        "Backend shutdown: EventAPI stop requested"
    );

    let color_start = Instant::now();
    crate::backend::irc::save_user_color_cache();
    if let Err(error) = crate::backend::irc::save_user_color_overrides() {
        crate::log_cache!(warn, "Failed to save user color overrides: {error}");
    }
    crate::log_ui!(
        info,
        elapsed_ms = color_start.elapsed().as_millis(),
        "Backend shutdown: user color cache saved"
    );

    let async_start = Instant::now();
    RUNTIME.block_on(async {
        let emote_start = Instant::now();
        if let Err(e) = crate::backend::emotes::EMOTE_CACHE.save_to_disk().await {
            crate::log_emotes!(warn, "Failed to save emote cache: {}", e);
        } else {
            crate::log_emotes!(
                info,
                elapsed_ms = emote_start.elapsed().as_millis(),
                "Emote cache saved successfully"
            );
        }
        let cache_start = Instant::now();
        crate::cache::ImageCacheService::shutdown_runtime("backend_shutdown").await;
        crate::log_cache!(
            info,
            elapsed_ms = cache_start.elapsed().as_millis(),
            "Cache state saved"
        );
    });
    crate::log_ui!(
        info,
        elapsed_ms = async_start.elapsed().as_millis(),
        "Backend shutdown: async cache saves complete"
    );

    let irc_shutdown_start = Instant::now();
    let irc_drained = crate::backend::irc::shutdown_read_connection(Duration::from_secs(3));
    crate::log_ui!(
        info,
        elapsed_ms = irc_shutdown_start.elapsed().as_millis(),
        drained = irc_drained,
        "Backend shutdown: IRC read connection stop attempted"
    );

    let readable_log_start = Instant::now();
    let readable_log_drained =
        crate::backend::history_cache::shutdown_readable_chat_logs(Duration::from_secs(3));
    crate::log_ui!(
        info,
        elapsed_ms = readable_log_start.elapsed().as_millis(),
        drained = readable_log_drained,
        "Backend shutdown: readable chat log shutdown attempted"
    );

    let activity_start = Instant::now();
    let activity_drained = crate::backend::activity_persistence::flush(Duration::from_secs(3));
    crate::log_ui!(
        info,
        elapsed_ms = activity_start.elapsed().as_millis(),
        drained = activity_drained,
        "Backend shutdown: Activity snapshot flush attempted"
    );

    let history_start = Instant::now();
    let _history_visibility_fence = crate::backend::blocked_users::visibility_commit_guard();
    let live_history_drained =
        crate::backend::history_cache::shutdown_live_history_writer(Duration::from_secs(3));
    crate::backend::history_cache::save_all_channels();
    crate::log_ui!(
        info,
        elapsed_ms = history_start.elapsed().as_millis(),
        drained = live_history_drained,
        "Backend shutdown: history cache saved"
    );

    crate::log_ui!(
        info,
        elapsed_ms = shutdown_start.elapsed().as_millis(),
        "Backend shutdown complete"
    );

    crate::backend::reliability::mark_clean_shutdown();
}
