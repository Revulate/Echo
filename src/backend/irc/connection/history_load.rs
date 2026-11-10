//! Loading recent-message history and filling gaps after a reconnect.

use super::*;

/// When each channel's history last finished loading. Used to skip the
/// connect-time streaming reload right after the startup prefetch while still
/// allowing reconnect gap-fill (which happens outside the skip window).
pub(super) static HISTORY_LOADED_AT: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) const HISTORY_RELOAD_SKIP_WINDOW_SECS: u64 = 30;

pub(super) fn record_history_loaded_now(channel_lower: &str) {
    HISTORY_LOADED_AT
        .lock()
        .insert(channel_lower.to_string(), std::time::Instant::now());
}

/// True once a recent-messages pass has completed for the channel since it was
/// opened. A failed pass leaves this false and posts an error row instead.
pub fn is_history_loaded(channel: &str) -> bool {
    let channel = channel.trim().trim_start_matches('#').to_lowercase();
    HISTORY_LOADED_AT.lock().contains_key(&channel)
}

pub(super) fn history_recently_loaded(channel: &str) -> bool {
    let channel_lower = channel.to_lowercase();
    HISTORY_LOADED_AT
        .lock()
        .get(&channel_lower)
        .is_some_and(|loaded_at| {
            loaded_at.elapsed() < std::time::Duration::from_secs(HISTORY_RELOAD_SKIP_WINDOW_SECS)
        })
}

pub(super) static HISTORY_LOADED_CHANNELS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// True while `load_channel_history_async` owns the channel's first history
/// pass: it claimed the channel but has not recorded a completed load yet.
/// A failed pass releases the claim, so a later join fetches again.
pub(super) fn history_load_in_flight(channel: &str) -> bool {
    let channel_lower = channel.to_lowercase();
    HISTORY_LOADED_CHANNELS.lock().contains(&channel_lower)
        && !HISTORY_LOADED_AT.lock().contains_key(&channel_lower)
}

/// Spawn history loading for a channel (call after join_channel_immediately succeeds)
/// This is an async function that should be spawned on the runtime
pub async fn spawn_history_load(channel: String) {
    if crate::bench::ui_fixture::is_fixture_channel(&channel) {
        crate::log_irc!(
            debug,
            "Skipping history hydration for hermetic UI fixture channel {}",
            channel
        );
        return;
    }
    load_channel_history_async(&channel).await;
}

/// Recheck the current blocked membership at the final history commit, after
/// network/cache loading and message parsing have completed.
pub(super) fn track_visible_history_chatter(channel: &str, message: &ChatMessage) {
    if message.flags.is_system
        || message.flags.is_usernotice
        || message.sender.trim().is_empty()
        || message.sender.eq_ignore_ascii_case("system")
    {
        return;
    }
    let display_name = if message.display_name.trim().is_empty() {
        message.sender.as_ref()
    } else {
        message.display_name.as_ref()
    };
    track_chatter(
        channel,
        message.sender.as_ref(),
        display_name,
        message.color.as_deref().map(str::to_owned),
    );
}

pub(super) fn seed_chatters_from_visible_buffer(channel: &str) {
    let rows: Vec<_> = CHAT_MESSAGES
        .get(channel)
        .map(|buffer| {
            buffer
                .iter()
                .filter(|message| {
                    !message.flags.is_system
                        && !message.flags.is_usernotice
                        && !message.sender.trim().is_empty()
                        && !message.sender.eq_ignore_ascii_case("system")
                })
                .map(|message| {
                    (
                        message.sender.to_string(),
                        message.display_name.to_string(),
                        message.color.as_deref().map(str::to_owned),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (sender, display_name, color) in rows {
        let display_name = if display_name.trim().is_empty() {
            sender.as_str()
        } else {
            display_name.as_str()
        };
        track_chatter(channel, &sender, display_name, color);
    }
}

pub(super) async fn merge_visible_history_messages(
    channel: &str,
    messages: Vec<ChatMessage>,
) -> usize {
    if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        return 0;
    }
    let mut messages = messages;
    prepare_history_emote_scopes(&mut messages, channel).await;
    // Retokenize before taking the gate: every live PRIVMSG commit and the
    // UI's visible-row reads wait on it, and a large history batch takes
    // milliseconds to parse. A catalog that changed while parsing gets one
    // more pass under the gate so committed rows match the current catalog.
    let parsed_against = history_emote_versions(channel);
    for message in &mut messages {
        reparse_message_slots(message, channel, true);
    }
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if READ_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
        return 0;
    }
    if history_emote_versions(channel) != parsed_against {
        for message in &mut messages {
            reparse_message_slots(message, channel, true);
        }
    }
    let mut messages = crate::backend::history::apply_blocked_visibility(messages, channel);
    if is_channel_closed(channel) {
        crate::log_irc!(
            debug,
            "Dropping late history rows for closed channel {}",
            channel
        );
        return 0;
    }
    let defer_visibility = crate::backend::blocked_users::initial_visibility_pending();
    let mut inserted_ids = Vec::new();
    if !messages.is_empty() {
        let mut buffer = CHAT_MESSAGES
            .entry(channel.to_string())
            .or_insert_with(MessageBuffer::with_default_capacity);
        let existing_ids: HashSet<Arc<str>> =
            buffer.iter().map(|message| message.id.clone()).collect();
        let mut seen_ids = HashSet::new();
        messages.retain(|message| {
            !existing_ids.contains(&message.id) && seen_ids.insert(message.id.clone())
        });
        if defer_visibility {
            let remaining =
                crate::backend::blocked_users::pending_channel_capacity_remaining_commit_held(
                    &visibility_commit,
                );
            if remaining == 0 {
                pending_channel_row_capacity_available_commit_held(&visibility_commit);
                drop(buffer);
                drop(visibility_commit);
                return 0;
            }

            if messages.len() > remaining {
                let drop_count = messages.len() - remaining;
                messages.drain(..drop_count);
                log_pending_channel_overflow();
            }
            inserted_ids.extend(messages.iter().map(|message| message.id.clone()));
        }
        let admitted = messages.len();
        if !defer_visibility {
            for message in &messages {
                track_visible_history_chatter(channel, message);
            }
        }
        buffer.merge_history(messages);
        if defer_visibility {
            inserted_ids.retain(|id| buffer.contains_id(id.as_ref()));
        }
        drop(buffer);
        for message_id in inserted_ids {
            defer_buffered_row_visibility_commit_held(
                &visibility_commit,
                channel,
                message_id.as_ref(),
            );
        }
        drop(visibility_commit);
        return admitted;
    }
    drop(visibility_commit);
    0
}

pub(super) fn history_emote_versions(channel: &str) -> (u32, u32) {
    (
        crate::backend::emotes::get_global_emote_version(),
        crate::backend::emotes::get_emote_cache_version(channel),
    )
}

pub(super) async fn prepare_history_emote_scopes(
    messages: &mut [ChatMessage],
    fallback_channel: &str,
) {
    use futures_util::StreamExt;
    let _ = tokio::time::timeout(
        INITIAL_GLOBAL_EMOTE_READY_TIMEOUT,
        crate::backend::emotes::wait_for_initial_catalogs_ready(),
    )
    .await;
    let preparation = async {
        let mut unresolved_by_id: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, message) in messages.iter().enumerate() {
            if message.emote_lookup_channel.as_deref() == Some("")
                && let Some(source_id) = message.emote_lookup_source_id.as_deref()
            {
                unresolved_by_id
                    .entry(source_id.to_string())
                    .or_default()
                    .push(index);
            }
        }

        let mut resolutions = futures_util::stream::iter(unresolved_by_id)
            .map(|(source_id, indices)| async move {
                let source_login =
                    crate::backend::twitch_api::get_channel_login_by_id_cached(&source_id).await;
                (source_id, indices, source_login)
            })
            .buffer_unordered(4);
        while let Some((source_id, indices, source_login)) = resolutions.next().await {
            if let Some(source_login) = source_login {
                crate::backend::twitch_api::cache_channel_id(&source_login, &source_id);
                let source_login: Arc<str> = source_login.into();
                for index in indices {
                    messages[index].emote_lookup_channel = Some(Arc::clone(&source_login));
                    messages[index].emote_lookup_source_id = None;
                }
            }
        }

        let mut sources = HashSet::new();
        if !crate::backend::whispers::is_whispers_channel(fallback_channel) {
            sources.insert(fallback_channel.to_string());
        }
        for message in messages.iter() {
            if let Some(source) = message
                .emote_lookup_channel
                .as_deref()
                .filter(|source| !source.is_empty())
            {
                sources.insert(source.to_string());
            }
        }
        futures_util::stream::iter(sources)
            .map(|source| async move {
                ensure_channel_emotes_ready_for_history(&source, None).await
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
    };
    let _ = tokio::time::timeout(HISTORY_EMOTE_READY_TIMEOUT, preparation).await;
}

/// Publish a history snapshot under the same gate as membership replacement.
/// Retokenization stays outside the gate; this final prune/snapshot/observer
/// commit makes its result current without holding the gate during parsing.
pub(super) fn notify_visible_history_loaded(channel: &str) -> bool {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let blocked_rows_changed = apply_blocked_user_filter_to_channel(channel);
    if crate::backend::blocked_users::initial_visibility_pending() {
        return false;
    }
    let messages_for_observers =
        snapshot_channel_messages_for_observers(&visibility_commit, channel);
    drop(visibility_commit);
    crate::backend::observers::get_observer_registry().notify(
        channel,
        crate::backend::observers::ChannelEvent::HistoryLoaded(messages_for_observers),
    );
    if blocked_rows_changed {
        dispatch_blocked_channel_refreshes(HashSet::from([channel.to_lowercase()]));
    }
    true
}

/// Load channel history asynchronously (extracted for reuse)
/// Prevents duplicate loads from multiple splits showing the same channel
pub(super) async fn load_channel_history_async(channel: &str) {
    let channel_lower = channel.to_lowercase();

    {
        let mut loaded = HISTORY_LOADED_CHANNELS.lock();

        if loaded.contains(&channel_lower) {
            crate::log_irc!(
                debug,
                "History already loaded for {}, skipping duplicate load",
                channel
            );
            return;
        }

        loaded.insert(channel_lower.clone());
    }

    crate::backend::emote_candidates::mark_channel_loading(&channel_lower);

    // The disk merge can wait a long time on emote readiness (shared-chat rows
    // resolve their source channels over the network), so the network fetch
    // runs alongside it instead of after it. Join-time fetches skip while this
    // pass is in flight, so it must not be held up by the disk merge.
    let cached = async {
        if !crate::backend::settings::get_persist_history_enabled() {
            return;
        }
        let local = crate::backend::history_cache::load_channel_history(channel);
        if local.is_empty() {
            return;
        }
        let n = merge_visible_history_messages(channel, local).await;
        if n > 0 && notify_visible_history_loaded(channel) {
            bump_chat_version();
            crate::diagnostics::startup_metrics::record(
                crate::diagnostics::startup_metrics::StartupMarker::CachedHistoryCommitted,
            );
        }

        crate::log_irc!(
            info,
            "Eager-loaded {} cached history messages for {} (instant paint)",
            n,
            channel
        );
    };

    let network = async {
        match crate::backend::history::fetch_recent_messages_streaming(channel).await {
            Ok(result) => {
                if let Some(error) = result.cache_fallback_error.as_deref() {
                    add_history_error_message(
                        channel,
                        "Showing cached history because recent messages could not be loaded.",
                        error,
                    );
                }

                crate::log_irc!(
                    info,
                    "Loaded {} initial history messages for {} (immediate)",
                    result.messages.len(),
                    channel
                );

                merge_visible_history_messages(channel, result.messages).await;
                record_history_loaded_now(&channel_lower);

                if notify_visible_history_loaded(channel) {
                    bump_chat_version();
                }
            }
            Err(e) => {
                crate::log_irc!(
                    warn,
                    "Immediate history fetch failed for {}: {}",
                    channel,
                    e
                );
                add_history_error_message(channel, "Recent messages could not be loaded.", &e);

                {
                    let mut loaded = HISTORY_LOADED_CHANNELS.lock();
                    loaded.remove(&channel_lower);
                }
            }
        }
    };

    tokio::join!(cached, network);
}

/// Fill in missing messages for a channel after reconnection
/// Uses per-channel disconnect state to:
/// 1. Calculate how many messages to fetch based on disconnect duration
/// 2. Filter messages to only those after the last received message
/// 3. Insert at correct chronological position
///
/// Uses DashMap for per-channel access and MessageBuffer for O(N) merge
pub(super) async fn fill_in_missing_messages(channel: &str) -> Result<usize, String> {
    let channel_lower = channel.to_lowercase();

    let disconnect_state = match take_channel_disconnect_state(&channel_lower) {
        Some(state) => state,
        None => {
            crate::log_irc!(
                debug,
                "No disconnect state for {}, skipping gap-fill",
                channel_lower
            );
            return Ok(0);
        }
    };

    let now_ms = crate::utils::unix_now_ms();
    let disconnect_secs = (now_ms - disconnect_state.disconnect_time_ms) / 1000;
    let limit = crate::config::calculate_adaptive_history_limit(disconnect_secs);

    crate::log_irc!(
        info,
        "Gap-fill {}: disconnect was {}s ago, fetching up to {} messages (last_msg_ts: {})",
        channel_lower,
        disconnect_secs,
        limit,
        disconnect_state.last_message_time_ms
    );

    let fetched_msgs = match crate::backend::history::fetch_recent_messages_with_limit(
        &channel_lower,
        limit,
    )
    .await
    {
        Ok(msgs) => msgs,
        Err(error) => {
            restore_channel_disconnect_state(&channel_lower, disconnect_state);
            return Err(error);
        }
    };

    if fetched_msgs.is_empty() {
        return Ok(0);
    }

    let existing_ids: HashSet<Arc<str>> = CHAT_MESSAGES
        .get(&channel_lower)
        .map(|buffer| buffer.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default();

    let disconnect_ts = disconnect_state.last_message_time_ms;
    let gap_messages: Vec<ChatMessage> = fetched_msgs
        .into_iter()
        .filter(|msg| msg.timestamp > disconnect_ts && !existing_ids.contains(&msg.id))
        .collect();

    if gap_messages.is_empty() {
        crate::log_irc!(
            debug,
            "No new gap messages for {} (filtered by timestamp/existing)",
            channel_lower
        );
        return Ok(0);
    }

    let gap_messages: Vec<ChatMessage> = gap_messages
        .into_iter()
        .filter(|msg| mark_message_seen(&channel_lower, &msg.id))
        .collect();

    if gap_messages.is_empty() {
        crate::log_irc!(
            debug,
            "No new gap messages for {} (all duplicates in dedup cache)",
            channel_lower
        );
        return Ok(0);
    }

    let gap_count = merge_visible_history_messages(&channel_lower, gap_messages).await;
    if gap_count == 0 {
        crate::log_irc!(
            debug,
            "No visible gap messages remained for {} after final blocked-user recheck",
            channel_lower
        );
        return Ok(0);
    }

    if notify_visible_history_loaded(&channel_lower) {
        bump_chat_version();
    }

    crate::log_irc!(
        info,
        "Gap-fill: Inserted {} gap messages for channel {}",
        gap_count,
        channel_lower
    );

    Ok(gap_count)
}
