//! Re-tokenizing buffered rows when emote catalogs change, and buffer snapshots.

use super::*;

/// Reparse resident rows for one generation-fenced provider-visibility transaction.
/// Catalog publication itself never calls this: add/remove/rename events retain
/// parse-time bindings for already resident messages.
pub fn retokenize_all_channels_if_current(is_current: impl Fn() -> bool) {
    let channels: Vec<String> = CHAT_MESSAGES.iter().map(|r| r.key().clone()).collect();

    crate::log_emotes!(
        debug,
        "Provider visibility changed - re-tokenizing {} channels",
        channels.len()
    );
    for channel in channels {
        if !is_current() {
            return;
        }
        retokenize_channel_messages_if_current(&channel, &is_current);
    }
}

pub(super) fn snapshot_channel_messages_for_observers(
    commit: &crate::backend::blocked_users::VisibilityCommit,
    channel: &str,
) -> Arc<Vec<ChatMessage>> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Arc::new(Vec::new());
    }
    let pending_ids =
        crate::backend::blocked_users::pending_channel_row_ids_commit_held(commit, channel);
    CHAT_MESSAGES
        .get_mut(channel)
        .map(|mut buffer| {
            if pending_ids.is_empty() {
                buffer.snapshot()
            } else {
                Arc::new(
                    buffer
                        .iter()
                        .filter(|message| !pending_ids.contains(message.id.as_ref()))
                        .cloned()
                        .collect(),
                )
            }
        })
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

/// Return the externally visible backend snapshot without exposing rows held
/// for the initial blocked-user authority decision.
pub(crate) fn visible_channel_messages(channel: &str) -> Vec<ChatMessage> {
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel = channel.to_lowercase();
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(
        &visibility_commit,
        &channel,
    );
    let messages = CHAT_MESSAGES
        .get(&channel)
        .map(|buffer| {
            buffer
                .iter()
                .filter(|message| !pending_ids.contains(message.id.as_ref()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    drop(visibility_commit);
    messages
}

/// Catalog scopes currently referenced by resident rows. Shared-chat source
/// channels may not be IRC-desired hosts but still need visibility rebuilds.
pub fn active_emote_lookup_channels() -> HashSet<String> {
    let mut channels = HashSet::new();
    for buffer in CHAT_MESSAGES.iter() {
        for message in buffer.value().iter() {
            match message.emote_lookup_channel.as_deref() {
                Some("") => {}
                Some(source) => {
                    channels.insert(source.to_string());
                }
                None if !message.channel.is_empty() => {
                    channels.insert(message.channel.to_string());
                }
                None => {}
            }
        }
    }
    channels
}

/// Return only the newest externally visible rows without cloning the entire
/// bounded channel buffer. This keeps small bridge reads cheap while the
/// initial blocked-user transaction still filters backend-only rows.
pub(crate) fn visible_channel_messages_tail(channel: &str, limit: usize) -> Vec<ChatMessage> {
    if limit == 0 {
        return Vec::new();
    }
    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let channel = channel.to_lowercase();
    let pending_ids = crate::backend::blocked_users::pending_channel_row_ids_commit_held(
        &visibility_commit,
        &channel,
    );
    let mut messages = CHAT_MESSAGES
        .get(&channel)
        .map(|buffer| {
            buffer
                .iter_recent(buffer.len())
                .filter(|message| !pending_ids.contains(message.id.as_ref()))
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    messages.reverse();
    drop(visibility_commit);
    messages
}

pub(super) fn parse_twitch_emotes_for_message(
    msg: &ChatMessage,
) -> FxHashMap<usize, (String, usize)> {
    msg.twitch_emotes_tag
        .as_ref()
        .map(|tag| parse_emotes_tag_with_offset(tag, msg.reply_emote_offset))
        .unwrap_or_default()
}

pub(super) fn message_emote_lookup_channel<'a>(
    msg: &'a ChatMessage,
    fallback_channel: &'a str,
) -> &'a str {
    msg.emote_lookup_channel
        .as_deref()
        .unwrap_or(fallback_channel)
}

pub(super) fn publish_resolved_message_emote_scope(
    channel: &str,
    message_id: &str,
    source_channel: &str,
) {
    let channel = channel.to_lowercase();
    let message_id = message_id.to_string();
    let source_channel: Arc<str> = source_channel.to_lowercase().into();
    let mut pending = PENDING_MESSAGE_EMOTE_SCOPES.lock();
    let changed = update_buffered_message_emote_lookup_scope(
        &channel,
        &message_id,
        Arc::clone(&source_channel),
    );
    if changed {
        drop(pending);
        crate::backend::history_cache::schedule_visible_channel_history_persist(&channel);
        return;
    }
    if pending.len() >= MAX_PENDING_MESSAGE_EMOTE_SCOPES
        && let Some(stale_key) = pending.keys().next().cloned()
    {
        pending.remove(&stale_key);
    }
    pending.insert((channel, message_id), source_channel);
}

pub(super) fn apply_pending_message_emote_scope(channel: &str, message_id: &str) {
    let channel = channel.to_lowercase();
    let message_id = message_id.to_string();
    let mut pending = PENDING_MESSAGE_EMOTE_SCOPES.lock();
    if let Some(source_channel) = pending.remove(&(channel.clone(), message_id.clone())) {
        update_buffered_message_emote_lookup_scope(&channel, &message_id, source_channel);
    }
}

pub(super) fn update_buffered_message_emote_lookup_scope(
    channel: &str,
    message_id: &str,
    source_channel: Arc<str>,
) -> bool {
    CHAT_MESSAGES.get_mut(channel).is_some_and(|mut messages| {
        messages.update_by_id(message_id, |message| {
            message.emote_lookup_channel = Some(Arc::clone(&source_channel));
            message.emote_lookup_source_id = None;
        })
    })
}

pub(super) fn reparse_message_slots(
    msg: &mut ChatMessage,
    fallback_channel: &str,
    force_install: bool,
) -> bool {
    let twitch_emotes = parse_twitch_emotes_for_message(msg);
    let emote_lookup_channel = message_emote_lookup_channel(msg, fallback_channel);
    let sender = if msg.flags.is_system {
        crate::backend::emotes::EmoteSender::NONE
    } else {
        crate::backend::emotes::EmoteSender::new(msg.user_id.as_deref(), &msg.sender)
    };
    let new_slots = crate::backend::slots::parser::parse_to_slots_with_gifs(
        &msg.content,
        emote_lookup_channel,
        sender,
        &twitch_emotes,
        msg.twitch_gifs_tag.as_deref(),
        msg.reply_emote_offset,
    );
    let new_hash = compute_structure_hash(&new_slots);
    let semantic_changed = !slots_semantically_equal(&msg.slots, &new_slots);
    if !force_install && !semantic_changed {
        return false;
    }
    msg.slots = new_slots.into();
    msg.structure_hash = new_hash;
    semantic_changed
}

/// Reparse the resident rows `user_id` sent in `channels` after that user's
/// 7TV personal emote set loaded or changed.
pub fn retokenize_user_messages(user_id: &str, channels: &[Arc<str>]) {
    let mut any_updated = false;
    for channel in channels {
        let channel = channel.to_lowercase();
        let candidates: Vec<ChatMessage> = match CHAT_MESSAGES.get(&channel) {
            Some(buffer) => buffer
                .iter()
                .filter(|msg| !msg.flags.is_system && msg.user_id.as_deref() == Some(user_id))
                .cloned()
                .collect(),
            None => continue,
        };
        let updated: Vec<ChatMessage> = candidates
            .into_iter()
            .filter_map(|mut msg| reparse_message_slots(&mut msg, &channel, false).then_some(msg))
            .collect();
        if updated.is_empty() {
            continue;
        }
        if let Some(mut buffer) = CHAT_MESSAGES.get_mut(&channel) {
            for msg in updated {
                if let Some(index) = buffer.index_of(msg.id.as_ref()) {
                    buffer.update_at(index, msg);
                }
            }
        }
        any_updated = true;
        crate::backend::observers::get_observer_registry().notify(
            &channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }
    if any_updated {
        bump_chat_version();
    }
}

pub(super) fn retokenize_channel_messages_if_current(
    channel: &str,
    is_current: &impl Fn() -> bool,
) -> bool {
    let messages_to_process: Vec<(usize, ChatMessage)> = match CHAT_MESSAGES.get(channel) {
        Some(buffer) => buffer
            .iter()
            .enumerate()
            .filter(|(_, msg)| !msg.flags.is_system)
            .map(|(idx, msg)| (idx, msg.clone()))
            .collect(),
        None => {
            return false;
        }
    };

    let mut updated_messages: Vec<(usize, ChatMessage)> =
        Vec::with_capacity(messages_to_process.len() / 10);
    for (idx, mut msg) in messages_to_process {
        let old_hash = msg.structure_hash;
        let emote_lookup_channel = message_emote_lookup_channel(&msg, channel).to_string();
        if reparse_message_slots(&mut msg, channel, false) {
            if crate::log::emote_trace_enabled() {
                let preview = msg.content.chars().take(60).collect::<String>();
                crate::log_emotes!(
                    debug,
                    "[RETOKENIZE] Message '{}' structure changed (ch={}, emote_ch={}): old_hash={}, new_hash={}, slots: {:?}",
                    preview,
                    channel,
                    emote_lookup_channel,
                    old_hash,
                    msg.structure_hash,
                    msg.slots
                        .iter()
                        .map(|s| match s {
                            crate::backend::slots::MessageSlot::Text(t) => {
                                let text_preview = t.chars().take(15).collect::<String>();
                                format!("Text({})", text_preview)
                            }
                            crate::backend::slots::MessageSlot::Emote(e) =>
                                format!("Emote({})", e.name),
                            crate::backend::slots::MessageSlot::TwitchEmote { name, .. } =>
                                format!("TwitchEmote({})", name),
                            _ => "Other".to_string(),
                        })
                        .collect::<Vec<_>>()
                );
            }
            updated_messages.push((idx, msg));
        }
    }

    let updated_count = updated_messages.len();

    if !is_current() {
        return false;
    }

    if !updated_messages.is_empty()
        && is_current()
        && let Some(mut buffer) = CHAT_MESSAGES.get_mut(channel)
    {
        for (idx, updated_msg) in updated_messages {
            if !is_current() {
                return false;
            }
            if !buffer.update_at(idx, updated_msg.clone())
                && let Some(real_idx) = buffer.index_of(updated_msg.id.as_ref())
            {
                buffer.update_at(real_idx, updated_msg);
            }
        }
    }

    if !is_current() {
        return false;
    }

    if updated_count > 0 {
        crate::log_emotes!(
            info,
            "Re-tokenized {} messages for channel {}",
            updated_count,
            channel
        );

        crate::backend::observers::get_observer_registry().notify(
            channel,
            crate::backend::observers::ChannelEvent::MessagesRetokenized,
        );
    }

    bump_chat_version();
    updated_count > 0
}
