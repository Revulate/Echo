//! Bounded whisper conversations shared by Activity, inline chat, and the
//! virtual Whispers split.

use crate::backend::lock_utils::RwLockExt;
use crate::backend::models::{ChatMessage, HighlightType, MessageFlags};
use crate::backend::observers::ChannelEvent;
use crate::backend::types::WhisperThread;
use crate::state::MessageBuffer;
use rustc_hash::FxHashMap;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::{LazyLock, RwLock};
use uuid::Uuid;

pub const WHISPERS_CHANNEL: &str = "whispers";
pub(crate) const MAX_WHISPER_MESSAGES: usize = 5_000;

#[inline]
pub fn is_whispers_channel(channel: &str) -> bool {
    channel
        .trim()
        .trim_start_matches('#')
        .eq_ignore_ascii_case(WHISPERS_CHANNEL)
}

/// Channels to notify, each with the copy its own buffer stored: every buffer
/// assigns its own zebra parity, so one shared copy would stripe wrongly.
pub(crate) struct WhisperPublication {
    deliveries: Vec<(String, ChatMessage)>,
}

#[derive(Clone)]
pub(crate) struct ActivityWhisperThread {
    pub(crate) username: String,
    pub(crate) user_id: Option<String>,
    pub(crate) last_message: String,
    pub(crate) unread_count: u32,
    pub(crate) last_timestamp: i64,
    pub(crate) messages: Vec<ChatMessage>,
}

#[derive(Default)]
pub(crate) struct AccountWhisperClear {
    pub summaries_changed: bool,
    channels: Vec<String>,
}

#[derive(Clone, Debug)]
struct WhisperThreadState {
    username: String,
    user_id: Option<String>,
    last_message: String,
    unread_count: u32,
    last_timestamp: i64,
    messages: VecDeque<ChatMessage>,
    transient: bool,
}

#[derive(Default)]
struct WhisperState {
    threads: HashMap<String, WhisperThreadState>,
    last_received_sender: Option<String>,
}

static WHISPERS: LazyLock<RwLock<WhisperState>> =
    LazyLock::new(|| RwLock::new(WhisperState::default()));
const MAX_WHISPER_THREADS: usize = 50;
const MAX_THREAD_MESSAGES: usize = 100;

fn push_thread_message(thread: &mut WhisperThreadState, message: ChatMessage) {
    if let Some(index) = thread
        .messages
        .iter()
        .position(|existing| existing.id == message.id)
    {
        thread.messages.remove(index);
    }
    thread.messages.push_back(message);
    while thread.messages.len() > MAX_THREAD_MESSAGES {
        thread.messages.pop_front();
    }
}

fn new_thread(username: String) -> WhisperThreadState {
    WhisperThreadState {
        username,
        user_id: None,
        last_message: String::new(),
        unread_count: 0,
        last_timestamp: 0,
        messages: VecDeque::new(),
        transient: false,
    }
}

fn reserve_thread_slot(state: &mut WhisperState, incoming_key: &str) {
    if state.threads.contains_key(incoming_key) || state.threads.len() < MAX_WHISPER_THREADS {
        return;
    }
    let oldest_key = state
        .threads
        .iter()
        .min_by_key(|(_, thread)| thread.last_timestamp)
        .map(|(key, _)| key.clone());
    if let Some(oldest_key) = oldest_key
        && let Some(evicted) = state.threads.remove(&oldest_key)
        && state
            .last_received_sender
            .as_deref()
            .is_some_and(|sender| sender.eq_ignore_ascii_case(&evicted.username))
    {
        state.last_received_sender = None;
    }
}

/// Whisper previews and unread state are account-private. A full owner
/// transition clears them under the global visibility transaction; same-owner
/// token rotations deliberately retain them.
pub(crate) fn clear_account_state_visibility_commit_held() -> AccountWhisperClear {
    crate::backend::activity_persistence::retire_account_visibility_commit_held();

    let summaries_changed = {
        let mut state = WHISPERS.write_or_recover();
        let changed = !state.threads.is_empty() || state.last_received_sender.is_some();
        *state = WhisperState::default();
        changed
    };

    let mut channels = Vec::new();
    let buffer_channels: Vec<String> = crate::backend::irc::CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    for channel in buffer_channels {
        if is_whispers_channel(&channel) {
            if crate::backend::irc::CHAT_MESSAGES
                .remove(&channel)
                .is_some()
            {
                channels.push(channel);
            }
            continue;
        }
        let removed = crate::backend::irc::CHAT_MESSAGES
            .get_mut(&channel)
            .map(|mut buffer| buffer.retain(|message| !is_whispers_channel(&message.channel)))
            .unwrap_or(0);
        if removed > 0 {
            channels.push(channel);
        }
    }

    if summaries_changed {
        crate::backend::activity_persistence::mark_dirty();
    }

    AccountWhisperClear {
        summaries_changed,
        channels,
    }
}

pub(crate) fn snapshot_activity_visibility_commit_held()
-> (Vec<ActivityWhisperThread>, Option<String>) {
    let state = WHISPERS.read_or_recover();
    let mut threads: Vec<_> = state
        .threads
        .values()
        .filter(|thread| !thread.transient)
        .map(|thread| {
            let mut messages: Vec<_> = thread.messages.iter().cloned().collect();
            messages.sort_by(|left, right| {
                left.timestamp
                    .cmp(&right.timestamp)
                    .then_with(|| left.id.cmp(&right.id))
            });
            ActivityWhisperThread {
                username: thread.username.clone(),
                user_id: thread.user_id.clone(),
                last_message: thread.last_message.clone(),
                unread_count: thread.unread_count,
                last_timestamp: thread.last_timestamp,
                messages,
            }
        })
        .collect();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_timestamp));
    threads.truncate(MAX_WHISPER_THREADS);

    let last_received_sender = state.last_received_sender.clone().filter(|sender| {
        normalize_username(sender).is_some_and(|(key, _)| {
            state
                .threads
                .get(&key)
                .is_some_and(|thread| !thread.transient)
        })
    });
    (threads, last_received_sender)
}

fn activity_thread_is_blocked(
    thread: &ActivityWhisperThread,
    policy: Option<&crate::backend::blocked_users::BlockedVisibilityPolicy>,
) -> bool {
    policy.is_some_and(|policy| policy.contains(thread.user_id.as_deref(), &thread.username))
}

fn sorted_deduped_messages(
    messages: impl IntoIterator<Item = ChatMessage>,
) -> VecDeque<ChatMessage> {
    let mut unique = HashMap::new();
    for message in messages {
        unique.insert(message.id.to_string(), message);
    }
    let mut messages: Vec<_> = unique.into_values().collect();
    messages.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.id.cmp(&right.id))
    });
    if messages.len() > MAX_THREAD_MESSAGES {
        messages.drain(..messages.len() - MAX_THREAD_MESSAGES);
    }
    messages.into()
}

pub(crate) fn restore_activity_visibility_commit_held(
    mut threads: Vec<ActivityWhisperThread>,
    last_received_sender: Option<String>,
) {
    let policy = crate::backend::blocked_users::visibility_policy_snapshot();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_timestamp));
    let mut restored = HashMap::new();
    for thread in threads {
        let Some((key, username)) = normalize_username(&thread.username) else {
            continue;
        };
        if crate::backend::blocked_users::is_blocked(thread.user_id.as_deref(), &thread.username)
            || activity_thread_is_blocked(&thread, policy.as_ref())
        {
            continue;
        }
        let mut thread = thread;
        thread.username = username;
        thread.messages = sorted_deduped_messages(thread.messages.into_iter().filter(|message| {
            !crate::backend::blocked_users::is_blocked(message.user_id.as_deref(), &message.sender)
        }))
        .into_iter()
        .collect();
        restored
            .entry(key)
            .and_modify(|existing: &mut ActivityWhisperThread| {
                if thread.last_timestamp > existing.last_timestamp {
                    *existing = thread.clone();
                }
            })
            .or_insert(thread);
    }
    let mut restored: Vec<_> = restored.into_iter().collect();
    restored.sort_by_key(|(_, thread)| std::cmp::Reverse(thread.last_timestamp));
    restored.truncate(MAX_WHISPER_THREADS);

    let mut state = WHISPERS.write_or_recover();
    for (key, persisted) in restored {
        let persisted_state = WhisperThreadState {
            username: persisted.username,
            user_id: persisted.user_id,
            last_message: persisted.last_message,
            unread_count: persisted.unread_count,
            last_timestamp: persisted.last_timestamp,
            messages: sorted_deduped_messages(persisted.messages),
            transient: false,
        };
        let Some(live) = state.threads.get_mut(&key) else {
            if state.threads.len() < MAX_WHISPER_THREADS {
                state.threads.insert(key, persisted_state);
            } else if let Some((oldest_key, oldest_timestamp)) = state
                .threads
                .iter()
                .min_by_key(|(_, thread)| thread.last_timestamp)
                .map(|(key, thread)| (key.clone(), thread.last_timestamp))
                && persisted_state.last_timestamp > oldest_timestamp
            {
                let evicted = state.threads.remove(&oldest_key);
                if evicted.is_some_and(|thread| {
                    state
                        .last_received_sender
                        .as_deref()
                        .is_some_and(|sender| sender.eq_ignore_ascii_case(&thread.username))
                }) {
                    state.last_received_sender = None;
                }
                state.threads.insert(key, persisted_state);
            }
            continue;
        };

        let live_is_newer = live.last_timestamp >= persisted_state.last_timestamp;
        let unread_count = live.unread_count;
        let live_messages = std::mem::take(&mut live.messages);
        let persisted_messages = persisted_state.messages;
        live.messages =
            sorted_deduped_messages(persisted_messages.into_iter().chain(live_messages));
        if !live_is_newer {
            live.username = persisted_state.username;
            live.last_message = persisted_state.last_message;
            live.last_timestamp = persisted_state.last_timestamp;
        }
        if live.user_id.is_none() {
            live.user_id = persisted_state.user_id;
        }
        live.unread_count = unread_count;
    }

    if state.last_received_sender.is_none()
        && let Some(sender) = last_received_sender
        && let Some((key, sender)) = normalize_username(&sender)
        && state
            .threads
            .get(&key)
            .is_some_and(|thread| !thread.transient)
    {
        state.last_received_sender = Some(sender);
    }

    let mut messages = HashMap::new();
    for thread in state.threads.values().filter(|thread| !thread.transient) {
        for message in &thread.messages {
            messages.insert(message.id.to_string(), message.clone());
        }
    }
    let mut messages: Vec<_> = messages.into_values().collect();
    messages.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.id.cmp(&right.id))
    });
    drop(state);

    let mut buffer = MessageBuffer::new(MAX_WHISPER_MESSAGES);
    for mut message in messages {
        buffer.push_stamped(&mut message);
    }
    crate::backend::irc::CHAT_MESSAGES.insert(WHISPERS_CHANNEL.to_string(), buffer);
}

pub(crate) fn dispatch_account_clear(clear: AccountWhisperClear) {
    if !clear.channels.is_empty() {
        crate::backend::irc::bump_chat_version();
        let observers = crate::backend::observers::get_observer_registry();
        for channel in clear.channels {
            observers.notify(&channel, ChannelEvent::MessagesRetokenized);
        }
        crate::managers::frame_rate_controller::wakeup();
        crate::managers::frame_rate_controller::record_activity();
    }
    if clear.summaries_changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
    }
}

fn normalize_username(username: &str) -> Option<(String, String)> {
    let normalized = username.trim().trim_start_matches('@').trim();
    if normalized.is_empty() {
        return None;
    }

    Some((normalized.to_lowercase(), normalized.to_string()))
}

pub fn record_received(username: &str, message: &str, timestamp: i64) {
    if record_received_inner(username, message, timestamp) {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::backend::activity_persistence::mark_dirty();
    }
}

/// Commit a deferred whisper while the blocked-user visibility gate is held.
/// UI/sound/desktop effects are dispatched after the gate is released.
pub(crate) fn record_received_visibility_commit_held(message: ChatMessage) -> WhisperPublication {
    if record_received_message_inner(&message) {
        crate::backend::activity_persistence::mark_dirty();
    }
    publish_message_visibility_commit_held(message)
}

fn record_received_inner(username: &str, message: &str, timestamp: i64) -> bool {
    let Some((key, username)) = normalize_username(username) else {
        return false;
    };

    let mut state = WHISPERS.write_or_recover();
    reserve_thread_slot(&mut state, &key);
    let thread = state
        .threads
        .entry(key)
        .or_insert_with(|| new_thread(username.clone()));
    thread.username = username.clone();
    thread.last_message = message.to_string();
    thread.unread_count = thread.unread_count.saturating_add(1);
    thread.last_timestamp = timestamp;
    state.last_received_sender = Some(username);
    true
}

fn record_received_message_inner(message: &ChatMessage) -> bool {
    let Some((key, username)) = normalize_username(&message.display_name) else {
        return false;
    };

    let mut state = WHISPERS.write_or_recover();
    reserve_thread_slot(&mut state, &key);
    let thread = state
        .threads
        .entry(key)
        .or_insert_with(|| new_thread(username.clone()));
    thread.username = username.clone();
    if let Some(user_id) = message.user_id.as_deref() {
        thread.user_id = Some(user_id.to_string());
    }
    thread.last_message = message.content.to_string();
    thread.unread_count = thread.unread_count.saturating_add(1);
    thread.last_timestamp = message.timestamp;
    thread.transient = false;
    push_thread_message(thread, message.clone());
    state.last_received_sender = Some(username);
    true
}

pub fn record_sent(username: &str, user_id: &str, message: &str, timestamp: i64) {
    let Some((key, username)) = normalize_username(username) else {
        return;
    };
    let own_user_id = crate::backend::auth::active_authenticated_user_id();
    let own_login = crate::backend::settings::get_username().unwrap_or_default();
    let slots = crate::backend::slots::parse_to_slots(
        message,
        WHISPERS_CHANNEL,
        crate::backend::emotes::EmoteSender::new(own_user_id.as_deref(), &own_login),
        &FxHashMap::default(),
    );
    let structure_hash = crate::backend::slots::compute_structure_hash(&slots);
    let chat_message = ChatMessage {
        id: Arc::from(Uuid::new_v4().to_string()),
        channel: Arc::from(WHISPERS_CHANNEL),
        emote_lookup_channel: None,
        emote_lookup_source_id: None,
        sender: Arc::from(username.as_str()),
        display_name: Arc::from(format!("You → {username}")),
        content: Arc::from(message),
        color: None,
        badges: Default::default(),
        timestamp,
        slots: slots.into(),
        structure_hash,
        reply: None,
        flags: MessageFlags {
            highlight_type: Some(HighlightType::Whisper),
            ..MessageFlags::default()
        },
        twitch_gifs_tag: None,
        twitch_emotes_tag: None,
        user_id: (!user_id.is_empty()).then(|| Arc::from(user_id)),
        reply_emote_offset: 0,
    };

    let visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let mut state = WHISPERS.write_or_recover();
    reserve_thread_slot(&mut state, &key);
    let thread = state
        .threads
        .entry(key)
        .or_insert_with(|| new_thread(username.clone()));
    thread.username = username;
    if let Some(user_id) = chat_message.user_id.as_deref() {
        thread.user_id = Some(user_id.to_string());
    }
    thread.last_message = format!("You: {message}");
    thread.unread_count = 0;
    thread.last_timestamp = timestamp;
    thread.transient = false;
    push_thread_message(thread, chat_message.clone());
    drop(state);
    let publication = publish_message_visibility_commit_held(chat_message);
    drop(visibility_commit);
    dispatch_publication(publication);
    crate::state::ui_invalidation::request(
        crate::state::ui_invalidation::UiInvalidation::TopBarState,
    );
    crate::backend::activity_persistence::mark_dirty();
}

fn publish_message_visibility_commit_held(mut message: ChatMessage) -> WhisperPublication {
    let mut channels: Vec<String> = crate::backend::irc::CHAT_MESSAGES
        .iter()
        .map(|entry| entry.key().clone())
        .filter(|channel| !is_whispers_channel(channel))
        .collect();
    channels.sort_unstable();
    channels.dedup();

    let mut deliveries = Vec::with_capacity(channels.len() + 1);
    for channel in channels {
        if let Some(mut buffer) = crate::backend::irc::CHAT_MESSAGES.get_mut(&channel) {
            let mut copy = message.clone();
            buffer.push_stamped(&mut copy);
            deliveries.push((channel, copy));
        }
    }
    crate::backend::irc::CHAT_MESSAGES
        .entry(WHISPERS_CHANNEL.to_string())
        .or_insert_with(|| MessageBuffer::new(MAX_WHISPER_MESSAGES))
        .push_stamped(&mut message);
    deliveries.push((WHISPERS_CHANNEL.to_string(), message));

    WhisperPublication { deliveries }
}

pub(crate) fn dispatch_publication(publication: WhisperPublication) {
    crate::backend::irc::bump_chat_version();
    let observers = crate::backend::observers::get_observer_registry();
    for (channel, message) in publication.deliveries {
        observers.notify(&channel, ChannelEvent::NewMessage(message));
    }
    crate::managers::frame_rate_controller::wakeup();
    crate::managers::frame_rate_controller::record_activity();
}

pub fn last_received_sender() -> Option<String> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return None;
    }
    WHISPERS.read_or_recover().last_received_sender.clone()
}

pub fn mark_read(username: &str) -> bool {
    let Some((key, _username)) = normalize_username(username) else {
        return false;
    };

    let _visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let mut state = WHISPERS.write_or_recover();

    let Some(thread) = state.threads.get_mut(&key) else {
        return false;
    };

    let changed = thread.unread_count != 0;
    thread.unread_count = 0;
    drop(state);
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}

pub fn mark_all_read() -> bool {
    let _visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let mut state = WHISPERS.write_or_recover();

    let mut changed = false;
    for thread in state.threads.values_mut() {
        changed |= thread.unread_count != 0;
        thread.unread_count = 0;
    }
    drop(state);
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}

/// Remove all retained Activity whisper summaries after an explicit Clear all.
pub fn clear_threads() -> bool {
    let _visibility_commit = crate::backend::blocked_users::visibility_commit_guard();
    let mut state = WHISPERS.write_or_recover();
    let changed = !state.threads.is_empty() || state.last_received_sender.is_some();
    *state = WhisperState::default();
    drop(state);
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}

pub fn unread_count() -> u32 {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return 0;
    }
    let state = WHISPERS.read_or_recover();

    state.threads.values().fold(0u32, |count, thread| {
        count.saturating_add(thread.unread_count)
    })
}

pub fn get_threads() -> Vec<WhisperThread> {
    if crate::backend::blocked_users::resident_visibility_revoked() {
        return Vec::new();
    }
    let state = WHISPERS.read_or_recover();

    let mut threads: Vec<_> = state
        .threads
        .values()
        .map(|thread| WhisperThread {
            username: thread.username.clone(),
            last_message: thread.last_message.clone(),
            unread_count: thread.unread_count,
            last_timestamp: thread.last_timestamp,
        })
        .collect();
    threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_timestamp));
    threads
}

/// Insert one process-local Activity smoke fixture without applying production
/// eviction or notification policy. The caller owns the unique username and
/// must remove it with [`remove_activity_smoke_thread`].
pub(crate) fn insert_activity_smoke_thread(username: &str, message: &str, timestamp: i64) -> bool {
    let Some((key, username)) = normalize_username(username) else {
        return false;
    };

    let mut state = WHISPERS.write_or_recover();
    if state.threads.contains_key(&key) {
        return false;
    }

    state.threads.insert(
        key,
        WhisperThreadState {
            username,
            user_id: None,
            last_message: message.to_string(),
            unread_count: 1,
            last_timestamp: timestamp,
            messages: VecDeque::new(),
            transient: true,
        },
    );
    true
}

/// Remove exactly one Activity smoke fixture. Matching the timestamp as well
/// as the unique username prevents cleanup from deleting a subsequently
/// replaced thread if the driver outlives its intended run.
pub(crate) fn remove_activity_smoke_thread(username: &str, timestamp: i64) -> bool {
    let Some((key, _username)) = normalize_username(username) else {
        return false;
    };

    let mut state = WHISPERS.write_or_recover();
    let matches_fixture = state
        .threads
        .get(&key)
        .is_some_and(|thread| thread.last_timestamp == timestamp);
    if !matches_fixture {
        return false;
    }

    state.threads.remove(&key);
    true
}

/// Observe one Activity smoke thread and the unfiltered source size. Unlike
/// the user-facing getter, this reports the fixture store even while an
/// account-visibility transaction temporarily hides private rows.
pub(crate) fn activity_smoke_thread_snapshot(username: &str) -> (bool, u32, usize) {
    let Some((key, _username)) = normalize_username(username) else {
        return (false, 0, 0);
    };

    let state = WHISPERS.read_or_recover();
    let thread = state.threads.get(&key);
    (
        thread.is_some(),
        thread.map(|thread| thread.unread_count).unwrap_or(0),
        state.threads.len(),
    )
}

pub(crate) fn remove_blocked_users_with_policy(
    policy: &crate::backend::blocked_users::BlockedVisibilityPolicy,
) -> bool {
    if policy.is_empty() {
        return false;
    }
    let mut state = WHISPERS.write_or_recover();

    let before = state.threads.len();
    state
        .threads
        .retain(|login, thread| !policy.contains(thread.user_id.as_deref(), login));
    if state.last_received_sender.as_deref().is_some_and(|sender| {
        normalize_username(sender).is_none_or(|(key, _)| !state.threads.contains_key(&key))
    }) {
        state.last_received_sender = None;
    }

    let changed = state.threads.len() != before;
    drop(state);
    if changed {
        crate::state::ui_invalidation::request(
            crate::state::ui_invalidation::UiInvalidation::TopBarState,
        );
        crate::backend::activity_persistence::mark_dirty();
    }
    changed
}
