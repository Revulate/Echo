//! Channel emote loading, shared-chat source rooms and per-message emote scopes.

use super::*;

pub(super) static DEFERRED_CHANNEL_EMOTE_REFRESHES: LazyLock<DashMap<String, ()>> =
    LazyLock::new(DashMap::new);
pub(super) static DEFERRED_CHANNEL_EMOTE_REFRESH_SLOT: AtomicU64 = AtomicU64::new(0);

pub(super) const MAX_PENDING_MESSAGE_EMOTE_SCOPES: usize = 1_024;

/// Shared-chat source rooms whose login is being resolved (with the messages
/// waiting on it) or recently failed to resolve, so a burst of messages from an
/// unresolved room issues one Helix lookup instead of one per message.
pub(super) enum SourceRoomLookup {
    InFlight(Vec<(String, String)>),
    /// When the lookup failed, and the proxy generation it failed under: a
    /// routing change makes the failure stale.
    Failed(std::time::Instant, u64),
}

impl SourceRoomLookup {
    pub(super) fn is_fresh_failure(&self, generation: u64) -> bool {
        matches!(self, SourceRoomLookup::Failed(at, failed_generation)
            if *failed_generation == generation && at.elapsed() < SOURCE_ROOM_LOOKUP_RETRY)
    }
}

pub(super) static SHARED_SOURCE_ROOM_LOOKUPS: LazyLock<Mutex<HashMap<String, SourceRoomLookup>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) static SOURCE_ROOM_WAITER_OVERFLOW_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(super) const SOURCE_ROOM_LOOKUP_RETRY: std::time::Duration =
    std::time::Duration::from_secs(5 * 60);
pub(super) const MAX_SOURCE_ROOM_LOOKUPS: usize = 256;
pub(super) const MAX_SOURCE_ROOM_WAITERS: usize = 256;

/// Register `message_id` as waiting on `source_id`; true when the caller must
/// start the lookup. The map is hard-capped: when full of in-flight lookups a
/// new source room is not looked up.
pub(super) fn begin_source_room_lookup(
    source_id: &str,
    host_channel: &str,
    message_id: &str,
) -> bool {
    let generation = crate::backend::http::proxy_generation();
    let mut lookups = SHARED_SOURCE_ROOM_LOOKUPS.lock();
    match lookups.get_mut(source_id) {
        Some(SourceRoomLookup::InFlight(waiters)) => {
            if waiters.len() < MAX_SOURCE_ROOM_WAITERS {
                waiters.push((host_channel.to_string(), message_id.to_string()));
            } else if !SOURCE_ROOM_WAITER_OVERFLOW_LOGGED
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                crate::log_irc!(
                    debug,
                    "Shared-chat source room {} has {} waiting messages; dropping further waiters",
                    source_id,
                    MAX_SOURCE_ROOM_WAITERS
                );
            }
            return false;
        }
        Some(lookup) if lookup.is_fresh_failure(generation) => return false,
        _ => {}
    }
    if !lookups.contains_key(source_id) && lookups.len() >= MAX_SOURCE_ROOM_LOOKUPS {
        lookups.retain(|_, lookup| {
            matches!(lookup, SourceRoomLookup::InFlight(_)) || lookup.is_fresh_failure(generation)
        });
        if lookups.len() >= MAX_SOURCE_ROOM_LOOKUPS {
            let oldest_failed = lookups
                .iter()
                .filter_map(|(id, lookup)| match lookup {
                    SourceRoomLookup::Failed(at, _) => Some((*at, id.clone())),
                    SourceRoomLookup::InFlight(_) => None,
                })
                .min_by_key(|(at, _)| *at)
                .map(|(_, id)| id);
            match oldest_failed {
                Some(id) => {
                    lookups.remove(&id);
                }
                None => return false,
            }
        }
    }
    lookups.insert(
        source_id.to_string(),
        SourceRoomLookup::InFlight(vec![(host_channel.to_string(), message_id.to_string())]),
    );
    true
}

/// Finish a lookup: returns the waiting messages; a failure is remembered.
pub(super) fn finish_source_room_lookup(source_id: &str, resolved: bool) -> Vec<(String, String)> {
    let mut lookups = SHARED_SOURCE_ROOM_LOOKUPS.lock();
    let previous = if resolved {
        lookups.remove(source_id)
    } else {
        lookups.insert(
            source_id.to_string(),
            SourceRoomLookup::Failed(
                std::time::Instant::now(),
                crate::backend::http::proxy_generation(),
            ),
        )
    };
    match previous {
        Some(SourceRoomLookup::InFlight(waiters)) => waiters,
        _ => Vec::new(),
    }
}
/// Emote lookup channel resolved for a buffered row, keyed by
/// `(host channel, message id)`.
type MessageEmoteScopes = HashMap<(String, String), Arc<str>>;

pub(super) static PENDING_MESSAGE_EMOTE_SCOPES: LazyLock<Mutex<MessageEmoteScopes>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// When resolving a channel's id for its emotes last failed. A later message
/// retries after [`EMOTE_CHANNEL_ID_RETRY`]; without this a shared-chat source
/// whose lookup failed once stayed without emotes for the session.
pub(super) static EMOTE_CHANNEL_ID_FAILURES: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub(super) const EMOTE_CHANNEL_ID_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// Preload emotes and embeds from message slots immediately on receipt
/// This ensures content is downloaded/fetched BEFORE entering viewport, eliminating blank spaces during scrolling.
pub(super) fn preload_message_content(slots: &[MessageSlot], channel: &str, msg_id: &str) {
    use crate::backend::slots::resolver::{ResolvedSlot, SlotResolver};

    let trace_preload = crate::log::emote_trace_enabled();
    if trace_preload {
        crate::log_emotes!(
            debug,
            "Preload: Processing message {} in channel {} with {} slots",
            msg_id,
            channel,
            slots.len()
        );
    }

    let mut emote_urls: Vec<String> = Vec::new();
    let resolver = SlotResolver::new(channel);

    for slot in slots {
        if trace_preload {
            crate::log_emotes!(debug, "Preload: Slot type: {:?}", slot);
        }
        match slot {
            MessageSlot::Emote(emote) => {
                let url = crate::backend::emotes::chat_display_url(emote);
                emote_urls.push(url.to_string());
            }

            MessageSlot::TwitchEmote { .. } => {
                if let ResolvedSlot::Emote(emote) = resolver.resolve(slot) {
                    let url = crate::backend::emotes::chat_display_url(&emote);
                    emote_urls.push(url.to_string());
                }
            }

            MessageSlot::Url { .. } => {}

            _ => {}
        }
    }

    if !emote_urls.is_empty() {
        if trace_preload {
            crate::log_emotes!(
                debug,
                "Preload: Triggering lazy preload for {} emote URLs in message {}",
                emote_urls.len(),
                msg_id
            );
        }
        crate::cache::loader::trigger_lazy_preload(
            emote_urls,
            crate::cache::loader::PreloadSource::IrcMessage,
        );
    } else if trace_preload {
        crate::log_emotes!(
            debug,
            "Preload: No emotes to preload in message {} ({} slots total)",
            msg_id,
            slots.len()
        );
    }
}

pub(super) fn schedule_staggered_channel_emote_load(
    channel_id: String,
    channel_login: String,
    cached_count: usize,
    reason: &'static str,
) {
    let channel_key = channel_login.to_lowercase();
    if DEFERRED_CHANNEL_EMOTE_REFRESHES
        .insert(channel_key.clone(), ())
        .is_some()
    {
        crate::backend::metrics::record_emote_event("irc.channel_emotes.staggered.coalesced");
        crate::log_emotes!(
            debug,
            "Staggered channel emote load already queued for {}",
            channel_key
        );
        return;
    }

    let slot = DEFERRED_CHANNEL_EMOTE_REFRESH_SLOT.fetch_add(1, Ordering::Relaxed);
    let force_refresh = cached_count > 0;
    let (base_delay, stagger) = if force_refresh {
        (
            CACHED_CHANNEL_EMOTE_REFRESH_BASE_DELAY_MS,
            CACHED_CHANNEL_EMOTE_REFRESH_STAGGER_MS,
        )
    } else {
        (0, CACHED_CHANNEL_EMOTE_INITIAL_STAGGER_MS)
    };
    let delay_ms = base_delay
        .saturating_add(slot.saturating_mul(stagger))
        .min(CACHED_CHANNEL_EMOTE_REFRESH_MAX_DELAY_MS);
    let scheduled_stage = if force_refresh {
        "irc.channel_emotes.deferred_cached.scheduled"
    } else {
        "irc.channel_emotes.staggered_initial.scheduled"
    };
    crate::backend::metrics::record_emote_stage(
        scheduled_stage,
        std::time::Duration::from_millis(delay_ms),
        cached_count,
    );
    if force_refresh {
        crate::log_emotes!(
            debug,
            "Using cached emotes for {} immediately ({} emotes); deferred refresh in {}ms ({})",
            channel_key,
            cached_count,
            delay_ms,
            reason
        );
    } else {
        crate::log_emotes!(
            debug,
            "Scheduling initial channel emote load for {} in {}ms ({})",
            channel_key,
            delay_ms,
            reason
        );
    }

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let started_at = std::time::Instant::now();
        let outcome = EMOTE_CACHE
            .load_channel_emotes_reporting(&channel_id, &channel_key, force_refresh)
            .await;

        match outcome {
            Ok(crate::backend::emotes::LoadOutcome::Coalesced) => {
                crate::backend::metrics::record_emote_event(
                    "irc.channel_emotes.staggered.coalesced_retry",
                );
                let retry_channel_id = channel_id.clone();
                let retry_channel_key = channel_key.clone();
                crate::backend::bridge::RUNTIME.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        COALESCED_CHANNEL_EMOTE_RETRY_DELAY_MS,
                    ))
                    .await;
                    if !crate::backend::emotes::channel_has_lookup_entry(&retry_channel_key) {
                        crate::log_emotes!(
                            debug,
                            "Coalesced staggered load left {} unresolved; forcing one retry",
                            retry_channel_key
                        );
                        let _ = EMOTE_CACHE
                            .load_channel_emotes_reporting(
                                &retry_channel_id,
                                &retry_channel_key,
                                true,
                            )
                            .await;
                    }
                    DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&retry_channel_key);
                });
            }
            Ok(_) => {
                DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&channel_key);
                let count = EMOTE_CACHE.channel_emote_count(&channel_key);
                let completed_stage = if force_refresh {
                    "irc.channel_emotes.deferred_cached.completed"
                } else {
                    "irc.channel_emotes.staggered_initial.completed"
                };
                crate::backend::metrics::record_emote_stage(
                    completed_stage,
                    started_at.elapsed(),
                    count,
                );
                crate::log_emotes!(
                    debug,
                    "Staggered channel emote load complete for {} ({} emotes)",
                    channel_key,
                    count
                );
            }
            Err(e) => {
                DEFERRED_CHANNEL_EMOTE_REFRESHES.remove(&channel_key);
                let failed_stage = if force_refresh {
                    "irc.channel_emotes.deferred_cached.failed"
                } else {
                    "irc.channel_emotes.staggered_initial.failed"
                };
                crate::backend::metrics::record_emote_event(failed_stage);
                crate::backend::metrics::record_emote_stage(failed_stage, started_at.elapsed(), 0);
                crate::log_emotes!(
                    debug,
                    "Staggered channel emote load failed for {}: {}",
                    channel_key,
                    e
                );
            }
        }
    });
}

pub(super) async fn load_or_defer_channel_emotes(
    channel_id: String,
    channel_login: String,
    reason: &'static str,
) {
    let cached_count = EMOTE_CACHE.channel_emote_count(&channel_login);
    schedule_staggered_channel_emote_load(channel_id, channel_login, cached_count, reason);
}

pub(super) async fn ensure_channel_emotes_loaded(
    channel_login: &str,
    channel_id_hint: Option<&str>,
) {
    let channel_key = channel_login.to_lowercase();
    {
        let mut failures = EMOTE_CHANNEL_ID_FAILURES.lock();
        if let Some(failed_at) = failures.get(&channel_key) {
            if failed_at.elapsed() < EMOTE_CHANNEL_ID_RETRY {
                return;
            }
            failures.remove(&channel_key);
        }
    }
    let should_load_emotes = {
        let mut loaded = EMOTE_LOADED_CHANNELS.lock();
        if !loaded.contains(&channel_key) {
            loaded.insert(channel_key.clone());
            true
        } else {
            false
        }
    };

    if !should_load_emotes {
        return;
    }

    let resolved_channel_id = if let Some(id) = channel_id_hint {
        id.to_string()
    } else if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(channel_login) {
        id
    } else {
        match crate::backend::twitch_api::get_user_id_cached(channel_login).await {
            Ok(id) => id,
            Err(err) => {
                if err.is_proxy_not_configured() {
                    crate::log_emotes!(
                        debug,
                        "Skipped channel id lookup for {} while loading emotes: {}",
                        channel_login,
                        err
                    );
                } else {
                    crate::log_emotes!(
                        warn,
                        "Failed to resolve channel id for {} while loading emotes: {}",
                        channel_login,
                        err
                    );
                }
                EMOTE_LOADED_CHANNELS.lock().remove(&channel_key);
                let mut failures = EMOTE_CHANNEL_ID_FAILURES.lock();
                if failures.len() >= 256 {
                    failures.retain(|_, failed_at| failed_at.elapsed() < EMOTE_CHANNEL_ID_RETRY);
                }
                failures.insert(channel_key, std::time::Instant::now());
                return;
            }
        }
    };

    crate::backend::twitch_api::cache_channel_id(channel_login, &resolved_channel_id);

    let load_login = channel_login.to_string();
    tokio::spawn(async move {
        load_or_defer_channel_emotes(resolved_channel_id, load_login, "irc.ensure").await;
    });
}

pub(super) const INITIAL_GLOBAL_EMOTE_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(15);
pub(super) const HISTORY_EMOTE_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(90);

pub(super) struct HistoryEmoteReadinessGuard {
    pub(super) channel: String,
    pub(super) committed: bool,
}

impl HistoryEmoteReadinessGuard {
    pub(super) fn new(channel: &str) -> Self {
        publish_history_emote_readiness(channel, false);
        Self {
            channel: channel.to_string(),
            committed: false,
        }
    }

    pub(super) fn commit(&mut self) {
        publish_history_emote_readiness(&self.channel, true);
        self.committed = true;
    }
}

impl Drop for HistoryEmoteReadinessGuard {
    fn drop(&mut self) {
        if !self.committed {
            publish_history_emote_readiness(&self.channel, false);
        }
    }
}

pub(super) async fn ensure_channel_emotes_ready_for_history(
    channel_login: &str,
    channel_id_hint: Option<&str>,
) -> bool {
    let channel_key = channel_login.to_lowercase();
    let mut readiness = HistoryEmoteReadinessGuard::new(&channel_key);
    if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
        readiness.commit();
        return true;
    }

    if EMOTE_CACHE.channel_emote_count(&channel_key) > 0 {
        crate::backend::emotes::rebuild_emote_indices(&channel_key).await;
        if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
            readiness.commit();
            return true;
        }
    }

    let channel_id = if let Some(id) = channel_id_hint {
        id.to_string()
    } else if let Some(id) = crate::backend::twitch_api::get_cached_channel_id(&channel_key) {
        id
    } else {
        match crate::backend::twitch_api::get_user_id_cached(&channel_key).await {
            Ok(id) => id,
            Err(error) => {
                crate::log_emotes!(
                    debug,
                    "History emote readiness could not resolve {}: {}",
                    channel_key,
                    error
                );
                return false;
            }
        }
    };
    crate::backend::twitch_api::cache_channel_id(&channel_key, &channel_id);

    let ready = tokio::time::timeout(HISTORY_EMOTE_READY_TIMEOUT, async {
        loop {
            if is_channel_closed(&channel_key) {
                return false;
            }
            if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
                return true;
            }
            if crate::backend::emotes::channel_initial_catalog_pending(&channel_key) {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
            let load_id = channel_id.clone();
            let load_key = channel_key.clone();
            let outcome = crate::backend::bridge::RUNTIME
                .spawn(async move {
                    EMOTE_CACHE
                        .load_channel_emotes_reporting(&load_id, &load_key, false)
                        .await
                })
                .await
                .unwrap_or_else(|error| Err(format!("channel emote task failed: {error}")));
            match outcome {
                Ok(crate::backend::emotes::LoadOutcome::Loaded) => {
                    if !crate::backend::emotes::channel_has_lookup_entry(&channel_key) {
                        crate::backend::emotes::rebuild_emote_indices(&channel_key).await;
                    }
                    if crate::backend::emotes::channel_catalog_ready_for_history(&channel_key) {
                        return true;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Ok(crate::backend::emotes::LoadOutcome::Coalesced) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => {
                    crate::log_emotes!(
                        debug,
                        "History emote readiness failed for {}: {}",
                        channel_key,
                        error
                    );
                    return false;
                }
            }
        }
    })
    .await
    .unwrap_or(false);

    if !ready {
        crate::log_emotes!(
            debug,
            "History emote readiness timed out for {} after {}s",
            channel_key,
            HISTORY_EMOTE_READY_TIMEOUT.as_secs()
        );
    } else {
        readiness.commit();
    }
    ready
}

pub(super) fn publish_history_emote_readiness(channel: &str, ready: bool) {
    let mut loaded = EMOTE_LOADED_CHANNELS.lock();
    if ready {
        if is_channel_closed(channel) {
            return;
        }
        loaded.insert(channel.to_string());
    } else {
        loaded.remove(channel);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SharedChatEmoteLookupSelection {
    pub(crate) channel_key: String,
    pub(crate) unresolved_source_room_id: Option<String>,
}

/// Select the channel catalog used to tokenize one shared-chat row.
///
/// A foreign source whose login is not yet known must not inherit the host
/// channel's emotes. Its empty key intentionally resolves only user/global
/// emotes; the caller can resolve `unresolved_source_room_id` asynchronously
/// for future rows without changing the current row.
pub(crate) fn select_shared_chat_emote_lookup(
    host_channel_login: &str,
    host_channel_id: &str,
    source_broadcaster_login: Option<&str>,
    source_room_id: Option<&str>,
    cached_source_login: impl FnOnce(&str) -> Option<String>,
) -> SharedChatEmoteLookupSelection {
    if let Some(source_login) = source_broadcaster_login {
        return SharedChatEmoteLookupSelection {
            channel_key: source_login.to_lowercase(),
            unresolved_source_room_id: None,
        };
    }

    let Some(source_id) = source_room_id.filter(|source_id| *source_id != host_channel_id) else {
        return SharedChatEmoteLookupSelection {
            channel_key: host_channel_login.to_lowercase(),
            unresolved_source_room_id: None,
        };
    };

    if let Some(source_login) = cached_source_login(source_id) {
        SharedChatEmoteLookupSelection {
            channel_key: source_login.to_lowercase(),
            unresolved_source_room_id: None,
        }
    } else {
        SharedChatEmoteLookupSelection {
            channel_key: String::new(),
            unresolved_source_room_id: Some(source_id.to_string()),
        }
    }
}
